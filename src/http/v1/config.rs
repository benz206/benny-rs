//! Guild config resources: prefixes, welcome/goodbye, logging, sentinel, roles,
//! moderation.
//!
//! Every write here follows the cog contract: update the SQLite row (SeaORM)
//! AND the corresponding in-memory cache in the same handler. Validation caps
//! mirror the cogs (see comments) — no new limits are invented.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    routing::{get, put},
};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};

use crate::entities::{
    goodbye_config, logging, mod_config, prefixes, sentinel_config, sentinels_decancer,
    sticky_roles_config, welcome_autoroles, welcome_config,
};
use crate::http::auth::{Actor, GuildScope};
use crate::http::error::{ApiError, ApiResult};
use crate::state::{
    AppState, GoodbyeConfig as CacheGoodbye, LoggingConfig as CacheLogging,
    SentinelConfig as CacheSentinel, WelcomeConfig as CacheWelcome,
};

use super::{audit, id_to_string, opt_id_to_string, parse_opt_id};

// Caps mirrored from the cogs (do not invent new limits).
const MAX_PREFIXES: usize = 15; // cogs::prefixes::MAX_PREFIXES
const MAX_PREFIX_LEN: usize = 25; // cogs::prefixes::MAX_PREFIX_LEN
const LEGACY_SEP: &str = ":|:"; // cogs::prefixes::LEGACY_SEP
const MAX_AUTOROLES: usize = 25; // cogs::welcome::MAX_AUTOROLES
const DEFAULT_THRESHOLD: f64 = 0.85; // sentinels_config schema default

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/guilds/{gid}/prefixes",
            get(get_prefixes).put(put_prefixes),
        )
        .route("/guilds/{gid}/welcome", get(get_welcome).put(put_welcome))
        .route("/guilds/{gid}/goodbye", get(get_goodbye).put(put_goodbye))
        .route("/guilds/{gid}/logging", get(get_logging).put(put_logging))
        .route(
            "/guilds/{gid}/sentinel",
            get(get_sentinel).put(put_sentinel),
        )
        .route("/guilds/{gid}/roles", get(get_roles).put(put_roles))
        .route(
            "/guilds/{gid}/moderation",
            put(put_moderation).get(get_moderation),
        )
}

// ---- prefixes --------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct PrefixesBody {
    pub prefixes: Vec<String>,
}

pub(super) async fn read_prefixes(state: &AppState, gid: u64) -> ApiResult<PrefixesBody> {
    let rows = prefixes::Entity::find()
        .filter(prefixes::Column::GuildId.eq(gid as i64))
        .all(state.servers_orm())
        .await?;
    let mut prefixes: Vec<String> = rows.into_iter().map(|m| m.prefix).collect();
    prefixes.sort_by_key(|p| p.len());
    Ok(PrefixesBody { prefixes })
}

async fn get_prefixes(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<PrefixesBody>> {
    Ok(Json(read_prefixes(&state, gid).await?))
}

/// PUT replaces the full prefix set. Mirrors `cogs::prefixes`: clear + insert,
/// then rewrite `prefix_cache` (empty list => remove, matching `guild_prefixes`
/// falling back to the default).
async fn put_prefixes(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<PrefixesBody>,
) -> ApiResult<Json<PrefixesBody>> {
    let mut clean: Vec<String> = Vec::new();
    for raw in &body.prefixes {
        let p = sanitize_prefix(raw)?;
        if !clean.contains(&p) {
            clean.push(p);
        }
    }
    if clean.len() > MAX_PREFIXES {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_PREFIXES} prefixes allowed"
        )));
    }

    prefixes::Entity::delete_many()
        .filter(prefixes::Column::GuildId.eq(gid as i64))
        .exec(state.servers_orm())
        .await?;
    for p in &clean {
        // Tolerate a conflicting row (no-op) like the cog does.
        let _ = prefixes::Entity::insert(prefixes::ActiveModel {
            guild_id: Set(gid as i64),
            prefix: Set(p.clone()),
        })
        .on_conflict(
            OnConflict::columns([prefixes::Column::GuildId, prefixes::Column::Prefix])
                .do_nothing()
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await;
    }

    let mut sorted = clean;
    sorted.sort_by_key(|p| p.len());
    if sorted.is_empty() {
        state.prefix_cache.remove(&gid);
    } else {
        state.prefix_cache.insert(gid, sorted.clone());
    }
    audit(actor, gid, "prefixes", "put");
    Ok(Json(PrefixesBody { prefixes: sorted }))
}

