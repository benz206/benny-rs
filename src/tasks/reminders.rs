use crate::state::AppState;
use serenity::http::Http;
use serenity::all::{CreateMessage, UserId};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info};

pub fn spawn_reminder_task(state: Arc<AppState>, http: Arc<Http>) {
    tokio::spawn(async move {
        info!("reminder task started");
        loop {
            let now = chrono::Utc::now().timestamp();
            let due: Vec<(i64, i64, String)> = sqlx::query_as(
                "SELECT id, user_id, content FROM reminders_reminders WHERE fire_at <= ?"
            )
            .bind(now)
            .fetch_all(state.users_db())
            .await
            .unwrap_or_default();

            for (id, user_id, content) in due {
                let uid = UserId::new(user_id as u64);
                match uid.create_dm_channel(&http).await {
                    Ok(channel) => {
                        let msg = CreateMessage::new().content(format!("⏰ **Reminder:** {content}"));
                        if let Err(e) = channel.send_message(&http, msg).await {
                            error!(error = ?e, user_id, "failed to send reminder DM");
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, user_id, "failed to create DM channel for reminder");
                    }
                }
                let _ = sqlx::query("DELETE FROM reminders_reminders WHERE id = ?")
                    .bind(id)
                    .execute(state.users_db())
                    .await;
            }

            sleep(Duration::from_secs(30)).await;
        }
    });
}
