use crate::cogs::reminders::sync_user_count;
use crate::state::AppState;
use crate::utils::colors;
use serenity::all::{CreateEmbed, CreateEmbedFooter, CreateMessage, Timestamp, UserId};
use serenity::http::Http;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use tracing::{error, info};

/// Poll every 30s for due reminders, DM the owner, delete the row, and keep the
/// per-user counters (SQLite + Redis) in sync.
pub fn spawn_reminder_task(state: Arc<AppState>, http: Arc<Http>) {
    tokio::spawn(async move {
        info!("reminder task started");
        loop {
            let now = chrono::Utc::now().timestamp();
            let due: Vec<(i64, i64, String)> = sqlx::query_as(
                "SELECT id, user_id, content FROM reminders_reminders WHERE fire_at <= ?",
            )
            .bind(now)
            .fetch_all(state.users_db())
            .await
            .unwrap_or_default();

            let mut affected: HashSet<i64> = HashSet::new();

            for (id, user_id, content) in due {
                let uid = UserId::new(user_id as u64);
                let embed = CreateEmbed::new()
                    .title("Reminder")
                    .description(format!("> {content}"))
                    .color(colors::BLUE)
                    .footer(CreateEmbedFooter::new(format!("Reminder ID: {id}")))
                    .timestamp(Timestamp::now());

                match uid.create_dm_channel(&http).await {
                    Ok(channel) => {
                        if let Err(e) = channel
                            .send_message(&http, CreateMessage::new().embed(embed))
                            .await
                        {
                            error!(error = ?e, user_id, "failed to send reminder DM");
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, user_id, "failed to create DM channel for reminder");
                    }
                }

                // Delete regardless of DM success so a blocked DM can't loop forever.
                let _ = sqlx::query("DELETE FROM reminders_reminders WHERE id = ?")
                    .bind(id)
                    .execute(state.users_db())
                    .await;
                affected.insert(user_id);
            }

            // Resync counters for every user whose reminder just fired.
            for user_id in affected {
                sync_user_count(&state, user_id).await;
            }

            sleep(Duration::from_secs(30)).await;
        }
    });
}
