use super::Cog;
use crate::state::AppState;
use crate::utils::parse::parse_duration;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct RemindersCog {
    state: Arc<AppState>,
}

impl RemindersCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for RemindersCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) { return; }
        let body = content[prefix.len()..].trim();
        let mut it = body.split_whitespace();
        let Some(cmd) = it.next() else { return };

        match cmd {
            "remind" | "reminder" => {
                self.handle_remind(ctx, msg, it.collect::<Vec<_>>().join(" ")).await;
            }
            "reminders" => {
                match it.next() {
                    Some("list") => self.handle_list(ctx, msg).await,
                    Some("delete") | Some("del") => {
                        if let Some(id_str) = it.next() {
                            self.handle_delete(ctx, msg, id_str).await;
                        } else {
                            let _ = msg.channel_id.say(&ctx.http, "Usage: reminders delete <id>").await;
                        }
                    }
                    _ => {
                        let _ = msg.channel_id.say(&ctx.http, "Usage: `reminders list` | `reminders delete <id>`").await;
                    }
                }
            }
            _ => {}
        }
    }
}

impl RemindersCog {
    async fn handle_remind(&self, ctx: &Context, msg: &Message, args: String) {
        let mut parts = args.splitn(2, ' ');
        let duration_str = match parts.next() {
            Some(s) if !s.is_empty() => s,
            _ => {
                let _ = msg.channel_id.say(&ctx.http, "Usage: remind <duration> <message>\nExample: remind 10m Take a break").await;
                return;
            }
        };
        let reminder_content = parts.next().unwrap_or("").trim();
        if reminder_content.is_empty() {
            let _ = msg.channel_id.say(&ctx.http, "Please provide a reminder message.").await;
            return;
        }

        let duration = match parse_duration(duration_str) {
            Some(d) => d,
            None => {
                let _ = msg.channel_id.say(&ctx.http, format!("Invalid duration: `{duration_str}`. Try `10m`, `1h`, `2h30m`.")).await;
                return;
            }
        };

        let user_id = msg.author.id.get() as i64;

        // Check reminder count limit
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM reminders_reminders WHERE user_id = ?"
        )
        .bind(user_id)
        .fetch_one(self.state.users_db())
        .await
        .unwrap_or(0);

        if count >= 10 {
            let _ = msg.channel_id.say(&ctx.http, "You already have 10 reminders. Delete some with `reminders delete <id>`.").await;
            return;
        }

        let fire_at = chrono::Utc::now().timestamp() + duration.as_secs() as i64;
        let result = sqlx::query(
            "INSERT INTO reminders_reminders (user_id, content, fire_at) VALUES (?, ?, ?)"
        )
        .bind(user_id)
        .bind(reminder_content)
        .bind(fire_at)
        .execute(self.state.users_db())
        .await;

        match result {
            Ok(r) => {
                let id = r.last_insert_rowid();
                let secs = duration.as_secs();
                let when_str = if secs < 60 {
                    format!("{secs}s")
                } else if secs < 3600 {
                    format!("{}m", secs / 60)
                } else {
                    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
                };
                let _ = msg.channel_id.say(
                    &ctx.http,
                    format!("✅ Reminder #{id} set for **{when_str}** from now. I'll DM you!")
                ).await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to create reminder");
                let _ = msg.channel_id.say(&ctx.http, "Failed to create reminder.").await;
            }
        }
    }

    async fn handle_list(&self, ctx: &Context, msg: &Message) {
        let user_id = msg.author.id.get() as i64;
        let rows: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT id, content, fire_at FROM reminders_reminders WHERE user_id = ? ORDER BY fire_at"
        )
        .bind(user_id)
        .fetch_all(self.state.users_db())
        .await
        .unwrap_or_default();

        if rows.is_empty() {
            let _ = msg.channel_id.say(&ctx.http, "You have no reminders.").await;
            return;
        }

        let now = chrono::Utc::now().timestamp();
        let mut lines = vec!["**Your Reminders:**".to_string()];
        for (id, content, fire_at) in rows {
            let remaining = fire_at - now;
            let when_str = if remaining <= 0 {
                "soon".to_string()
            } else if remaining < 60 {
                format!("{remaining}s")
            } else if remaining < 3600 {
                format!("{}m", remaining / 60)
            } else {
                format!("{}h {}m", remaining / 3600, (remaining % 3600) / 60)
            };
            lines.push(format!("**#{id}** (in {when_str}): {content}"));
        }
        let _ = msg.channel_id.say(&ctx.http, lines.join("\n")).await;
    }

    async fn handle_delete(&self, ctx: &Context, msg: &Message, id_str: &str) {
        let id: i64 = match id_str.parse() {
            Ok(n) => n,
            Err(_) => {
                let _ = msg.channel_id.say(&ctx.http, "Invalid reminder ID.").await;
                return;
            }
        };
        let user_id = msg.author.id.get() as i64;
        let result = sqlx::query(
            "DELETE FROM reminders_reminders WHERE id = ? AND user_id = ?"
        )
        .bind(id)
        .bind(user_id)
        .execute(self.state.users_db())
        .await;

        match result {
            Ok(r) if r.rows_affected() > 0 => {
                let _ = msg.channel_id.say(&ctx.http, format!("✅ Deleted reminder #{id}.")).await;
            }
            Ok(_) => {
                let _ = msg.channel_id.say(&ctx.http, "Reminder not found or not yours.").await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to delete reminder");
                let _ = msg.channel_id.say(&ctx.http, "Failed to delete reminder.").await;
            }
        }
    }
}
