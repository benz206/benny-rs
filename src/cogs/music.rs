//! Music playback cog backed by Lavalink (via `lavalink-rs` 0.15, no songbird).
//!
//! Voice joining is done by forwarding a raw VOICE_STATE_UPDATE (gateway opcode 4)
//! through serenity's shard, then asking `lavalink-rs` for the resulting
//! connection info (it is fed the `voice_server_update` / `voice_state_update`
//! events from `main.rs`). Commands are dispatched from `on_command`.

use crate::cogs::Cog;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

use serenity::all::{ChannelId, CreateEmbed, CreateMessage, GuildId, Http, UserId};

use lavalink_rs::client::LavalinkClient;
use lavalink_rs::hook;
use lavalink_rs::model::client::NodeDistributionStrategy;
use lavalink_rs::model::events;
use lavalink_rs::model::track::{TrackData, TrackLoadData};
use lavalink_rs::node::NodeBuilder;
use lavalink_rs::player_context::{PlayerContext, TrackInQueue};

/// Per-player user data: where to announce "now playing", and an http handle to do it.
type PlayerData = (ChannelId, Arc<Http>);

/// Upper bound on queued tracks per guild, so repeated large-playlist loads
/// can't grow the in-memory queue without limit.
const MAX_QUEUE: usize = 500;

// ===========================================================================
// Errors
// ===========================================================================

enum MusicError {
    QueueEmpty,
    NothingPlaying,
    NotConnected,
    NotInVoice,
    DifferentVoice,
    QueueFull,
    TrackNotFound,
    NotReady,
    Failed(String),
}

impl MusicError {
    fn message(&self) -> String {
        match self {
            MusicError::QueueEmpty => "The queue is empty.".to_string(),
            MusicError::NothingPlaying => "Nothing is playing right now.".to_string(),
            MusicError::NotConnected => "I'm not connected to a voice channel.".to_string(),
            MusicError::NotInVoice => "You need to be in a voice channel to use that.".to_string(),
            MusicError::DifferentVoice => {
                "You need to be in my voice channel to use that.".to_string()
            }
            MusicError::QueueFull => "The queue is full.".to_string(),
            MusicError::TrackNotFound => "No tracks found for that query.".to_string(),
            MusicError::NotReady => {
                "The music system isn't ready yet. Try again in a moment.".to_string()
            }
            MusicError::Failed(why) => why.clone(),
        }
    }
}

// ===========================================================================
// Lavalink client construction + event hooks (called from main.rs at ready)
// ===========================================================================

/// Build and connect a `LavalinkClient` using the bot config. Called once at
/// `ready`. Never panics if the Lavalink server is down: the node simply keeps
/// retrying in the background.
pub async fn connect_lavalink(state: &Arc<AppState>, user_id: UserId) -> LavalinkClient {
    let cfg = &state.config.lavalink;

    let events = events::Events {
        ready: Some(ready_event),
        track_start: Some(track_start),
        track_end: Some(track_end),
        ..Default::default()
    };

    let node = NodeBuilder {
        hostname: format!("{}:{}", cfg.host, cfg.port),
        is_ssl: false,
        events: events::Events::default(),
        password: cfg.password.clone(),
        user_id: user_id.into(),
        session_id: None,
    };

    LavalinkClient::new(events, vec![node], NodeDistributionStrategy::round_robin()).await
}

#[hook]
async fn ready_event(client: LavalinkClient, _session_id: String, _event: &events::Ready) {
    // Clear any stale players left over from a Lavalink restart.
    let _ = client.delete_all_player_contexts().await;
}

#[hook]
async fn track_start(client: LavalinkClient, _session_id: String, event: &events::TrackStart) {
    let Some(player) = client.get_player_context(event.guild_id) else {
        return;
    };
    let Ok(data) = player.data::<PlayerData>() else {
        return;
    };
    let (channel_id, http) = (&data.0, &data.1);

    let embed = now_playing_embed(&event.track, "Now Playing");
    let _ = channel_id
        .send_message(http, CreateMessage::new().embed(embed))
        .await;
}

#[hook]
async fn track_end(_client: LavalinkClient, _session_id: String, _event: &events::TrackEnd) {
    // No-op: lavalink-rs auto-advances the built-in queue on track end.
}

// ===========================================================================
// Embed / formatting helpers
// ===========================================================================

