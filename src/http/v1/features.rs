//! Feature-module config: leveling, starboard and automod, plus the read-only
//! views they imply (level leaderboard, role rewards, active giveaways).
//!
//! These three cogs each keep a module-private `CONFIG_CACHE` that their event
//! handlers read on the hot path. Rather than duplicate the DB write + cache
//! refresh here, every write goes through the cog's own `update_config`, which
//! is the single place that keeps the two in step.

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};

use crate::cogs::{automod, levels, starboard};
use crate::entities::{
    automod_config, giveaway_entries, giveaways, levels_config, levels_rewards, levels_users,
    starboard_config,
};
use crate::http::auth::{Actor, GuildScope};
use crate::http::error::{ApiError, ApiResult};
use crate::state::AppState;

use super::{audit, id_to_string, opt_id_to_string, parse_id, parse_opt_id};

/// Punishment/raid-action vocabularies accepted by `cogs::automod`.
const PUNISHMENTS: [&str; 4] = ["delete", "warn", "timeout", "kick"];
const RAID_ACTIONS: [&str; 2] = ["alert", "kick"];
/// Timeout bounds applied by `cogs::automod` before issuing a timeout.
const MIN_TIMEOUT_SECS: i64 = 60;
const MAX_TIMEOUT_SECS: i64 = 28 * 24 * 60 * 60;
/// A guild may map at most this many levels to role rewards.
const MAX_REWARDS: usize = 50;
const LEADERBOARD_LIMIT: u64 = 25;

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/guilds/{gid}/levels", get(get_levels).put(put_levels))
        .route(
            "/guilds/{gid}/levels/rewards",
            get(get_rewards).put(put_rewards),
        )
        .route("/guilds/{gid}/levels/leaderboard", get(get_leaderboard))
        .route(
            "/guilds/{gid}/starboard",
            get(get_starboard).put(put_starboard),
        )
        .route("/guilds/{gid}/automod", get(get_automod).put(put_automod))
        .route("/guilds/{gid}/giveaways", get(get_giveaways))
}

// ---- levels ----------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct LevelsConfig {
    pub enabled: bool,
    pub announce: bool,
    pub levelup_channel_id: Option<String>,
    pub xp_min: i64,
    pub xp_max: i64,
    pub cooldown_secs: i64,
}

pub(super) async fn read_levels(state: &AppState, gid: u64) -> ApiResult<LevelsConfig> {
    let model = levels_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .unwrap_or_else(|| levels::default_model(gid));
    Ok(LevelsConfig {
        enabled: model.enabled,
        announce: model.announce,
        levelup_channel_id: opt_id_to_string(model.levelup_channel_id),
        xp_min: model.xp_min,
        xp_max: model.xp_max,
        cooldown_secs: model.cooldown_secs,
    })
}

async fn get_levels(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<LevelsConfig>> {
    Ok(Json(read_levels(&state, gid).await?))
}

async fn put_levels(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<LevelsConfig>,
) -> ApiResult<Json<LevelsConfig>> {
    // `cogs::levels` awards a random value in `xp_min..=xp_max` on a cooldown,
    // so both must be positive and ordered.
    if body.xp_min < 1 || body.xp_max < body.xp_min {
        return Err(ApiError::bad_request(
            "xp_min must be at least 1 and no greater than xp_max",
        ));
    }
    if body.xp_max > 1000 {
        return Err(ApiError::bad_request("xp_max must be at most 1000"));
    }
    if body.cooldown_secs < 1 || body.cooldown_secs > 3600 {
        return Err(ApiError::bad_request(
            "cooldown_secs must be between 1 and 3600",
        ));
    }
    let channel_id = parse_opt_id(&body.levelup_channel_id)?;

    levels::update_config(&state, gid, |c| {
        c.enabled = body.enabled;
        c.announce = body.announce;
        c.levelup_channel_id = channel_id;
        c.xp_min = body.xp_min;
        c.xp_max = body.xp_max;
        c.cooldown_secs = body.cooldown_secs;
    })
    .await?;

    audit(actor, gid, "levels", "put");
    Ok(Json(read_levels(&state, gid).await?))
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Reward {
    pub level: i64,
    pub role_id: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct RewardsBody {
    pub rewards: Vec<Reward>,
}

async fn read_rewards(state: &AppState, gid: u64) -> ApiResult<RewardsBody> {
    let rows = levels_rewards::Entity::find()
        .filter(levels_rewards::Column::GuildId.eq(gid as i64))
        .order_by_asc(levels_rewards::Column::Level)
        .all(state.servers_orm())
        .await?;
    Ok(RewardsBody {
        rewards: rows
            .into_iter()
            .map(|r| Reward {
                level: r.level,
                role_id: id_to_string(r.role_id),
            })
            .collect(),
    })
}

async fn get_rewards(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<RewardsBody>> {
    Ok(Json(read_rewards(&state, gid).await?))
}

/// PUT replaces the whole reward set, then reloads the cog's rewards cache.
async fn put_rewards(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<RewardsBody>,
) -> ApiResult<Json<RewardsBody>> {
    if body.rewards.len() > MAX_REWARDS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_REWARDS} role rewards allowed"
        )));
    }
    let mut parsed: Vec<(i64, i64)> = Vec::with_capacity(body.rewards.len());
    for r in &body.rewards {
        if r.level < 1 {
            return Err(ApiError::bad_request("level must be at least 1"));
        }
        let role_id = parse_id(&r.role_id)?;
        if parsed.iter().any(|(level, _)| *level == r.level) {
            return Err(ApiError::bad_request(format!(
                "duplicate reward for level {}",
                r.level
            )));
        }
        parsed.push((r.level, role_id));
    }

    levels_rewards::Entity::delete_many()
        .filter(levels_rewards::Column::GuildId.eq(gid as i64))
        .exec(state.servers_orm())
        .await?;
    for (level, role_id) in &parsed {
        levels_rewards::Entity::insert(levels_rewards::ActiveModel {
            guild_id: Set(gid as i64),
            level: Set(*level),
            role_id: Set(*role_id),
        })
        .on_conflict(
            OnConflict::columns([
                levels_rewards::Column::GuildId,
                levels_rewards::Column::Level,
            ])
            .update_column(levels_rewards::Column::RoleId)
            .to_owned(),
        )
        .exec(state.servers_orm())
        .await?;
    }
    levels::refresh_rewards_cache(&state, gid).await;

    audit(actor, gid, "levels/rewards", "put");
    Ok(Json(read_rewards(&state, gid).await?))
}