/// Mirror of `cogs::prefixes::sanitize_prefix`.
fn sanitize_prefix(raw: &str) -> ApiResult<String> {
    if raw.contains(LEGACY_SEP) {
        return Err(ApiError::bad_request("prefix may not contain ':|:'"));
    }
    let clean = raw.trim();
    if clean.is_empty() {
        return Err(ApiError::bad_request("prefix must not be empty"));
    }
    if clean.chars().count() > MAX_PREFIX_LEN {
        return Err(ApiError::bad_request(format!(
            "prefix must be at most {MAX_PREFIX_LEN} characters"
        )));
    }
    Ok(clean.to_string())
}

// ---- welcome / goodbye -----------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct MessageConfig {
    pub channel_id: Option<String>,
    pub message: String,
    pub embed_json: Option<String>,
    pub enabled: bool,
}

impl MessageConfig {
    fn empty() -> Self {
        Self {
            channel_id: None,
            message: String::new(),
            embed_json: None,
            enabled: false,
        }
    }
}

pub(super) async fn read_message(
    state: &AppState,
    gid: u64,
    is_welcome: bool,
) -> ApiResult<MessageConfig> {
    if is_welcome {
        let row = welcome_config::Entity::find_by_id(gid as i64)
            .one(state.servers_orm())
            .await?;
        Ok(row.map_or_else(MessageConfig::empty, |m| MessageConfig {
            channel_id: opt_id_to_string(m.channel_id),
            message: m.message,
            embed_json: m.embed_json,
            enabled: m.enabled,
        }))
    } else {
        let row = goodbye_config::Entity::find_by_id(gid as i64)
            .one(state.servers_orm())
            .await?;
        Ok(row.map_or_else(MessageConfig::empty, |m| MessageConfig {
            channel_id: opt_id_to_string(m.channel_id),
            message: m.message,
            embed_json: m.embed_json,
            enabled: m.enabled,
        }))
    }
}

