use super::Cog;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use sqlx::SqlitePool;
use std::sync::Arc;

pub struct PrefixesCog {
    pool: SqlitePool,
    default_prefix: String,
}

impl PrefixesCog {
    pub fn new(pool: SqlitePool, default_prefix: String) -> Arc<Self> {
        Arc::new(Self { pool, default_prefix })
    }
}

#[async_trait]
impl Cog for PrefixesCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let content = msg.content.trim();
        if !content.starts_with(&self.default_prefix) { return; }
        let body = &content[self.default_prefix.len()..];
        let mut it = body.split_whitespace();
        let Some(cmd) = it.next() else { return };
        if cmd != "prefix" { return; }

        let guild_id = match msg.guild_id {
            Some(g) => g.get() as i64,
            None => {
                let _ = msg.channel_id.say(&ctx.http, "This command can only be used in a server.").await;
                return;
            }
        };

        match it.next() {
            Some("add") => {
                let Some(newp) = it.next() else {
                    let _ = msg.channel_id.say(&ctx.http, "Usage: prefix add <prefix>").await;
                    return;
                };
                let result = sqlx::query(
                    "INSERT OR IGNORE INTO settings_prefixes (guild_id, prefix) VALUES (?, ?)"
                )
                .bind(guild_id)
                .bind(newp)
                .execute(&self.pool)
                .await;

                match result {
                    Ok(r) if r.rows_affected() > 0 => {
                        let _ = msg.channel_id.say(&ctx.http, format!("Added prefix `{}`.", newp)).await;
                    }
                    Ok(_) => {
                        let _ = msg.channel_id.say(&ctx.http, format!("Prefix `{}` is already set.", newp)).await;
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to add prefix");
                        let _ = msg.channel_id.say(&ctx.http, "Database error.").await;
                    }
                }
            }
            Some("remove") => {
                let Some(p) = it.next() else {
                    let _ = msg.channel_id.say(&ctx.http, "Usage: prefix remove <prefix>").await;
                    return;
                };
                let result = sqlx::query(
                    "DELETE FROM settings_prefixes WHERE guild_id = ? AND prefix = ?"
                )
                .bind(guild_id)
                .bind(p)
                .execute(&self.pool)
                .await;

                match result {
                    Ok(r) if r.rows_affected() > 0 => {
                        let _ = msg.channel_id.say(&ctx.http, format!("Removed prefix `{}`.", p)).await;
                    }
                    Ok(_) => {
                        let _ = msg.channel_id.say(&ctx.http, format!("Prefix `{}` was not set.", p)).await;
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to remove prefix");
                        let _ = msg.channel_id.say(&ctx.http, "Database error.").await;
                    }
                }
            }
            Some("reset") => {
                let _ = sqlx::query("DELETE FROM settings_prefixes WHERE guild_id = ?")
                    .bind(guild_id)
                    .execute(&self.pool)
                    .await;
                let _ = msg.channel_id.say(
                    &ctx.http,
                    format!("Prefixes reset. Default prefix is `{}`.", self.default_prefix)
                ).await;
            }
            Some("list") => {
                let rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT prefix FROM settings_prefixes WHERE guild_id = ? ORDER BY prefix"
                )
                .bind(guild_id)
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();

                let prefixes: Vec<String> = rows.into_iter().map(|(p,)| format!("`{}`", p)).collect();
                let text = if prefixes.is_empty() {
                    format!("Default prefix: `{}`", self.default_prefix)
                } else {
                    format!("Prefixes: {}", prefixes.join(", "))
                };
                let _ = msg.channel_id.say(&ctx.http, text).await;
            }
            _ => {
                let _ = msg.channel_id.say(
                    &ctx.http,
                    "Usage: `prefix add <p>` | `prefix remove <p>` | `prefix reset` | `prefix list`"
                ).await;
            }
        }
    }
}