fn fmt_duration(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn now_playing_embed(track: &TrackData, title: &str) -> CreateEmbed {
    let info = &track.info;
    let mut embed = CreateEmbed::new()
        .title(title)
        .description(format!("**{}**", info.title))
        .color(colors::BLURPLE)
        .field("Author", info.author.as_str(), true);

    if info.is_stream {
        embed = embed.field("Duration", "🔴 LIVE", true);
    } else {
        embed = embed.field("Duration", fmt_duration(info.length), true);
    }

    if let Some(uri) = &info.uri {
        embed = embed.url(uri.as_str());
    }
    if let Some(art) = &info.artwork_url {
        embed = embed.thumbnail(art.as_str());
    }
    if let Some(rid) = requester_id(track) {
        embed = embed.field("Requested by", format!("<@{rid}>"), true);
    }
    embed
}

fn requester_id(track: &TrackData) -> Option<u64> {
    track
        .user_data
        .as_ref()
        .and_then(|d| d.get("requester_id"))
        .and_then(|v| v.as_u64())
}

// ===========================================================================
// Cog
// ===========================================================================

pub struct MusicCog {
    #[allow(dead_code)]
    state: Arc<AppState>,
}

impl MusicCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for MusicCog {}

// ===========================================================================
// Command surface
// ===========================================================================

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        play(),
        disconnect(),
        pause(),
        resume(),
        skip(),
        queue(),
        nowplaying(),
        volume(),
        stop(),
    ]
}

// ===========================================================================
// Free helpers (voice + player)
// ===========================================================================

/// Look up the voice channel the user is currently connected to, via the
/// cache. Scoped so the cache reference is dropped before any await.
fn user_voice_channel(
    sctx: &serenity::all::Context,
    guild_id: GuildId,
    user_id: UserId,
) -> Option<ChannelId> {
    let guild = sctx.cache.guild(guild_id)?;
    guild
        .voice_states
        .get(&user_id)
        .and_then(|vs| vs.channel_id)
}

/// Require the invoker to share the bot's voice channel before letting them
/// drive playback (skip/stop/pause/disconnect/volume) — otherwise any member
/// who can type could control or kick the bot from a channel they're not in.
fn require_same_voice(
    sctx: &serenity::all::Context,
    guild_id: GuildId,
    user_id: UserId,
) -> Result<(), MusicError> {
    let bot_id = sctx.cache.current_user().id;
    let bot_vc =
        user_voice_channel(sctx, guild_id, bot_id).ok_or(MusicError::NotConnected)?;
    match user_voice_channel(sctx, guild_id, user_id) {
        Some(vc) if vc == bot_vc => Ok(()),
        _ => Err(MusicError::DifferentVoice),
    }
}

/// Whether a raw URL may be handed to Lavalink to load. Lavalink fetches
/// URLs server-side, so an unrestricted URL is an SSRF vector (internal
/// hosts, link-local metadata, etc.); only known media hosts are allowed.
fn is_allowed_media_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    const ALLOWED: &[&str] = &[
        "youtube.com",
        "youtu.be",
        "soundcloud.com",
        "spotify.com",
        "bandcamp.com",
        "twitch.tv",
        "vimeo.com",
        "deezer.com",
        "music.apple.com",
        "nicovideo.jp",
    ];
    ALLOWED
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Send a raw gateway voice-state-update (opcode 4). `channel` = None leaves.
fn send_voice_state(
    sctx: &serenity::all::Context,
    guild_id: GuildId,
    channel: Option<ChannelId>,
) {
    let payload = serde_json::json!({
        "op": 4,
        "d": {
            "guild_id": guild_id.get().to_string(),
            "channel_id": channel.map(|c| c.get().to_string()),
            "self_mute": false,
            "self_deaf": false,
        }
    })
    .to_string();
    sctx.shard
        .websocket_message(tokio_tungstenite::tungstenite::Message::Text(payload));
}

/// Join `voice_channel`, creating a Lavalink player bound to `text_channel`.
async fn join(
    sctx: &serenity::all::Context,
    state: &AppState,
    guild_id: GuildId,
    voice_channel: ChannelId,
    text_channel: ChannelId,
) -> Result<(), MusicError> {
    let lava = state.lavalink().ok_or(MusicError::NotReady)?;

    send_voice_state(sctx, guild_id, Some(voice_channel));

    let connection_info = lava
        .get_connection_info(guild_id, Duration::from_secs(10))
        .await
        .map_err(|e| MusicError::Failed(format!("Failed to join voice channel: {e}")))?;

    lava.create_player_context_with_data::<PlayerData>(
        guild_id,
        connection_info,
        Arc::new((text_channel, sctx.http.clone())),
    )
    .await
    .map_err(|e| MusicError::Failed(format!("Failed to start the player: {e}")))?;

    Ok(())
}

/// Resolve an existing player context, mapping the absence to a user error.
fn player_ctx(state: &AppState, guild_id: GuildId) -> Result<PlayerContext, MusicError> {
    let lava = state.lavalink().ok_or(MusicError::NotReady)?;
    lava.get_player_context(guild_id)
        .ok_or(MusicError::NotConnected)
}

