use super::Cog;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use sqlx::SqlitePool;
use std::sync::Arc;

pub struct PrefixesCog {
    pool: SqlitePool,
    prefix: String,
}

impl PrefixesCog {
    pub fn new(pool: SqlitePool, prefix: String) -> Arc<Self> { Arc::new(Self { pool, prefix }) }
}

#[async_trait]
impl Cog for PrefixesCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let content = msg.content.trim();
        if !content.starts_with(&self.prefix) { return; }
        let body = &content[self.prefix.len()..];
        let mut it = body.split_whitespace();
        let Some(cmd) = it.next() else { return };
        if cmd != "prefix" { return; }

        match it.next() {
            Some("add") => {
                if let Some(newp) = it.next() {
                    let guild_id = msg.guild_id.map(|g| g.get().to_string()).unwrap_or_default();
                    if !guild_id.is_empty() {
                        let _ = sqlx::query("INSERT INTO settings_prefixes(guild_id, prefixes) VALUES(?, ?) ON CONFLICT(guild_id) DO UPDATE SET prefixes = prefixes || ',' || excluded.prefixes")
                            .bind(guild_id)
                            .bind(newp)
                            .execute(&self.pool).await;
                        let _ = msg.channel_id.say(&ctx.http, format!("Added prefix `{}`", newp)).await;
                    }
                }
            }
            Some("list") => {
                let guild_id = msg.guild_id.map(|g| g.get().to_string()).unwrap_or_default();
                if !guild_id.is_empty() {
                    let row: Option<(String,)> = sqlx::query_as("SELECT prefixes FROM settings_prefixes WHERE guild_id = ?")
                        .bind(guild_id)
                        .fetch_optional(&self.pool).await.ok().flatten();
                    let text = row.map(|(p,)| p).unwrap_or_else(|| self.prefix.clone());
                    let _ = msg.channel_id.say(&ctx.http, format!("Prefixes: {}", text)).await;
                }
            }
            _ => { let _ = msg.channel_id.say(&ctx.http, "Usage: prefix add <p> | prefix list").await; }
        }
    }
}


