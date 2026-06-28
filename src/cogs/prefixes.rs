use super::Cog;
use crate::entities::prefixes;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, Set};
use serenity::all::{CreateEmbed, Guild, Timestamp, UnavailableGuild};
use std::sync::Arc;

/// Maximum number of custom prefixes a guild may have.
const MAX_PREFIXES: usize = 5;
/// Maximum length of a single prefix; values over this limit are rejected.
const MAX_PREFIX_LEN: usize = 25;
/// Separator previously used to join multiple prefixes in a single DB column.
/// Disallowed as a prefix so it can never corrupt storage.
const LEGACY_SEP: &str = ":|:";

pub struct PrefixesCog {
    state: Arc<AppState>,
}

impl PrefixesCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for PrefixesCog {
    /// Hydrate `prefix_cache` from the DB on startup.
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        let rows = prefixes::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        let mut count = 0usize;
        for m in rows {
            self.state
                .prefix_cache
                .entry(m.guild_id as u64)
                .or_default()
                .push(m.prefix);
            count += 1;
        }
        // Keep each guild's prefixes sorted by length.
        for mut entry in self.state.prefix_cache.iter_mut() {
            entry.value_mut().sort_by_key(|p| p.len());
        }
        tracing::info!("Prefix cache loaded ({count} prefixes)");
    }

    /// On joining a guild (or initial guild sync), ensure a default prefix row
    /// and cache entry exist. Idempotent: skips guilds already cached.
    async fn on_guild_create(&self, _ctx: &serenity::all::Context, guild: &Guild) {
        let guild_id = guild.id.get();
        if self.state.prefix_cache.contains_key(&guild_id) {
            return;
        }

        let existing = current_prefixes(&self.state, guild_id).await;
        if existing.is_empty() {
            let default = self.state.prefix().to_string();
            let _ = prefixes::Entity::insert(prefixes::ActiveModel {
                guild_id: Set(guild_id as i64),
                prefix: Set(default.clone()),
            })
            .on_conflict(
                OnConflict::columns([prefixes::Column::GuildId, prefixes::Column::Prefix])
                    .do_nothing()
                    .to_owned(),
            )
            .exec(self.state.servers_orm())
            .await;
            self.state.prefix_cache.insert(guild_id, vec![default]);
        } else {
            self.state.prefix_cache.insert(guild_id, existing);
        }
    }

    /// On leaving a guild, remove its prefixes from the DB and cache.
    async fn on_guild_delete(
        &self,
        _ctx: &serenity::all::Context,
        incomplete: UnavailableGuild,
        _full: Option<Guild>,
    ) {
        let guild_id = incomplete.id.get();
        let _ = prefixes::Entity::delete_many()
            .filter(prefixes::Column::GuildId.eq(guild_id as i64))
            .exec(self.state.servers_orm())
            .await;
        self.state.prefix_cache.remove(&guild_id);
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![prefix()]
}

// ---- commands --------------------------------------------------------------

/// Manage custom command prefixes for this server.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Prefixes",
    subcommands("prefix_add", "prefix_remove", "prefix_list", "prefix_reset"),
    subcommand_required,
    guild_only
)]
async fn prefix(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Add a new custom prefix to this server.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "add",
    required_permissions = "MANAGE_GUILD",
    guild_only
)]
async fn prefix_add(
    ctx: Context<'_>,
    #[description = "Prefix to add"] prefix: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;

    let clean = match sanitize_prefix(&prefix) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e).await,
    };

    let mut guild_prefixes = current_prefixes(state, guild_id).await;

    if guild_prefixes.iter().any(|p| p == &clean) {
        return send_error(
            ctx,
            &format!("You already have `{clean}` as a prefix in your server"),
        )
        .await;
    }
    if guild_prefixes.len() >= MAX_PREFIXES {
        return send_error(
            ctx,
            &format!("You can only have up to {MAX_PREFIXES} prefixes"),
        )
        .await;
    }

    let result = prefixes::Entity::insert(prefixes::ActiveModel {
        guild_id: Set(guild_id as i64),
        prefix: Set(clean.clone()),
    })
    .on_conflict(
        OnConflict::columns([prefixes::Column::GuildId, prefixes::Column::Prefix])
            .do_nothing()
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;

    match result {
        Ok(_) | Err(DbErr::RecordNotInserted) => {}
        Err(e) => {
            tracing::error!(error = ?e, "failed to add prefix");
            return send_error(ctx, "Database error.").await;
        }
    }

    guild_prefixes.push(clean.clone());
    guild_prefixes.sort_by_key(|p| p.len());
    state.prefix_cache.insert(guild_id, guild_prefixes);

    send_embed(
        ctx,
        embeds::success_embed("Success", &format!("Successfully added `{clean}` to your server")),
    )
    .await
}