// ===========================================================================
// Commands
// ===========================================================================

/// Play a song or add it to the queue. Joins your voice channel if needed.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Music",
    aliases("p")
)]
async fn play(
    ctx: Context<'_>,
    #[description = "Search query or URL"]
    #[rest]
    query: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if query.is_empty() {
        return send_error(ctx, "Please provide a search query or URL.").await;
    }
    let Some(lava) = state.lavalink() else {
        return send_error(ctx, &MusicError::NotReady.message()).await;
    };

    // The invoker must be in a voice channel; join it if we're not already in one.
    if lava.get_player_context(guild_id).is_none() {
        let Some(voice) = user_voice_channel(sctx, guild_id, ctx.author().id) else {
            return send_error(ctx, &MusicError::NotInVoice.message()).await;
        };
        if let Err(e) = join(sctx, state, guild_id, voice, ctx.channel_id()).await {
            return send_error(ctx, &e.message()).await;
        }
    }

    let Some(player) = lava.get_player_context(guild_id) else {
        return send_error(ctx, &MusicError::NotConnected.message()).await;
    };

    // URLs from known media hosts (incl. Spotify, via the LavaSrc plugin) are
    // passed through verbatim; anything else — bare terms, and crucially any
    // non-allowlisted URL — goes through the search source so Lavalink never
    // fetches an arbitrary URL server-side (SSRF).
    let is_url = query.starts_with("http://") || query.starts_with("https://");
    let search_query = if is_url && is_allowed_media_url(&query) {
        query.clone()
    } else {
        format!("{}:{}", state.config.lavalink.search_source, query)
    };

    let loaded = match lava.load_tracks(guild_id, &search_query).await {
        Ok(t) => t,
        Err(e) => return send_error(ctx, &format!("Failed to load tracks: {e}")).await,
    };

    let mut tracks: Vec<TrackInQueue> = match loaded.data {
        Some(TrackLoadData::Track(t)) => vec![t.into()],
        Some(TrackLoadData::Search(list)) => match list.into_iter().next() {
            Some(t) => vec![t.into()],
            None => return send_error(ctx, &MusicError::TrackNotFound.message()).await,
        },
        Some(TrackLoadData::Playlist(pl)) => pl.tracks.into_iter().map(Into::into).collect(),
        _ => return send_error(ctx, &MusicError::TrackNotFound.message()).await,
    };

    if tracks.is_empty() {
        return send_error(ctx, &MusicError::TrackNotFound.message()).await;
    }

    // Tag every track with the requester for queue / now-playing displays.
    for t in &mut tracks {
        t.track.user_data =
            Some(serde_json::json!({ "requester_id": ctx.author().id.get() }));
    }

    let added = tracks.len();
    let first = tracks[0].track.info.clone();

    let queue = player.get_queue();
    // Bound the in-memory queue so repeated large-playlist loads can't grow
    // it without limit and exhaust memory.
    let current = queue.get_count().await.unwrap_or(0);
    if current + added > MAX_QUEUE {
        return send_error(ctx, &MusicError::QueueFull.message()).await;
    }
    if let Err(e) = queue.append(tracks.into()) {
        return send_error(ctx, &format!("Failed to queue tracks: {e}")).await;
    }

    // If nothing is playing, kick off the first queued track.
    if let Ok(data) = player.get_player().await {
        if data.track.is_none() && queue.get_track(0).await.is_ok_and(|x| x.is_some()) {
            let _ = player.skip();
        }
    }

    let desc = if added > 1 {
        format!("Added **{added}** tracks to the queue.")
    } else {
        format!("Added **{} - {}** to the queue.", first.author, first.title)
    };
    send_embed(ctx, embeds::success_embed("Queued", &desc)).await
}

/// Disconnect the bot from the voice channel and stop playback.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Music",
    aliases("dc", "leave")
)]
async fn disconnect(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if let Err(e) = require_same_voice(sctx, guild_id, ctx.author().id) {
        return send_error(ctx, &e.message()).await;
    }
    let Some(lava) = state.lavalink() else {
        return send_error(ctx, &MusicError::NotReady.message()).await;
    };
    if lava.get_player_context(guild_id).is_none() {
        return send_error(ctx, &MusicError::NotConnected.message()).await;
    }
    let _ = lava.delete_player(guild_id).await;
    send_voice_state(sctx, guild_id, None);
    send_embed(ctx, embeds::success_embed("Disconnected", "Left the voice channel.")).await
}

/// Pause playback.
#[poise::command(slash_command, prefix_command, guild_only, category = "Music")]
async fn pause(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if let Err(e) = require_same_voice(sctx, guild_id, ctx.author().id) {
        return send_error(ctx, &e.message()).await;
    }
    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };
    match player.get_player().await {
        Ok(data) if data.track.is_some() => {
            let _ = player.set_pause(true).await;
            send_embed(ctx, embeds::success_embed("Paused", "Playback paused.")).await
        }
        _ => send_error(ctx, &MusicError::NothingPlaying.message()).await,
    }
}

