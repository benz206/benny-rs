use super::Cog;
use crate::state::AppState;
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use serenity::all::{Context, CreateEmbed, CreateMessage, Guild, Message, Timestamp, UnavailableGuild};
use std::sync::Arc;

/// Maximum number of custom prefixes a guild may have.
///
/// DESIGN.md (7.9) mandates 5 for the Rust rewrite. The original Python
/// `settings.py` allowed 15; we follow the Rust spec.
const MAX_PREFIXES: usize = 5;
/// Maximum length of a single prefix (Python `sanitize_prefix` truncated to 25;
/// here we reject anything longer, per the porting task).
const MAX_PREFIX_LEN: usize = 25;
/// Legacy `:|:` separator the Python implementation used to join prefixes in a
/// single column. Disallowed as a prefix so it can never corrupt storage.
const LEGACY_SEP: &str = ":|:";

pub struct PrefixesCog {
    state: Arc<AppState>,
}

impl PrefixesCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    /// Sanitize a prefix the way Python's `sanitize_prefix` did (strip ends,
    /// reject the `:|:` separator) plus the limits the task requires (non-empty,
    /// max length). Inner spaces are allowed.
    fn sanitize_prefix(raw: &str) -> Result<String, String> {
        if raw.contains(LEGACY_SEP) {
            return Err("Why do you have `:|:` as a prefix...".to_string());
        }
        let clean = raw.trim();
        if clean.is_empty() {
            return Err("You cannot have an empty prefix".to_string());
        }
        if clean.chars().count() > MAX_PREFIX_LEN {
            return Err(format!(
                "Prefixes can be at most {MAX_PREFIX_LEN} characters long"
            ));
        }
        Ok(clean.to_string())
    }

    /// Current prefixes for a guild: cache first, falling back to the DB. An
    /// empty result means the guild has no custom prefixes (the default still
    /// works via the bot's configured prefix / mention).
    async fn current_prefixes(&self, guild_id: u64) -> Vec<String> {
        if let Some(entry) = self.state.prefix_cache.get(&guild_id) {
            return entry.clone();
        }
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT prefix FROM settings_prefixes WHERE guild_id = ?",
        )
        .bind(guild_id as i64)
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();
        let mut prefixes: Vec<String> = rows.into_iter().map(|(p,)| p).collect();
        prefixes.sort_by_key(|p| p.len());
        prefixes
    }
}

#[async_trait]
impl Cog for PrefixesCog {
    /// Hydrate `prefix_cache` from the DB on startup.
    async fn on_ready(&self, _ctx: &Context) {
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT guild_id, prefix FROM settings_prefixes")
                .fetch_all(self.state.servers_db())
                .await
                .unwrap_or_default();

        let mut count = 0usize;
        for (guild_id, prefix) in rows {
            self.state
                .prefix_cache
                .entry(guild_id as u64)
                .or_default()
                .push(prefix);
            count += 1;
        }
        // Keep each guild's prefixes sorted by length (matches Python).
        for mut entry in self.state.prefix_cache.iter_mut() {
            entry.value_mut().sort_by_key(|p| p.len());
        }
        tracing::info!("Prefix cache loaded ({count} prefixes)");
    }

    /// On joining a guild (or initial guild sync), ensure a default prefix row
    /// and cache entry exist. Idempotent: skips guilds already cached.
    async fn on_guild_create(&self, _ctx: &Context, guild: &Guild) {
        let guild_id = guild.id.get();
        if self.state.prefix_cache.contains_key(&guild_id) {
            return;
        }

        let existing = self.current_prefixes(guild_id).await;
        if existing.is_empty() {
            let default = self.state.prefix().to_string();
            let _ = sqlx::query(
                "INSERT OR IGNORE INTO settings_prefixes (guild_id, prefix) VALUES (?, ?)",
            )
            .bind(guild_id as i64)
            .bind(&default)
            .execute(self.state.servers_db())
            .await;
            self.state.prefix_cache.insert(guild_id, vec![default]);
        } else {
            self.state.prefix_cache.insert(guild_id, existing);
        }
    }

    /// On leaving a guild, remove its prefixes from the DB and cache.
    async fn on_guild_delete(&self, _ctx: &Context, incomplete: UnavailableGuild, _full: Option<Guild>) {
        let guild_id = incomplete.id.get();
        let _ = sqlx::query("DELETE FROM settings_prefixes WHERE guild_id = ?")
            .bind(guild_id as i64)
            .execute(self.state.servers_db())
            .await;
        self.state.prefix_cache.remove(&guild_id);
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        // splitn(3): keep the remainder intact so multi-word prefixes work
        // (Python used a greedy `*, prefix: str`).
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "prefix" {
            return;
        }

        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "This command can only be used in a server.")
                    .await;
                return;
            }
        };

        let sub = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("").trim();

        match sub {
            "add" | "create" | "+" => self.cmd_add(ctx, msg, guild_id, rest).await,
            "remove" | "del" | "rm" | "delete" | "-" => {
                self.cmd_remove(ctx, msg, guild_id, rest).await
            }
            "list" | "view" | "config" => self.cmd_list(ctx, msg, guild_id).await,
            "reset" => self.cmd_reset(ctx, msg, guild_id).await,
            _ => self.send_help(ctx, msg).await,
        }
    }
}

