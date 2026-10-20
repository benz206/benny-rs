//! A guild's channels and roles, read from serenity's cache, so the dashboard
//! can offer names instead of raw ids.

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use serenity::all::ChannelType;

use crate::http::auth::GuildScope;
use crate::http::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new().route("/guilds/{gid}/directory", get(get_directory))
}

#[derive(Serialize)]
struct Channel {
    id: String,
    name: String,
    kind: &'static str,
    parent_id: Option<String>,
    position: u16,
}

#[derive(Serialize)]
struct Role {
    id: String,
    name: String,
    color: u32,
    position: u16,
    managed: bool,
}

#[derive(Serialize)]
struct Directory {
    channels: Vec<Channel>,
    roles: Vec<Role>,
}

fn kind_name(kind: ChannelType) -> &'static str {
    match kind {
        ChannelType::Text => "text",
        ChannelType::News => "announcement",
        ChannelType::Voice => "voice",
        ChannelType::Stage => "stage",
        ChannelType::Category => "category",
        ChannelType::Forum => "forum",
        _ => "other",
    }
}

async fn get_directory(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> ApiResult<Json<Directory>> {
    let cache = state.discord_cache.get().ok_or(ApiError::NotFound)?;
    let guild = cache.guild(gid).ok_or(ApiError::NotFound)?;

    let mut channels: Vec<Channel> = guild
        .channels
        .values()
        .map(|c| Channel {
            id: c.id.get().to_string(),
            name: c.name.clone(),
            kind: kind_name(c.kind),
            parent_id: c.parent_id.map(|p| p.get().to_string()),
            position: c.position,
        })
        .collect();
    channels.sort_by_key(|c| c.position);

    let mut roles: Vec<Role> = guild
        .roles
        .values()
        .filter(|r| r.id.get() != gid)
        .map(|r| Role {
            id: r.id.get().to_string(),
            name: r.name.clone(),
            color: r.colour.0,
            position: r.position,
            managed: r.managed,
        })
        .collect();
    roles.sort_by_key(|r| std::cmp::Reverse(r.position));

    Ok(Json(Directory { channels, roles }))
}
