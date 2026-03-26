use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct SettingsCog {
    state: Arc<AppState>,
}

impl SettingsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for SettingsCog {
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
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };

        match cmd {
            "settings" => {
                let subcmd = it.next().unwrap_or("");
                match subcmd {
                    "show" => self.cmd_show(ctx, msg, guild_id).await,
                    "reset" => self.cmd_reset(ctx, msg, guild_id).await,
                    _ => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Usage: `settings show` | `settings reset`",
                            )
                            .await;
                    }
                }
            }
            "blacklist" => {
                let subcmd = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                self.cmd_blacklist(ctx, msg, subcmd, arg).await;
            }
            _ => {}
        }
    }
}

impl SettingsCog {
    async fn cmd_show(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let prefixes: Vec<(String,)> = sqlx::query_as(
            "SELECT prefix FROM settings_prefixes WHERE guild_id = ? ORDER BY prefix",
        )
        .bind(guild_id as i64)
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        let prefix_str = if prefixes.is_empty() {
            format!("`{}`", self.state.prefix())
        } else {
            prefixes
                .iter()
                .map(|(p,)| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let welcome_str = match self.state.welcome_cache.get(&guild_id) {
            Some(c) if c.enabled => c
                .channel_id
                .map(|id| format!("Enabled in <#{id}>"))
                .unwrap_or_else(|| "Enabled (no channel set)".to_string()),
            Some(_) => "Disabled".to_string(),
            None => "Not configured".to_string(),
        };

        let logging_str = match self.state.logging_cache.get(&guild_id) {
            Some(c) if c.enabled => "Enabled".to_string(),
            Some(_) => "Disabled".to_string(),
            None => "Not configured".to_string(),
        };

        let sentinel_str = match self.state.sentinel_cache.get(&guild_id) {
            Some(c) if c.enabled => "Enabled".to_string(),
            Some(_) => "Disabled".to_string(),
            None => "Not configured".to_string(),
        };

        let text = format!(
            "**Server Settings**\n\
            **Prefixes:** {prefix_str}\n\
            **Welcome:** {welcome_str}\n\
            **Logging:** {logging_str}\n\
            **Sentinel:** {sentinel_str}"
        );
        let _ = msg.channel_id.say(&ctx.http, text).await;
    }

    async fn cmd_reset(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let gid = guild_id as i64;
        let _ = sqlx::query("DELETE FROM settings_prefixes WHERE guild_id = ?")
            .bind(gid)
            .execute(self.state.servers_db())
            .await;
        let _ = sqlx::query("DELETE FROM welcome_config WHERE guild_id = ?")
            .bind(gid)
            .execute(self.state.servers_db())
            .await;
        let _ = sqlx::query("DELETE FROM goodbye_config WHERE guild_id = ?")
            .bind(gid)
            .execute(self.state.servers_db())
            .await;
        let _ = sqlx::query("DELETE FROM logging_webhooks WHERE guild_id = ?")
            .bind(gid)
            .execute(self.state.servers_db())
            .await;
        let _ = sqlx::query("DELETE FROM sentinels_config WHERE guild_id = ?")
            .bind(gid)
            .execute(self.state.servers_db())
            .await;

        self.state.prefix_cache.remove(&guild_id);
        self.state.welcome_cache.remove(&guild_id);
        self.state.goodbye_cache.remove(&guild_id);
        self.state.logging_cache.remove(&guild_id);
        self.state.sentinel_cache.remove(&guild_id);

        let _ = msg
            .channel_id
            .say(&ctx.http, "All settings reset to defaults.")
            .await;
    }

    async fn cmd_blacklist(
        &self,
        ctx: &Context,
        msg: &Message,
        subcmd: &str,
        arg: &str,
    ) {
        let user_id: Option<u64> = if arg.starts_with("<@") && arg.ends_with('>') {
            arg[2..arg.len() - 1]
                .trim_start_matches('!')
                .parse()
                .ok()
        } else {
            arg.parse().ok()
        };

        let user_id = match user_id {
            Some(id) => id as i64,
            None => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `blacklist add <@user>` | `blacklist remove <@user>`",
                    )
                    .await;
                return;
            }
        };

        match subcmd {
            "add" => {
                let _ = sqlx::query(
                    "INSERT INTO settings_users (user_id, is_blacklisted) VALUES (?, 1) \
                     ON CONFLICT(user_id) DO UPDATE SET is_blacklisted = 1",
                )
                .bind(user_id)
                .execute(self.state.users_db())
                .await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("<@{user_id}> added to blacklist."))
                    .await;
            }
            "remove" => {
                let _ = sqlx::query(
                    "UPDATE settings_users SET is_blacklisted = 0 WHERE user_id = ?",
                )
                .bind(user_id)
                .execute(self.state.users_db())
                .await;
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!("<@{user_id}> removed from blacklist."),
                    )
                    .await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `blacklist add <@user>` | `blacklist remove <@user>`",
                    )
                    .await;
            }
        }
    }
}