/// Remove a custom prefix from this server.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "remove",
    required_permissions = "MANAGE_GUILD",
    guild_only
)]
async fn prefix_remove(
    ctx: Context<'_>,
    #[description = "Prefix to remove"] prefix: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;

    let clean = match sanitize_prefix(&prefix) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e).await,
    };

    let mut guild_prefixes = current_prefixes(state, guild_id).await;
    if !guild_prefixes.iter().any(|p| p == &clean) {
        return send_error(
            ctx,
            &format!("You don't have `{clean}` as a prefix in your server"),
        )
        .await;
    }

    let result = prefixes::Entity::delete_many()
        .filter(prefixes::Column::GuildId.eq(guild_id as i64))
        .filter(prefixes::Column::Prefix.eq(clean.as_str()))
        .exec(state.servers_orm())
        .await;

    if let Err(e) = result {
        tracing::error!(error = ?e, "failed to remove prefix");
        return send_error(ctx, "Database error.").await;
    }

    guild_prefixes.retain(|p| p != &clean);
    state.prefix_cache.insert(guild_id, guild_prefixes);

    send_embed(
        ctx,
        embeds::success_embed(
            "Prefix Removed",
            &format!("Successfully removed `{clean}` from your server"),
        ),
    )
    .await
}

/// List all custom prefixes for this server.
#[poise::command(slash_command, prefix_command, rename = "list", guild_only)]
async fn prefix_list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;
    let sctx = ctx.serenity_context();

    let mut guild_prefixes = current_prefixes(state, guild_id.get()).await;
    if guild_prefixes.is_empty() {
        guild_prefixes.push(state.prefix().to_string());
    }

    let guild_name = sctx
        .cache
        .guild(guild_id)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| "this server".to_string());

    let mut visual = String::new();
    for (count, prefix) in guild_prefixes.iter().enumerate() {
        visual.push_str(&format!("\n{}. {}", count + 1, prefix));
    }

    let embed = CreateEmbed::new()
        .title("Prefixes")
        .description(format!(
            "Viewing prefixes for {guild_name}\n```md{visual}\n```"
        ))
        .color(colors::CYAN)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Reset all custom prefixes back to the default.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "reset",
    required_permissions = "MANAGE_GUILD",
    guild_only
)]
async fn prefix_reset(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;

    let _ = prefixes::Entity::delete_many()
        .filter(prefixes::Column::GuildId.eq(guild_id as i64))
        .exec(state.servers_orm())
        .await;
    state.prefix_cache.remove(&guild_id);

    let default = state.prefix().to_string();
    send_embed(
        ctx,
        embeds::success_embed(
            "Prefixes Reset",
            &format!("Reset to the default prefix `{default}`."),
        ),
    )
    .await
}

// ---- helpers ---------------------------------------------------------------

/// Sanitize a raw prefix: strip leading/trailing whitespace, reject the `:|:`
/// separator, enforce non-empty and max length. Inner spaces are allowed.
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
async fn current_prefixes(state: &AppState, guild_id: u64) -> Vec<String> {
    state.custom_prefixes(guild_id).await
}
