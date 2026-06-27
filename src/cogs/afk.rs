use super::Cog;
use crate::state::{AfkEntry, AppState};
use crate::utils::format::humanize_duration;
use async_trait::async_trait;
use chrono::Utc;
use serenity::all::{
    Colour, Context, CreateEmbed, CreateEmbedFooter, CreateMessage, Message, Timestamp,
};
use std::sync::Arc;
use std::time::Duration;

// Exact colors from the Python bot (gears/style.py).
const AQUA: Colour = Colour::from_rgb(0x7F, 0xDB, 0xFF); // 0x7FDBFF
const PINK: Colour = Colour::from_rgb(0xF0, 0x12, 0xBE); // 0xF012BE

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
        let rows: Vec<(i64, i64, String, i64)> =
            sqlx::query_as("SELECT guild_id, user_id, message, set_at FROM base_afk")
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
        if msg.author.bot {
            return;
        }
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let now = Utc::now().timestamp();

        // `afk [message]` command — set AFK (mirrors base.py afk_group → set_afk).
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if let Some(body) = content.strip_prefix(&prefix) {
            let body = body.trim();
            let mut it = body.splitn(2, ' ');
            if it.next() == Some("afk") {
                let message = it.next().unwrap_or("").trim().to_string();
                self.set_afk(ctx, msg, guild_id, message).await;
            }
        }

        // Run on every non-bot message (mirrors base.py on_message → manage_afk).
        self.manage_afk(ctx, msg, guild_id, now).await;
    }
}

impl AfkCog {
    async fn set_afk(&self, ctx: &Context, msg: &Message, guild_id: u64, message: String) {
        let user_id = msg.author.id.get();
        let set_at = Utc::now().timestamp();

        self.state.afk_cache.insert(
            (guild_id, user_id),
            AfkEntry {
                message: message.clone(),
                set_at,
            },
        );
        let _ = sqlx::query(
            "INSERT OR REPLACE INTO base_afk (guild_id, user_id, message, set_at) VALUES (?, ?, ?, ?)",
        )
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .bind(&message)
        .bind(set_at)
        .execute(self.state.servers_db())
        .await;

        let avatar = msg
            .author
            .avatar_url()
            .unwrap_or_else(|| msg.author.default_avatar_url());
        let embed = CreateEmbed::new()
            .title("Set AFK")
            .description(format!(">>> {message}"))
            .color(AQUA)
            .timestamp(Timestamp::now())
            .footer(
                CreateEmbedFooter::new("To remove this AFK send a message anywhere")
                    .icon_url(avatar),
            );
        let _ = msg
            .channel_id
            .send_message(
                &ctx.http,
                CreateMessage::new().embed(embed).reference_message(msg),
            )
            .await;
    }

    async fn manage_afk(&self, ctx: &Context, msg: &Message, guild_id: u64, now: i64) {
        let user_id = msg.author.id.get();

        // Author sent a message → clear their AFK if >3s have elapsed since it was set.
        let own = self
            .state
            .afk_cache
            .get(&(guild_id, user_id))
            .map(|e| e.clone());
        if let Some(entry) = own {
            if entry.set_at + 3 < now {
                self.state.afk_cache.remove(&(guild_id, user_id));
                let _ = sqlx::query("DELETE FROM base_afk WHERE guild_id = ? AND user_id = ?")
                    .bind(guild_id as i64)
                    .bind(user_id as i64)
                    .execute(self.state.servers_db())
                    .await;

                let dur =
                    humanize_duration(Duration::from_secs((now - entry.set_at).max(0) as u64));
                let embed = CreateEmbed::new()
                    .title("Removed AFK")
                    .description(format!(
                        "Welcome back <@{user_id}>!\n\nYou've been AFK for {dur}."
                    ))
                    .color(PINK)
                    .timestamp(Timestamp::now());
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embed).reference_message(msg),
                    )
                    .await;
            }
        }

        // Notify about any AFK users mentioned in this message (first 3, never self).
        for mentioned in msg.mentions.iter().take(3) {
            let mid = mentioned.id.get();
            if mid == user_id {
                continue;
            }
            let entry = self
                .state
                .afk_cache
                .get(&(guild_id, mid))
                .map(|e| e.clone());
            if let Some(entry) = entry {
                let dur =
                    humanize_duration(Duration::from_secs((now - entry.set_at).max(0) as u64));
                let embed = CreateEmbed::new()
                    .title(format!("{} is AFK", mentioned.name))
                    .description(entry.message.clone())
                    .color(PINK)
                    .timestamp(Timestamp::now())
                    .footer(CreateEmbedFooter::new(format!("Went AFK {dur} ago")));
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }
        }
    }
}
