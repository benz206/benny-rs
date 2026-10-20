//! Moderation views: the case log, with reason edits and deletes, and active
//! timed infractions, which can be lifted early.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};

use super::{Pagination, audit, id_to_string};
use crate::entities::{mod_cases, mod_timed};
use crate::http::auth::{Actor, GuildScope};
use crate::http::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/guilds/{gid}/cases", get(list_cases))
        .route(
            "/guilds/{gid}/cases/{number}",
            patch(patch_case).delete(delete_case),
        )
        .route("/guilds/{gid}/mutes", get(list_mutes))
        .route("/guilds/{gid}/mutes/{number}/lift", post(lift_mute))
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

fn case_response(c: mod_cases::Model) -> CaseResponse {
    CaseResponse {
        case_number: c.case_number,
        action_type: c.action_type,
        target_id: id_to_string(c.target_id),
        moderator_id: id_to_string(c.moderator_id),
        reason: c.reason,
        created_at: c.created_at,
        active: c.active,
        expires_at: c.expires_at,
    }
}

async fn list_cases(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    Query(p): Query<Pagination>,
) -> ApiResult<Json<CasesResponse>> {
    let (limit, offset) = p.resolve();

    let rows = mod_cases::Entity::find()
        .filter(mod_cases::Column::GuildId.eq(gid as i64))
        .order_by_desc(mod_cases::Column::CaseNumber)
        .limit(limit)
        .offset(offset)
        .all(state.servers_orm())
        .await?;

    let cases = rows.into_iter().map(case_response).collect();

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

#[derive(Deserialize)]
struct PatchCase {
    reason: String,
}

async fn patch_case(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Path((_, number)): Path<(String, i64)>,
    Json(body): Json<PatchCase>,
) -> ApiResult<Json<CaseResponse>> {
    let reason = body.reason.trim();
    if reason.is_empty() {
        return Err(ApiError::bad_request("reason must not be empty"));
    }

    let res = mod_cases::Entity::update_many()
        .col_expr(mod_cases::Column::Reason, Expr::value(reason))
        .filter(mod_cases::Column::GuildId.eq(gid as i64))
        .filter(mod_cases::Column::CaseNumber.eq(number))
        .exec(state.servers_orm())
        .await?;
    if res.rows_affected == 0 {
        return Err(ApiError::NotFound);
    }

    let case = mod_cases::Entity::find_by_id((gid as i64, number))
        .one(state.servers_orm())
        .await?
        .ok_or(ApiError::NotFound)?;
    audit(actor, gid, "cases", "patch");
    Ok(Json(case_response(case)))
}

async fn delete_case(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Path((_, number)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let res = mod_cases::Entity::delete_by_id((gid as i64, number))
        .exec(state.servers_orm())
        .await?;
    if res.rows_affected == 0 {
        return Err(ApiError::NotFound);
    }
    mod_timed::Entity::delete_by_id((gid as i64, number))
        .exec(state.servers_orm())
        .await?;

    audit(actor, gid, "cases", "delete");
    Ok(StatusCode::NO_CONTENT)
}

async fn lift_mute(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Path((_, number)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let row = mod_timed::Entity::find_by_id((gid as i64, number))
        .one(state.servers_orm())
        .await?
        .ok_or(ApiError::NotFound)?;
    if row.action != "mute" && row.action != "ban" {
        return Err(ApiError::conflict(
            "only mutes and temp-bans can be lifted here; timeouts are cleared in Discord",
        ));
    }

    mod_timed::Entity::update_many()
        .col_expr(mod_timed::Column::ExpiresAt, Expr::value(Utc::now().timestamp()))
        .filter(mod_timed::Column::GuildId.eq(gid as i64))
        .filter(mod_timed::Column::CaseNumber.eq(number))
        .exec(state.servers_orm())
        .await?;

    audit(actor, gid, "mutes", "lift");
    Ok(StatusCode::ACCEPTED)
}
