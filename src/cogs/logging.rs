use super::Cog;
use crate::state::{AppState, LoggingConfig};
use async_trait::async_trait;
use serenity::all::{ChannelId, Context, GuildId, Member, Message, MessageId, User};
use serenity::model::event::MessageUpdateEvent;
use std::sync::Arc;
use tracing::error;

pub struct LoggingCog {
    state: Arc<AppState>,
}

impl LoggingCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    async fn send_log(&self, guild_id: u64, payload: serde_json::Value) {
        let config = match self.state.logging_cache.get(&guild_id) {
            Some(c) if c.enabled && !c.webhook_url.is_empty() => c.clone(),
            _ => return,
        };

        if let Err(e) = self
            .state
            .http
            .post(&config.webhook_url)
            .json(&payload)
            .send()
            .await
        {
            error!(error = ?e, guild_id, "failed to send log webhook");
        }
    }
}

#[async_trait]
impl Cog for LoggingCog {
    async fn on_ready(&self, _ctx: &Context) {
        let rows: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT guild_id, webhook_url, enabled FROM logging_webhooks",
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        for (guild_id, webhook_url, enabled) in rows {
            self.state.logging_cache.insert(
                guild_id as u64,
                LoggingConfig {
                    webhook_url,
                    enabled: enabled != 0,
                },
            );
        }
        tracing::info!("Logging configs loaded");
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        let mut it = body.splitn(2, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "logging" {
            return;
        }

        let subcmd = it.next().unwrap_or("").trim();
        let mut parts = subcmd.splitn(2, ' ');
        let action = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();

        match action {
            "setup" => {
                let webhook_url = arg;
                if webhook_url.is_empty() || !webhook_url.starts_with("https://") {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: logging setup <webhook_url>")
                        .await;
                    return;
                }
                let _ = sqlx::query(
                    "INSERT INTO logging_webhooks (guild_id, webhook_url, enabled) VALUES (?, ?, 1) \
                     ON CONFLICT(guild_id) DO UPDATE SET webhook_url = excluded.webhook_url, enabled = 1",
                )
                .bind(guild_id as i64)
                .bind(webhook_url)
                .execute(self.state.servers_db())
                .await;
                self.state.logging_cache.insert(
                    guild_id,
                    LoggingConfig {
                        webhook_url: webhook_url.to_string(),
                        enabled: true,
                    },
                );
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Logging webhook configured and enabled.")
                    .await;
            }
            "disable" => {
                let _ = sqlx::query(
                    "UPDATE logging_webhooks SET enabled = 0 WHERE guild_id = ?",
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                if let Some(mut e) = self.state.logging_cache.get_mut(&guild_id) {
                    e.enabled = false;
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Logging disabled.")
                    .await;
            }
            "test" => {
                let payload = serde_json::json!({
                    "content": "Logging test — this webhook is working!"
                });
                self.send_log(guild_id, payload).await;
                let _ = msg.channel_id.say(&ctx.http, "Test log sent.").await;
            }
            _ => {
                let _ = msg.channel_id.say(
                    &ctx.http,
                    "Usage: `logging setup <webhook_url>` | `logging disable` | `logging test`",
                ).await;
            }
        }
    }

    async fn on_message_update(
        &self,
        _ctx: &Context,
        old: Option<Message>,
        new: Option<Message>,
        _event: &MessageUpdateEvent,
    ) {
        let new_msg = match &new {
            Some(m) => m,
            None => return,
        };
        if new_msg.author.bot {
            return;
        }
        let guild_id = match new_msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };

        let old_content = old
            .as_ref()
            .map(|m| m.content.as_str())
            .unwrap_or("(unknown)");
        let new_content = &new_msg.content;
        if old_content == new_content {
            return;
        }

        let payload = serde_json::json!({
            "embeds": [{
                "title": "Message Edited",
                "color": 0xfee75c_u32,
                "fields": [
                    { "name": "Author", "value": format!("<@{}>", new_msg.author.id.get()), "inline": true },
                    { "name": "Channel", "value": format!("<#{}>", new_msg.channel_id.get()), "inline": true },
                    { "name": "Before", "value": old_content, "inline": false },
                    { "name": "After", "value": new_content.as_str(), "inline": false },
                ]
            }]
        });
        self.send_log(guild_id, payload).await;
    }

    async fn on_message_delete(
        &self,
        _ctx: &Context,
        channel_id: ChannelId,
        _msg_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        let guild_id = match guild_id {
            Some(g) => g.get(),
            None => return,
        };

        let payload = serde_json::json!({
            "embeds": [{
                "title": "Message Deleted",
                "color": 0xed4245_u32,
                "fields": [
                    { "name": "Channel", "value": format!("<#{}>", channel_id.get()), "inline": true },
                ]
            }]
        });
        self.send_log(guild_id, payload).await;
    }

    async fn on_member_join(&self, _ctx: &Context, member: &Member) {
        let guild_id = member.guild_id.get();
        let created_at = member.user.id.created_at();
        let created_timestamp = created_at.unix_timestamp();
        let now = chrono::Utc::now().timestamp();
        let account_age_days = (now - created_timestamp) / 86400;

        let payload = serde_json::json!({
            "embeds": [{
                "title": "Member Joined",
                "color": 0x57f287_u32,
                "fields": [
                    { "name": "User", "value": format!("<@{}>", member.user.id.get()), "inline": true },
                    { "name": "Account Age", "value": format!("{account_age_days} days"), "inline": true },
                ]
            }]
        });
        self.send_log(guild_id, payload).await;
    }

    async fn on_member_leave(&self, _ctx: &Context, guild_id: GuildId, user: &User) {
        let discriminator_str = user
            .discriminator
            .map(|d| format!("#{d}"))
            .unwrap_or_default();
        let payload = serde_json::json!({
            "embeds": [{
                "title": "Member Left",
                "color": 0xed4245_u32,
                "fields": [
                    { "name": "User", "value": format!("{}{}", user.name, discriminator_str), "inline": true },
                ]
            }]
        });
        self.send_log(guild_id.get(), payload).await;
    }
}