impl PrefixesCog {
    async fn send_help(&self, ctx: &Context, msg: &Message) {
        let _ = msg
            .channel_id
            .say(
                &ctx.http,
                "Usage: `prefix add <prefix>` | `prefix remove <prefix>` | `prefix list` | `prefix reset`",
            )
            .await;
    }

    async fn send_embed(&self, ctx: &Context, msg: &Message, embed: CreateEmbed) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    async fn cmd_add(&self, ctx: &Context, msg: &Message, guild_id: u64, raw: &str) {
        if raw.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: prefix add <prefix>")
                .await;
            return;
        }

        let clean = match Self::sanitize_prefix(raw) {
            Ok(p) => p,
            Err(e) => {
                self.send_embed(ctx, msg, embeds::error_embed(&e)).await;
                return;
            }
        };

        let mut prefixes = self.current_prefixes(guild_id).await;

        if prefixes.iter().any(|p| p == &clean) {
            self.send_embed(
                ctx,
                msg,
                embeds::error_embed(&format!(
                    "You already have `{clean}` as a prefix in your server"
                )),
            )
            .await;
            return;
        }
        if prefixes.len() >= MAX_PREFIXES {
            self.send_embed(
                ctx,
                msg,
                embeds::error_embed(&format!("You can only have up to {MAX_PREFIXES} prefixes")),
            )
            .await;
            return;
        }

        let result = sqlx::query(
            "INSERT OR IGNORE INTO settings_prefixes (guild_id, prefix) VALUES (?, ?)",
        )
        .bind(guild_id as i64)
        .bind(&clean)
        .execute(self.state.servers_db())
        .await;

        if let Err(e) = result {
            tracing::error!(error = ?e, "failed to add prefix");
            self.send_embed(ctx, msg, embeds::error_embed("Database error.")).await;
            return;
        }

        prefixes.push(clean.clone());
        prefixes.sort_by_key(|p| p.len());
        self.state.prefix_cache.insert(guild_id, prefixes);

        self.send_embed(
            ctx,
            msg,
            embeds::success_embed("Success", &format!("Successfully added `{clean}` to your server")),
        )
        .await;
    }

    async fn cmd_remove(&self, ctx: &Context, msg: &Message, guild_id: u64, raw: &str) {
        if raw.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: prefix remove <prefix>")
                .await;
            return;
        }

        let clean = match Self::sanitize_prefix(raw) {
            Ok(p) => p,
            Err(e) => {
                self.send_embed(ctx, msg, embeds::error_embed(&e)).await;
                return;
            }
        };

        let mut prefixes = self.current_prefixes(guild_id).await;
        if !prefixes.iter().any(|p| p == &clean) {
            self.send_embed(
                ctx,
                msg,
                embeds::error_embed(&format!(
                    "You don't have `{clean}` as a prefix in your server"
                )),
            )
            .await;
            return;
        }

        let result = sqlx::query(
            "DELETE FROM settings_prefixes WHERE guild_id = ? AND prefix = ?",
        )
        .bind(guild_id as i64)
        .bind(&clean)
        .execute(self.state.servers_db())
        .await;

        if let Err(e) = result {
            tracing::error!(error = ?e, "failed to remove prefix");
            self.send_embed(ctx, msg, embeds::error_embed("Database error.")).await;
            return;
        }

        prefixes.retain(|p| p != &clean);
        self.state.prefix_cache.insert(guild_id, prefixes);

        self.send_embed(
            ctx,
            msg,
            embeds::success_embed(
                "Prefix Removed",
                &format!("Successfully removed `{clean}` from your server"),
            ),
        )
        .await;
    }

    async fn cmd_list(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let mut prefixes = self.current_prefixes(guild_id).await;
        if prefixes.is_empty() {
            prefixes.push(self.state.prefix().to_string());
        }

        let guild_name = ctx
            .cache
            .guild(guild_id)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "this server".to_string());

        let mut visual = String::new();
        for (count, prefix) in prefixes.iter().enumerate() {
            visual.push_str(&format!("\n{}. {}", count + 1, prefix));
        }

        let embed = CreateEmbed::new()
            .title("Prefixes")
            .description(format!(
                "Viewing prefixes for {guild_name}\n```md{visual}\n```"
            ))
            .color(colors::CYAN)
            .timestamp(Timestamp::now());
        self.send_embed(ctx, msg, embed).await;
    }

    async fn cmd_reset(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let _ = sqlx::query("DELETE FROM settings_prefixes WHERE guild_id = ?")
            .bind(guild_id as i64)
            .execute(self.state.servers_db())
            .await;
        self.state.prefix_cache.remove(&guild_id);

        let default = self.state.prefix().to_string();
        self.send_embed(
            ctx,
            msg,
            embeds::success_embed(
                "Prefixes Reset",
                &format!("Reset to the default prefix `{default}`."),
            ),
        )
        .await;
    }
}
