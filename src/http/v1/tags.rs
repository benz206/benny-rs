//! Tags CRUD. Writes update the DB and `tag_cache` together, mirroring
//! `cogs::tags`. Validation caps (`MAX_TAG_NAME_LEN`, `MAX_TAG_CONTENT_LEN`,
//! reserved names, charset) are the cog's, reused verbatim.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, Set};
use serde::{Deserialize, Serialize};

use super::{audit, id_to_string};
use crate::entities::tags;
use crate::http::auth::{Actor, GuildScope};
use crate::http::error::{ApiError, ApiResult};
use crate::state::{AppState, Tag};

// Mirrored from cogs::tags.
const RESERVED_NAMES: &[&str] = &["tag", "tagtest", "tt", "playground", "testtag"];
const MAX_TAG_NAME_LEN: usize = 32;
const MAX_TAG_CONTENT_LEN: usize = 2000;
const MAX_LIMIT: u64 = 100;

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/guilds/{gid}/tags", get(list_tags).post(create_tag))
        .route(
            "/guilds/{gid}/tags/{name}",
            get(get_tag).patch(patch_tag).delete(delete_tag),
        )
}

#[derive(Serialize)]
struct TagResponse {
    name: String,
    content: String,
    owner_id: String,
    uses: i64,
    created_at: i64,
}

impl TagResponse {
    fn from_model(m: tags::Model) -> Self {
        Self {
            name: m.name,
            content: m.content,
            owner_id: id_to_string(m.owner_id),
            uses: m.uses,
            created_at: m.created_at,
        }
    }
}

#[derive(Serialize)]
struct TagList {
    tags: Vec<TagResponse>,
}

#[derive(Deserialize)]
struct CreateTag {
    name: String,
    content: String,
}

#[derive(Deserialize)]
struct PatchTag {
    content: String,
}

async fn list_tags(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<TagList>> {
    let rows = tags::Entity::find()
        .filter(tags::Column::GuildId.eq(gid as i64))
        .limit(MAX_LIMIT)
        .all(state.servers_orm())
        .await?;
    Ok(Json(TagList {
        tags: rows.into_iter().map(TagResponse::from_model).collect(),
    }))
}

async fn create_tag(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<CreateTag>,
) -> ApiResult<impl IntoResponse> {
    let name = body.name.trim().to_lowercase();
    validate_name(&name)?;
    let content = body.content.trim().to_string();
    validate_content(&content)?;

    let owner_id = actor.0 as i64;
    let created_at = chrono::Utc::now().timestamp();

    let res = tags::Entity::insert(tags::ActiveModel {
        guild_id: Set(gid as i64),
        name: Set(name.clone()),
        content: Set(content.clone()),
        owner_id: Set(owner_id),
        uses: Set(0),
        created_at: Set(created_at),
    })
    .on_conflict(
        OnConflict::columns([tags::Column::GuildId, tags::Column::Name])
            .do_nothing()
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;
    match res {
        Ok(_) => {}
        Err(DbErr::RecordNotInserted) => {
            return Err(ApiError::conflict(format!("tag '{name}' already exists")));
        }
        Err(e) => return Err(e.into()),
    }

    state
        .tag_cache
        .entry(gid)
        .or_insert_with(HashMap::new)
        .insert(
            name.clone(),
            Tag {
                name: name.clone(),
                content: content.clone(),
                owner_id,
                uses: 0,
                created_at,
            },
        );
    audit(actor, gid, "tags", "create");

    Ok((
        StatusCode::CREATED,
        Json(TagResponse {
            name,
            content,
            owner_id: id_to_string(owner_id),
            uses: 0,
            created_at,
        }),
    ))
}

async fn get_tag(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    Path((_gid, name)): Path<(u64, String)>,
) -> ApiResult<Json<TagResponse>> {
    let name = name.trim().to_lowercase();
    let m = tags::Entity::find_by_id((gid as i64, name))
        .one(state.servers_orm())
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(TagResponse::from_model(m)))
}

async fn patch_tag(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    Path((_gid, name)): Path<(u64, String)>,
    actor: Actor,
    Json(body): Json<PatchTag>,
) -> ApiResult<Json<TagResponse>> {
    let name = name.trim().to_lowercase();
    let content = body.content.trim().to_string();
    validate_content(&content)?;

    let existing = tags::Entity::find_by_id((gid as i64, name.clone()))
        .one(state.servers_orm())
        .await?
        .ok_or(ApiError::NotFound)?;

    tags::Entity::update_many()
        .col_expr(tags::Column::Content, Expr::value(content.clone()))
        .filter(tags::Column::GuildId.eq(gid as i64))
        .filter(tags::Column::Name.eq(name.as_str()))
        .exec(state.servers_orm())
        .await?;
    if let Some(mut gt) = state.tag_cache.get_mut(&gid)
        && let Some(t) = gt.get_mut(&name)
    {
        t.content = content.clone();
    }
    audit(actor, gid, "tags", "patch");

    Ok(Json(TagResponse {
        name,
        content,
        owner_id: id_to_string(existing.owner_id),
        uses: existing.uses,
        created_at: existing.created_at,
    }))
}

async fn delete_tag(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    Path((_gid, name)): Path<(u64, String)>,
    actor: Actor,
) -> ApiResult<StatusCode> {
    let name = name.trim().to_lowercase();
    let res = tags::Entity::delete_many()
        .filter(tags::Column::GuildId.eq(gid as i64))
        .filter(tags::Column::Name.eq(name.as_str()))
        .exec(state.servers_orm())
        .await?;
    if res.rows_affected == 0 {
        return Err(ApiError::NotFound);
    }
    if let Some(mut gt) = state.tag_cache.get_mut(&gid) {
        gt.remove(&name);
    }
    audit(actor, gid, "tags", "delete");
    Ok(StatusCode::NO_CONTENT)
}

fn validate_name(name: &str) -> ApiResult<()> {
    if RESERVED_NAMES.contains(&name) {
        return Err(ApiError::bad_request(format!(
            "'{name}' is a reserved tag name"
        )));
    }
    if name.is_empty() {
        return Err(ApiError::bad_request("tag name must not be empty"));
    }
    if name.chars().count() > MAX_TAG_NAME_LEN
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(ApiError::bad_request(format!(
            "tag name must be at most {MAX_TAG_NAME_LEN} characters and contain only letters, numbers, '_' or '-'"
        )));
    }
    Ok(())
}

fn validate_content(content: &str) -> ApiResult<()> {
    if content.is_empty() {
        return Err(ApiError::bad_request("tag content must not be empty"));
    }
    if content.chars().count() > MAX_TAG_CONTENT_LEN {
        return Err(ApiError::bad_request(format!(
            "tag content must be at most {MAX_TAG_CONTENT_LEN} characters"
        )));
    }
    Ok(())
}
