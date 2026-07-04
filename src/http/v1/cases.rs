//! Read-only moderation views: paginated case log and active mutes.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};

use super::id_to_string;
use crate::entities::{mod_cases, mod_timed};
use crate::http::auth::GuildScope;
use crate::http::error::ApiResult;
use crate::state::AppState;

const DEFAULT_LIMIT: u64 = 25;
const MAX_LIMIT: u64 = 100;

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/guilds/{gid}/cases", get(list_cases))
        .route("/guilds/{gid}/mutes", get(list_mutes))
}

#[derive(Deserialize)]
struct Pagination {
    limit: Option<u64>,
    offset: Option<u64>,
}

#[derive(Serialize)]
struct CaseResponse {
    case_number: i64,
    action_type: String,
    target_id: String,
    moderator_id: String,
    reason: String,
    created_at: i64,
    active: bool,
    expires_at: Option<i64>,
}

#[derive(Serialize)]
struct CasesResponse {
    cases: Vec<CaseResponse>,
    limit: u64,
    offset: u64,
}

async fn list_cases(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    Query(p): Query<Pagination>,
) -> ApiResult<Json<CasesResponse>> {
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = p.offset.unwrap_or(0);

    let rows = mod_cases::Entity::find()
        .filter(mod_cases::Column::GuildId.eq(gid as i64))
        .order_by_desc(mod_cases::Column::CaseNumber)
        .limit(limit)
        .offset(offset)
        .all(state.servers_orm())
        .await?;

    let cases = rows
        .into_iter()
        .map(|c| CaseResponse {
            case_number: c.case_number,
            action_type: c.action_type,
            target_id: id_to_string(c.target_id),
            moderator_id: id_to_string(c.moderator_id),
            reason: c.reason,
            created_at: c.created_at,
            active: c.active,
            expires_at: c.expires_at,
        })
        .collect();

    Ok(Json(CasesResponse {
        cases,
        limit,
        offset,
    }))
}

#[derive(Serialize)]
struct MuteResponse {
    case_number: i64,
    user_id: String,
    action: String,
    expires_at: i64,
}

#[derive(Serialize)]
struct MutesResponse {
    mutes: Vec<MuteResponse>,
}

async fn list_mutes(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<MutesResponse>> {
    let rows = mod_timed::Entity::find()
        .filter(mod_timed::Column::GuildId.eq(gid as i64))
        .filter(mod_timed::Column::Action.eq("mute"))
        .order_by_asc(mod_timed::Column::CaseNumber)
        .all(state.servers_orm())
        .await?;

    let mutes = rows
        .into_iter()
        .map(|m| MuteResponse {
            case_number: m.case_number,
            user_id: id_to_string(m.user_id),
            action: m.action,
            expires_at: m.expires_at,
        })
        .collect();

    Ok(Json(MutesResponse { mutes }))
}