#[derive(Serialize)]
struct LeaderboardEntry {
    rank: usize,
    user_id: String,
    xp: i64,
    level: i64,
    /// XP earned into the current level, and what the next one costs — the same
    /// pair `/rank` renders as a progress bar.
    xp_into_level: i64,
    xp_for_next: i64,
}

#[derive(Serialize)]
struct Leaderboard {
    entries: Vec<LeaderboardEntry>,
}

async fn get_leaderboard(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<Leaderboard>> {
    let rows = levels_users::Entity::find()
        .filter(levels_users::Column::GuildId.eq(gid as i64))
        .order_by_desc(levels_users::Column::Xp)
        .limit(LEADERBOARD_LIMIT)
        .all(state.servers_orm())
        .await?;
    Ok(Json(Leaderboard {
        entries: rows
            .into_iter()
            .enumerate()
            .map(|(i, u)| {
                let (level, xp_into_level, xp_for_next) = levels::level_from_xp(u.xp);
                LeaderboardEntry {
                    rank: i + 1,
                    user_id: id_to_string(u.user_id),
                    xp: u.xp,
                    level,
                    xp_into_level,
                    xp_for_next,
                }
            })
            .collect(),
    }))
}

// ---- starboard -------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct StarboardConfig {
    pub enabled: bool,
    pub channel_id: Option<String>,
    pub threshold: i64,
    pub emoji: String,
    pub self_star: bool,
}

pub(super) async fn read_starboard(state: &AppState, gid: u64) -> ApiResult<StarboardConfig> {
    let model = starboard_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .unwrap_or_else(|| starboard::default_model(gid));
    Ok(StarboardConfig {
        enabled: model.enabled,
        channel_id: opt_id_to_string(model.channel_id),
        threshold: model.threshold,
        emoji: model.emoji,
        self_star: model.self_star,
    })
}

async fn get_starboard(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<StarboardConfig>> {
    Ok(Json(read_starboard(&state, gid).await?))
}

async fn put_starboard(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<StarboardConfig>,
) -> ApiResult<Json<StarboardConfig>> {
    if body.threshold < 1 {
        return Err(ApiError::bad_request("threshold must be at least 1"));
    }
    let emoji = body.emoji.trim().to_string();
    // The cog compares `reaction.emoji.to_string()` against this verbatim.
    if emoji.is_empty() || emoji.chars().count() > 64 {
        return Err(ApiError::bad_request(
            "emoji must be between 1 and 64 characters",
        ));
    }
    let channel_id = parse_opt_id(&body.channel_id)?;
    if body.enabled && channel_id.is_none() {
        return Err(ApiError::bad_request(
            "a channel_id is required to enable the starboard",
        ));
    }

    starboard::update_config(&state, gid, |c| {
        c.enabled = body.enabled;
        c.channel_id = channel_id;
        c.threshold = body.threshold;
        c.emoji = emoji;
        c.self_star = body.self_star;
    })
    .await?;

    audit(actor, gid, "starboard", "put");
    Ok(Json(read_starboard(&state, gid).await?))
}

// ---- automod ---------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct AutomodConfig {
    pub enabled: bool,
    pub log_channel_id: Option<String>,
    pub anti_invite: bool,
    pub anti_link: bool,
    pub mention_limit: i64,
    pub spam_msgs: i64,
    pub spam_secs: i64,
    pub punishment: String,
    pub timeout_secs: i64,
    pub raid_enabled: bool,
    pub raid_joins: i64,
    pub raid_secs: i64,
    pub min_account_age_days: i64,
    pub raid_action: String,
}