/// Validate, upsert the full row, then refresh the welcome/goodbye cache —
/// mirrors `cogs::welcome::upsert_config` + `reload`.
async fn put_message(
    state: &AppState,
    gid: u64,
    actor: Actor,
    is_welcome: bool,
    body: MessageConfig,
) -> ApiResult<Json<MessageConfig>> {
    let embed_json = match body.embed_json.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(serde_json::Value::Object(_)) => Some(s.to_string()),
            _ => return Err(ApiError::bad_request("embed_json must be a JSON object")),
        },
    };
    let channel_id = parse_opt_id(&body.channel_id)?;
    let message = body.message;
    let enabled = body.enabled;

    if is_welcome {
        welcome_config::Entity::insert(welcome_config::ActiveModel {
            guild_id: Set(gid as i64),
            channel_id: Set(channel_id),
            message: Set(message.clone()),
            embed_json: Set(embed_json.clone()),
            enabled: Set(enabled),
        })
        .on_conflict(
            OnConflict::column(welcome_config::Column::GuildId)
                .update_columns([
                    welcome_config::Column::ChannelId,
                    welcome_config::Column::Message,
                    welcome_config::Column::EmbedJson,
                    welcome_config::Column::Enabled,
                ])
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await?;
        state.welcome_cache.insert(
            gid,
            CacheWelcome {
                channel_id,
                message: message.clone(),
                embed_json: embed_json.clone(),
                enabled,
            },
        );
    } else {
        goodbye_config::Entity::insert(goodbye_config::ActiveModel {
            guild_id: Set(gid as i64),
            channel_id: Set(channel_id),
            message: Set(message.clone()),
            embed_json: Set(embed_json.clone()),
            enabled: Set(enabled),
        })
        .on_conflict(
            OnConflict::column(goodbye_config::Column::GuildId)
                .update_columns([
                    goodbye_config::Column::ChannelId,
                    goodbye_config::Column::Message,
                    goodbye_config::Column::EmbedJson,
                    goodbye_config::Column::Enabled,
                ])
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await?;
        state.goodbye_cache.insert(
            gid,
            CacheGoodbye {
                channel_id,
                message: message.clone(),
                embed_json: embed_json.clone(),
                enabled,
            },
        );
    }

    audit(actor, gid, if is_welcome { "welcome" } else { "goodbye" }, "put");
    Ok(Json(MessageConfig {
        channel_id: opt_id_to_string(channel_id),
        message,
        embed_json,
        enabled,
    }))
}

async fn get_welcome(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<MessageConfig>> {
    Ok(Json(read_message(&state, gid, true).await?))
}

async fn put_welcome(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<MessageConfig>,
) -> ApiResult<Json<MessageConfig>> {
    put_message(&state, gid, actor, true, body).await
}

async fn get_goodbye(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<MessageConfig>> {
    Ok(Json(read_message(&state, gid, false).await?))
}

async fn put_goodbye(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<MessageConfig>,
) -> ApiResult<Json<MessageConfig>> {
    put_message(&state, gid, actor, false, body).await
}

// ---- logging ---------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct LoggingConfig {
    pub webhook_url: String,
    pub enabled: bool,
}

pub(super) async fn read_logging(state: &AppState, gid: u64) -> ApiResult<LoggingConfig> {
    let row = logging::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?;
    Ok(row.map_or(
        LoggingConfig {
            webhook_url: String::new(),
            enabled: false,
        },
        |m| LoggingConfig {
            webhook_url: m.webhook_url,
            enabled: m.enabled,
        },
    ))
}

async fn get_logging(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<LoggingConfig>> {
    Ok(Json(read_logging(&state, gid).await?))
}

async fn put_logging(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<LoggingConfig>,
) -> ApiResult<Json<LoggingConfig>> {
    let url = body.webhook_url.trim().to_string();
    // Mirror cogs::logging: only a Discord webhook host may be stored, since the
    // log stream carries message edit/delete content.
    if !url.is_empty() && !is_discord_webhook(&url) {
        return Err(ApiError::bad_request(
            "webhook_url must be a Discord webhook (https://discord.com/api/webhooks/...)",
        ));
    }
    if url.is_empty() && body.enabled {
        return Err(ApiError::bad_request(
            "cannot enable logging without a webhook_url",
        ));
    }

    logging::Entity::insert(logging::ActiveModel {
        guild_id: Set(gid as i64),
        webhook_url: Set(url.clone()),
        enabled: Set(body.enabled),
    })
    .on_conflict(
        OnConflict::column(logging::Column::GuildId)
            .update_columns([logging::Column::WebhookUrl, logging::Column::Enabled])
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await?;

    state.logging_cache.insert(
        gid,
        CacheLogging {
            webhook_url: url.clone(),
            enabled: body.enabled,
        },
    );
    audit(actor, gid, "logging", "put");
    Ok(Json(LoggingConfig {
        webhook_url: url,
        enabled: body.enabled,
    }))
}

/// Mirror of `cogs::logging::is_discord_webhook`.
fn is_discord_webhook(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "discord.com" | "discordapp.com" | "ptb.discord.com" | "canary.discord.com"
    ) && path.starts_with("api/webhooks/")
}

// ---- sentinel --------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Thresholds {
    pub toxicity: f64,
    pub severe_toxicity: f64,
    pub obscene: f64,
    pub threat: f64,
    pub insult: f64,
    pub identity_attack: f64,
    pub sexual_explicit: f64,
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct DecancerConfig {
    pub enabled: bool,
    pub log_channel_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct SentinelConfig {
    pub enabled: bool,
    pub log_channel_id: Option<String>,
    pub delete_flagged: bool,
    pub thresholds: Thresholds,
    pub decancer: DecancerConfig,
}

pub(super) async fn read_sentinel(state: &AppState, gid: u64) -> ApiResult<SentinelConfig> {
    let row = sentinel_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?;
    let drow = sentinels_decancer::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?;

    let (enabled, log_channel_id, delete_flagged, thresholds) = match row {
        Some(m) => (
            m.enabled,
            opt_id_to_string(m.log_channel_id),
            m.delete_flagged,
            Thresholds {
                toxicity: m.toxicity,
                severe_toxicity: m.severe_toxicity,
                obscene: m.obscene,
                threat: m.threat,
                insult: m.insult,
                identity_attack: m.identity_attack,
                sexual_explicit: m.sexual_explicit,
            },
        ),
        None => (
            false,
            None,
            false,
            Thresholds {
                toxicity: DEFAULT_THRESHOLD,
                severe_toxicity: DEFAULT_THRESHOLD,
                obscene: DEFAULT_THRESHOLD,
                threat: DEFAULT_THRESHOLD,
                insult: DEFAULT_THRESHOLD,
                identity_attack: DEFAULT_THRESHOLD,
                sexual_explicit: DEFAULT_THRESHOLD,
            },
        ),
    };
    let decancer = drow.map_or(
        DecancerConfig {
            enabled: false,
            log_channel_id: None,
        },
        |d| DecancerConfig {
            enabled: d.enabled,
            log_channel_id: opt_id_to_string(d.log_channel_id),
        },
    );

    Ok(SentinelConfig {
        enabled,
        log_channel_id,
        delete_flagged,
        thresholds,
        decancer,
    })
}

async fn get_sentinel(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<SentinelConfig>> {
    Ok(Json(read_sentinel(&state, gid).await?))
}

/// Upsert the full sentinel + decancer rows and mirror all three caches:
/// `sentinel_cache`, and the cog-private `delete_flagged` / decancer caches via
/// `cogs::sentinel`'s pub setters.
async fn put_sentinel(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<SentinelConfig>,
) -> ApiResult<Json<SentinelConfig>> {
    let t = &body.thresholds;
    for (label, v) in [
        ("toxicity", t.toxicity),
        ("severe_toxicity", t.severe_toxicity),
        ("obscene", t.obscene),
        ("threat", t.threat),
        ("insult", t.insult),
        ("identity_attack", t.identity_attack),
        ("sexual_explicit", t.sexual_explicit),
    ] {
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(ApiError::bad_request(format!(
                "threshold {label} must be between 0.0 and 1.0"
            )));
        }
    }

    let log_channel_id = parse_opt_id(&body.log_channel_id)?;
    let decancer_log = parse_opt_id(&body.decancer.log_channel_id)?;

    sentinel_config::Entity::insert(sentinel_config::ActiveModel {
        guild_id: Set(gid as i64),
        enabled: Set(body.enabled),
        log_channel_id: Set(log_channel_id),
        toxicity: Set(t.toxicity),
        severe_toxicity: Set(t.severe_toxicity),
        obscene: Set(t.obscene),
        threat: Set(t.threat),
        insult: Set(t.insult),
        identity_attack: Set(t.identity_attack),
        sexual_explicit: Set(t.sexual_explicit),
        delete_flagged: Set(body.delete_flagged),
    })
    .on_conflict(
        OnConflict::column(sentinel_config::Column::GuildId)
            .update_columns([
                sentinel_config::Column::Enabled,
                sentinel_config::Column::LogChannelId,
                sentinel_config::Column::Toxicity,
                sentinel_config::Column::SevereToxicity,
                sentinel_config::Column::Obscene,
                sentinel_config::Column::Threat,
                sentinel_config::Column::Insult,
                sentinel_config::Column::IdentityAttack,
                sentinel_config::Column::SexualExplicit,
                sentinel_config::Column::DeleteFlagged,
            ])
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await?;

    state.sentinel_cache.insert(
        gid,
        CacheSentinel {
            enabled: body.enabled,
            log_channel_id,
            toxicity: t.toxicity,
            severe_toxicity: t.severe_toxicity,
            obscene: t.obscene,
            threat: t.threat,
            insult: t.insult,
            identity_attack: t.identity_attack,
            sexual_explicit: t.sexual_explicit,
        },
    );
    crate::cogs::sentinel::cache_set_delete_flagged(gid, body.delete_flagged);

    sentinels_decancer::Entity::insert(sentinels_decancer::ActiveModel {
        guild_id: Set(gid as i64),
        enabled: Set(body.decancer.enabled),
        log_channel_id: Set(decancer_log),
    })
    .on_conflict(
        OnConflict::column(sentinels_decancer::Column::GuildId)
            .update_columns([
                sentinels_decancer::Column::Enabled,
                sentinels_decancer::Column::LogChannelId,
            ])
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await?;
    crate::cogs::sentinel::cache_set_decancer(gid, body.decancer.enabled, decancer_log);

    audit(actor, gid, "sentinel", "put");
    Ok(Json(read_sentinel(&state, gid).await?))
}

// ---- roles (autoroles + sticky toggle) -------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct RolesConfig {
    pub autoroles: Vec<String>,
    pub sticky_enabled: bool,
}

pub(super) async fn read_roles(state: &AppState, gid: u64) -> ApiResult<RolesConfig> {
    let rows = welcome_autoroles::Entity::find()
        .filter(welcome_autoroles::Column::GuildId.eq(gid as i64))
        .all(state.servers_orm())
        .await?;
    let autoroles = rows.into_iter().map(|m| id_to_string(m.role_id)).collect();

    let sticky_enabled = sticky_roles_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .map(|m| m.enabled)
        .unwrap_or(false);

    Ok(RolesConfig {
        autoroles,
        sticky_enabled,
    })
}

async fn get_roles(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<RolesConfig>> {
    Ok(Json(read_roles(&state, gid).await?))
}

/// Replace the autorole set and set the sticky toggle. Both are read straight
/// from the DB by the cog on each join (no AppState cache), so this is DB-only.
async fn put_roles(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<RolesConfig>,
) -> ApiResult<Json<RolesConfig>> {
    let mut role_ids: Vec<i64> = Vec::new();
    for raw in &body.autoroles {
        let id = raw
            .trim()
            .parse::<u64>()
            .map_err(|_| ApiError::bad_request("invalid role id"))? as i64;
        if !role_ids.contains(&id) {
            role_ids.push(id);
        }
    }
    if role_ids.len() > MAX_AUTOROLES {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_AUTOROLES} autoroles allowed"
        )));
    }

    welcome_autoroles::Entity::delete_many()
        .filter(welcome_autoroles::Column::GuildId.eq(gid as i64))
        .exec(state.servers_orm())
        .await?;
    for id in &role_ids {
        let _ = welcome_autoroles::Entity::insert(welcome_autoroles::ActiveModel {
            guild_id: Set(gid as i64),
            role_id: Set(*id),
        })
        .on_conflict(
            OnConflict::columns([
                welcome_autoroles::Column::GuildId,
                welcome_autoroles::Column::RoleId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(state.servers_orm())
        .await;
    }

    sticky_roles_config::Entity::insert(sticky_roles_config::ActiveModel {
        guild_id: Set(gid as i64),
        enabled: Set(body.sticky_enabled),
    })
    .on_conflict(
        OnConflict::column(sticky_roles_config::Column::GuildId)
            .update_column(sticky_roles_config::Column::Enabled)
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await?;

    audit(actor, gid, "roles", "put");
    Ok(Json(read_roles(&state, gid).await?))
}

// ---- moderation config -----------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct ModerationConfig {
    pub mute_role_id: Option<String>,
}

pub(super) async fn read_moderation(state: &AppState, gid: u64) -> ApiResult<ModerationConfig> {
    let mute_role_id = mod_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .and_then(|m| m.mute_role_id);
    Ok(ModerationConfig {
        mute_role_id: opt_id_to_string(mute_role_id),
    })
}

async fn get_moderation(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<ModerationConfig>> {
    Ok(Json(read_moderation(&state, gid).await?))
}

/// Upsert the mute role. Read from the DB by the cog on each mute/unmute (no
/// AppState cache), so this is DB-only.
async fn put_moderation(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<ModerationConfig>,
) -> ApiResult<Json<ModerationConfig>> {
    let mute_role_id = parse_opt_id(&body.mute_role_id)?;

    mod_config::Entity::insert(mod_config::ActiveModel {
        guild_id: Set(gid as i64),
        mute_role_id: Set(mute_role_id),
    })
    .on_conflict(
        OnConflict::column(mod_config::Column::GuildId)
            .update_column(mod_config::Column::MuteRoleId)
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await?;

    audit(actor, gid, "moderation", "put");
    Ok(Json(ModerationConfig {
        mute_role_id: opt_id_to_string(mute_role_id),
    }))
}
