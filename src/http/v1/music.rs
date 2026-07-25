//! Live music playback: what the bot is playing right now, plus the transport
//! controls the `/music` commands expose.
//!
//! Unlike every other resource here there is no database behind this — the
//! authoritative state lives in the Lavalink player context for the guild, so
//! reads go straight to `lavalink-rs` and writes are fire-and-forget commands
//! to the node. A guild with no player is not an error: `connected` is simply
//! `false`.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post, put},
};
use lavalink_rs::model::track::TrackData;
use lavalink_rs::player_context::PlayerContext;
use serde::{Deserialize, Serialize};
use serenity::all::GuildId;

use super::audit;
use crate::cogs::music::requester_id;
use crate::http::auth::{Actor, GuildScope};
use crate::http::error::{ApiError, ApiResult};
use crate::state::AppState;

/// How many up-next tracks a response carries. The in-memory queue holds up to
/// `cogs::music::MAX_QUEUE` (500); serialising all of them would make for a
/// needlessly large payload, so the rest is summarised by `queue_length`.
const MAX_QUEUE_PREVIEW: usize = 100;

/// Volume bounds mirrored from `cogs::music::volume`.
const MIN_VOLUME: u16 = 1;
const MAX_VOLUME: u16 = 100;

pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/guilds/{gid}/music", get(get_music))
        .route("/guilds/{gid}/music/pause", post(post_pause))
        .route("/guilds/{gid}/music/skip", post(post_skip))
        .route("/guilds/{gid}/music/stop", post(post_stop))
        .route("/guilds/{gid}/music/volume", put(put_volume))
}

// ---- wire types ------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct Track {
    title: String,
    author: String,
    uri: Option<String>,
    artwork_url: Option<String>,
    length_ms: u64,
    is_stream: bool,
    source: String,
    requester_id: Option<String>,
}

impl From<&TrackData> for Track {
    fn from(t: &TrackData) -> Self {
        let info = &t.info;
        Self {
            title: info.title.clone(),
            author: info.author.clone(),
            uri: info.uri.clone(),
            artwork_url: info.artwork_url.clone(),
            length_ms: info.length,
            is_stream: info.is_stream,
            source: info.source_name.clone(),
            requester_id: requester_id(t).map(|id| id.to_string()),
        }
    }
}

#[derive(Serialize)]
pub(super) struct MusicState {
    /// Whether the bot has a Lavalink node at all. `false` means the music
    /// module is unavailable process-wide, not just for this guild.
    available: bool,
    /// Whether a player exists for this guild (i.e. the bot is in a voice
    /// channel here).
    connected: bool,
    playing: bool,
    paused: bool,
    volume: u16,
    position_ms: u64,
    now_playing: Option<Track>,
    /// The first [`MAX_QUEUE_PREVIEW`] up-next tracks.
    queue: Vec<Track>,
    /// Full up-next length, which may exceed `queue.len()`.
    queue_length: usize,
}

impl MusicState {
    fn idle(available: bool) -> Self {
        Self {
            available,
            connected: false,
            playing: false,
            paused: false,
            volume: 0,
            position_ms: 0,
            now_playing: None,
            queue: Vec::new(),
            queue_length: 0,
        }
    }
}

// ---- reads -----------------------------------------------------------------

/// Snapshot the guild's player. Returns the idle state when the node is down or
/// the bot isn't connected here — both are ordinary states, not failures.
pub(super) async fn read_state(state: &AppState, gid: u64) -> MusicState {
    let Some(lava) = state.lavalink() else {
        return MusicState::idle(false);
    };
    let Some(player) = lava.get_player_context(GuildId::new(gid)) else {
        return MusicState::idle(true);
    };
    let Ok(data) = player.get_player().await else {
        return MusicState::idle(true);
    };

    let queued = player.get_queue().get_queue().await.unwrap_or_default();
    let queue = queued
        .iter()
        .take(MAX_QUEUE_PREVIEW)
        .map(|t| Track::from(&t.track))
        .collect();

    MusicState {
        available: true,
        connected: true,
        playing: data.track.is_some() && !data.paused,
        paused: data.paused,
        volume: data.volume,
        position_ms: data.state.position,
        now_playing: data.track.as_ref().map(Track::from),
        queue,
        queue_length: queued.len(),
    }
}

async fn get_music(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
) -> Json<MusicState> {
    Json(read_state(&state, gid).await)
}

// ---- controls --------------------------------------------------------------

/// Resolve the guild's player, mapping "node down" and "not in voice" onto
/// distinct client-visible errors.
fn player(state: &AppState, gid: u64) -> ApiResult<PlayerContext> {
    let lava = state
        .lavalink()
        .ok_or_else(|| ApiError::conflict("the music system is not available"))?;
    lava.get_player_context(GuildId::new(gid))
        .ok_or_else(|| ApiError::conflict("the bot is not connected to a voice channel"))
}

#[derive(Deserialize)]
struct PauseBody {
    paused: bool,
}

async fn post_pause(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<PauseBody>,
) -> ApiResult<Json<MusicState>> {
    let player = player(&state, gid)?;
    player
        .set_pause(body.paused)
        .await
        .map_err(|_| ApiError::conflict("the player did not accept that command"))?;
    audit(
        actor,
        gid,
        "music",
        if body.paused { "pause" } else { "resume" },
    );
    Ok(Json(read_state(&state, gid).await))
}

async fn post_skip(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
) -> ApiResult<Json<MusicState>> {
    let player = player(&state, gid)?;
    player
        .skip()
        .map_err(|_| ApiError::conflict("the player did not accept that command"))?;
    audit(actor, gid, "music", "skip");
    Ok(Json(read_state(&state, gid).await))
}

/// Stop playback and clear the queue — mirrors `cogs::music::stop`.
async fn post_stop(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
) -> ApiResult<Json<MusicState>> {
    let player = player(&state, gid)?;
    let _ = player.get_queue().clear();
    player
        .stop_now()
        .await
        .map_err(|_| ApiError::conflict("the player did not accept that command"))?;
    audit(actor, gid, "music", "stop");
    Ok(Json(read_state(&state, gid).await))
}

#[derive(Deserialize)]
struct VolumeBody {
    volume: u16,
}

async fn put_volume(
    State(state): State<Arc<AppState>>,
    GuildScope(gid): GuildScope,
    actor: Actor,
    Json(body): Json<VolumeBody>,
) -> ApiResult<Json<MusicState>> {
    if !(MIN_VOLUME..=MAX_VOLUME).contains(&body.volume) {
        return Err(ApiError::bad_request(format!(
            "volume must be between {MIN_VOLUME} and {MAX_VOLUME}"
        )));
    }
    let player = player(&state, gid)?;
    player
        .set_volume(body.volume)
        .await
        .map_err(|_| ApiError::conflict("the player did not accept that command"))?;
    audit(actor, gid, "music", "volume");
    Ok(Json(read_state(&state, gid).await))
}
