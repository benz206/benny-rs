use super::Cog;
use crate::state::{AfkEntry, AppState};
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct AfkCog {
    state: Arc<AppState>,
}

impl AfkCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for AfkCog {
    async fn on_ready(&self, _ctx: &Context) {
        let rows: Vec<(i64, i64, String, i64)> = sqlx::query_as(
            "SELECT guild_id, user_id, message, set_at FROM base_afk"
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        for (guild_id, user_id, message, set_at) in rows {
            self.state.afk_cache.insert(
                (guild_id as u64, user_id as u64),
                AfkEntry { message, set_at },
            );
        }
        tracing::info!("AFK cache loaded ({} entries)", self.state.afk_cache.len());
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let user_id = msg.author.id.get();
        let now = chrono::Utc::now().timestamp();
        let prefix = self.state.prefix().to_string();

        // Handle !afk [reason] command
        let content = msg.content.trim();
        if content.starts_with(&prefix) {
            let body = content[prefix.len()..].trim();
            let mut it = body.split_whitespace();
            if it.next() == Some("afk") {
                let reason: String = it.collect::<Vec<_>>().join(" ");
                let set_at = now;
                let entry = AfkEntry { message: reason.clone(), set_at };
                self.state.afk_cache.insert((guild_id, user_id), entry);
                let _ = sqlx::query(
                    "INSERT OR REPLACE INTO base_afk (guild_id, user_id, message, set_at) VALUES (?, ?, ?, ?)"
                )
                .bind(guild_id as i64)
                .bind(user_id as i64)
                .bind(&reason)
                .bind(set_at)
                .execute(self.state.servers_db())
                .await;
                let afk_msg = if reason.is_empty() {
                    format!("{} is now AFK.", msg.author.name)
                } else {
                    format!("{} is now AFK: {reason}", msg.author.name)
                };
                let _ = msg.channel_id.say(&ctx.http, afk_msg).await;
                return;
            }
        }

        // Check if the author was AFK — if so, clear it (if >3s since they set it)
        if let Some(entry) = self.state.afk_cache.get(&(guild_id, user_id)) {
            if now - entry.set_at > 3 {
                drop(entry);
                self.state.afk_cache.remove(&(guild_id, user_id));
                let _ = sqlx::query("DELETE FROM base_afk WHERE guild_id = ? AND user_id = ?")
                    .bind(guild_id as i64)
                    .bind(user_id as i64)
                    .execute(self.state.servers_db())
                    .await;
                let _ = msg.channel_id.say(
                    &ctx.http,
                    format!("Welcome back, {}!", msg.author.name)
                ).await;
                return;
            }
        }

        // Skip AFK mention checks for command messages
        if content.starts_with(&prefix) { return; }

        // Notify about AFK mentioned users
        for mentioned_user in &msg.mentions {
            let mid = mentioned_user.id.get();
            if let Some(entry) = self.state.afk_cache.get(&(guild_id, mid)) {
                let since = now - entry.set_at;
                let since_str = if since < 60 {
                    format!("{since}s ago")
                } else if since < 3600 {
                    format!("{}m ago", since / 60)
                } else {
                    format!("{}h ago", since / 3600)
                };
                let reason = if entry.message.is_empty() {
                    "No reason given.".to_string()
                } else {
                    entry.message.clone()
                };
                let _ = msg.channel_id.say(
                    &ctx.http,
                    format!("**{}** is AFK ({since_str}): {reason}", mentioned_user.name)
                ).await;
            }
        }
    }
}
