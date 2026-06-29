//! `/api/v1` route tree and shared helpers.
//!
//! Conventions:
//! - Every Discord snowflake id is serialized as a **string** (JSON numbers lose
//!   precision above 2^53). Request bodies likewise accept ids as strings.
//! - Reads come straight from the DB (the authoritative store the in-memory
//!   caches mirror). Writes update the DB **and** the corresponding cache in the
//!   same handler, exactly as the cogs do — see `config.rs`.

mod cases;
mod config;
mod tags;

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use std::sync::Arc;

use super::auth::{Actor, GuildScope};
use super::error::{ApiError, ApiResult};
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stats", get(stats))
        .route("/guilds", get(list_guilds))
        .route("/guilds/{gid}", get(overview))
        .merge(config::router())
        .merge(tags::router())
        .merge(cases::router())
}

// ---- shared id helpers -----------------------------------------------------

/// Stored ids are `i64` (SQLite); the wire format is the unsigned snowflake as a
/// string.
pub(super) fn id_to_string(v: i64) -> String {
    (v as u64).to_string()
}

pub(super) fn opt_id_to_string(v: Option<i64>) -> Option<String> {
    v.map(id_to_string)
}

/// Parse an optional string id from a request body into a stored `i64`. Treats
/// an empty/whitespace string the same as absent (`None`).
pub(super) fn parse_opt_id(s: &Option<String>) -> ApiResult<Option<i64>> {
    match s {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Ok(None),
        Some(v) => v
            .trim()
            .parse::<u64>()
            .map(|n| Some(n as i64))
            .map_err(|_| ApiError::bad_request("invalid id (expected a numeric snowflake string)")),
    }
}

/// Structured audit log for every API mutation (actor, guild, resource,
/// action). Shared by the config and tags handlers.
pub(super) fn audit(actor: Actor, gid: u64, resource: &str, action: &str) {
    tracing::info!(
        actor = actor.0,
        guild = gid,
        resource,
        action,
        "dashboard API mutation"
    );
}

// ---- top-level routes ------------------------------------------------------

#[derive(Serialize)]
struct StatsResponse {
    version: &'static str,
    uptime_secs: u64,
    guild_count: usize,
}

async fn stats(State(state): State<Arc<AppState>>) -> Json<StatsResponse> {
    Json(StatsResponse {
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.uptime_secs(),
        guild_count: state.guild_set.len(),
    })
}

#[derive(Serialize)]
struct GuildList {
    guild_ids: Vec<String>,
}

async fn list_guilds(State(state): State<Arc<AppState>>) -> Json<GuildList> {
    let guild_ids = state
        .guild_set
        .iter()
        .map(|e| (*e.key()).to_string())
        .collect();
    Json(GuildList { guild_ids })
}

/// Aggregated per-guild overview: every config section in one response.
#[derive(Serialize)]
struct GuildOverview {
    guild_id: String,
    prefixes: config::PrefixesBody,
    welcome: config::MessageConfig,
    goodbye: config::MessageConfig,
    logging: config::LoggingConfig,
    sentinel: config::SentinelConfig,
    roles: config::RolesConfig,
    moderation: config::ModerationConfig,
}

async fn overview(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<GuildOverview>> {
    Ok(Json(GuildOverview {
        guild_id: gid.to_string(),
        prefixes: config::read_prefixes(&state, gid).await?,
        welcome: config::read_message(&state, gid, true).await?,
        goodbye: config::read_message(&state, gid, false).await?,
        logging: config::read_logging(&state, gid).await?,
        sentinel: config::read_sentinel(&state, gid).await?,
        roles: config::read_roles(&state, gid).await?,
        moderation: config::read_moderation(&state, gid).await?,
    }))
}