/// Resume paused playback.
#[poise::command(slash_command, prefix_command, guild_only, category = "Music")]
async fn resume(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if let Err(e) = require_same_voice(sctx, guild_id, ctx.author().id) {
        return send_error(ctx, &e.message()).await;
    }
    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };
    match player.get_player().await {
        Ok(data) if data.track.is_some() => {
            let _ = player.set_pause(false).await;
            send_embed(ctx, embeds::success_embed("Resumed", "Playback resumed.")).await
        }
        _ => send_error(ctx, &MusicError::NothingPlaying.message()).await,
    }
}

/// Skip the current track.
#[poise::command(slash_command, prefix_command, guild_only, category = "Music")]
async fn skip(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if let Err(e) = require_same_voice(sctx, guild_id, ctx.author().id) {
        return send_error(ctx, &e.message()).await;
    }
    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };
    match player.get_player().await {
        Ok(data) => match data.track {
            Some(track) => {
                let _ = player.skip();
                send_embed(
                    ctx,
                    embeds::success_embed("Skipped", &format!("Skipped **{}**.", track.info.title)),
                )
                .await
            }
            None => send_error(ctx, &MusicError::NothingPlaying.message()).await,
        },
        Err(_) => send_error(ctx, &MusicError::NothingPlaying.message()).await,
    }
}

/// Show the current queue.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Music",
    aliases("q")
)]
async fn queue(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;

    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };

    let now_playing = match player.get_player().await {
        Ok(data) => data.track,
        Err(_) => None,
    };
    let tracks = player.get_queue().get_queue().await.unwrap_or_default();

    if now_playing.is_none() && tracks.is_empty() {
        return send_error(ctx, &MusicError::QueueEmpty.message()).await;
    }

    let mut description = String::new();
    if let Some(track) = &now_playing {
        description.push_str(&format!(
            "**Now playing:** {} - {}\n\n",
            track.info.author, track.info.title
        ));
    }

    if tracks.is_empty() {
        description.push_str("*The up-next queue is empty.*");
    } else {
        for (idx, item) in tracks.iter().take(10).enumerate() {
            let info = &item.track.info;
            description.push_str(&format!(
                "`{}.` {} - {}\n",
                idx + 1,
                info.author,
                info.title
            ));
        }
        if tracks.len() > 10 {
            description.push_str(&format!("\n*…and {} more.*", tracks.len() - 10));
        }
    }

    let embed = CreateEmbed::new()
        .title("Queue")
        .description(description)
        .color(colors::BLURPLE);
    send_embed(ctx, embed).await
}

/// Show what's currently playing.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Music",
    aliases("np")
)]
async fn nowplaying(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;

    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };
    match player.get_player().await {
        Ok(data) => match data.track {
            Some(track) => {
                let mut embed = now_playing_embed(&track, "Now Playing");
                if !track.info.is_stream {
                    embed = embed.field(
                        "Position",
                        format!(
                            "{} / {}",
                            fmt_duration(data.state.position),
                            fmt_duration(track.info.length)
                        ),
                        false,
                    );
                }
                send_embed(ctx, embed).await
            }
            None => send_error(ctx, &MusicError::NothingPlaying.message()).await,
        },
        Err(_) => send_error(ctx, &MusicError::NothingPlaying.message()).await,
    }
}

/// Set the playback volume (1–100).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Music",
    aliases("vol")
)]
async fn volume(
    ctx: Context<'_>,
    #[description = "Volume level (1-100)"]
    #[min = 1]
    #[max = 100]
    level: u8,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if let Err(e) = require_same_voice(sctx, guild_id, ctx.author().id) {
        return send_error(ctx, &e.message()).await;
    }
    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };
    let _ = player.set_volume(level as u16).await;
    send_embed(
        ctx,
        embeds::success_embed("Volume", &format!("Volume set to **{level}%**.")),
    )
    .await
}

/// Stop playback and clear the queue.
#[poise::command(slash_command, prefix_command, guild_only, category = "Music")]
async fn stop(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    if let Err(e) = require_same_voice(sctx, guild_id, ctx.author().id) {
        return send_error(ctx, &e.message()).await;
    }
    let player = match player_ctx(state, guild_id) {
        Ok(p) => p,
        Err(e) => return send_error(ctx, &e.message()).await,
    };
    let _ = player.get_queue().clear();
    let _ = player.stop_now().await;
    send_embed(
        ctx,
        embeds::success_embed("Stopped", "Stopped playback and cleared the queue."),
    )
    .await
}