pub(super) async fn read_automod(state: &AppState, gid: u64) -> ApiResult<AutomodConfig> {
    let model = automod_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .unwrap_or_else(|| automod::default_model(gid));
    Ok(AutomodConfig {
        enabled: model.enabled,
        log_channel_id: opt_id_to_string(model.log_channel_id),
        anti_invite: model.anti_invite,
        anti_link: model.anti_link,
        mention_limit: model.mention_limit,
        spam_msgs: model.spam_msgs,
        spam_secs: model.spam_secs,
        punishment: model.punishment,
        timeout_secs: model.timeout_secs,
        raid_enabled: model.raid_enabled,
        raid_joins: model.raid_joins,
        raid_secs: model.raid_secs,
        min_account_age_days: model.min_account_age_days,
        raid_action: model.raid_action,
    })
}

async fn get_automod(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<AutomodConfig>> {
    Ok(Json(read_automod(&state, gid).await?))
}

async fn put_automod(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<AutomodConfig>,
) -> ApiResult<Json<AutomodConfig>> {
    if !PUNISHMENTS.contains(&body.punishment.as_str()) {
        return Err(ApiError::bad_request(format!(
            "punishment must be one of: {}",
            PUNISHMENTS.join(", ")
        )));
    }
    if !RAID_ACTIONS.contains(&body.raid_action.as_str()) {
        return Err(ApiError::bad_request(format!(
            "raid_action must be one of: {}",
            RAID_ACTIONS.join(", ")
        )));
    }
    // Zero disables a numeric filter, so only negatives are rejected.
    for (name, value) in [
        ("mention_limit", body.mention_limit),
        ("spam_msgs", body.spam_msgs),
        ("spam_secs", body.spam_secs),
        ("raid_joins", body.raid_joins),
        ("raid_secs", body.raid_secs),
        ("min_account_age_days", body.min_account_age_days),
    ] {
        if value < 0 {
            return Err(ApiError::bad_request(format!(
                "{name} must not be negative"
            )));
        }
    }
    if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&body.timeout_secs) {
        return Err(ApiError::bad_request(format!(
            "timeout_secs must be between {MIN_TIMEOUT_SECS} and {MAX_TIMEOUT_SECS}"
        )));
    }
    let log_channel_id = parse_opt_id(&body.log_channel_id)?;
    let punishment = body.punishment.clone();
    let raid_action = body.raid_action.clone();

    automod::update_config(&state, gid, |c| {
        c.enabled = body.enabled;
        c.log_channel_id = log_channel_id;
        c.anti_invite = body.anti_invite;
        c.anti_link = body.anti_link;
        c.mention_limit = body.mention_limit;
        c.spam_msgs = body.spam_msgs;
        c.spam_secs = body.spam_secs;
        c.punishment = punishment;
        c.timeout_secs = body.timeout_secs;
        c.raid_enabled = body.raid_enabled;
        c.raid_joins = body.raid_joins;
        c.raid_secs = body.raid_secs;
        c.min_account_age_days = body.min_account_age_days;
        c.raid_action = raid_action;
    })
    .await?;

    audit(actor, gid, "automod", "put");
    Ok(Json(read_automod(&state, gid).await?))
}

// ---- giveaways (read only) -------------------------------------------------

#[derive(Serialize)]
struct Giveaway {
    id: i64,
    channel_id: String,
    message_id: String,
    prize: String,
    winners: i64,
    host_id: String,
    ends_at: i64,
    ended: bool,
    entries: i64,
}

#[derive(Serialize)]
struct GiveawaysResponse {
    giveaways: Vec<Giveaway>,
}

/// Newest first, with the current entry count for each.
async fn get_giveaways(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<GiveawaysResponse>> {
    let rows = giveaways::Entity::find()
        .filter(giveaways::Column::GuildId.eq(gid as i64))
        .order_by_desc(giveaways::Column::EndsAt)
        .limit(50)
        .all(state.servers_orm())
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for g in rows {
        let entries = giveaway_entries::Entity::find()
            .filter(giveaway_entries::Column::GiveawayId.eq(g.id))
            .count(state.servers_orm())
            .await? as i64;
        out.push(Giveaway {
            id: g.id,
            channel_id: id_to_string(g.channel_id),
            message_id: id_to_string(g.message_id),
            prize: g.prize,
            winners: g.winners,
            host_id: id_to_string(g.host_id),
            ends_at: g.ends_at,
            ended: g.ended,
            entries,
        });
    }
    Ok(Json(GiveawaysResponse { giveaways: out }))
}
