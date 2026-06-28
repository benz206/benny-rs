//! Music playback cog backed by Lavalink (via `lavalink-rs` 0.15, no songbird).
//!
//! Voice joining is done by forwarding a raw VOICE_STATE_UPDATE (gateway opcode 4)
//! through serenity's shard, then asking `lavalink-rs` for the resulting
//! connection info (it is fed the `voice_server_update` / `voice_state_update`
//! events from `main.rs`). Commands are dispatched from `on_message`.

use crate::cogs::Cog;
use crate::state::{AppState, CommandInvocation};
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

use serenity::all::{
    ChannelId, Context, CreateEmbed, CreateMessage, GuildId, Http, Message, UserId,
};

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
    state: Arc<AppState>,
}

impl MusicCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    fn lava(&self) -> Option<LavalinkClient> {
        self.state.lavalink()
    }

    /// Resolve an existing player context, mapping the absence to a user error.
    fn player_ctx(&self, guild_id: GuildId) -> Result<PlayerContext, MusicError> {
        let lava = self.lava().ok_or(MusicError::NotReady)?;
        lava.get_player_context(guild_id)
            .ok_or(MusicError::NotConnected)
    }

    async fn send_embed(&self, ctx: &Context, channel: ChannelId, embed: CreateEmbed) {
        let _ = channel
            .send_message(ctx, CreateMessage::new().embed(embed))
            .await;
    }

    async fn send_error(&self, ctx: &Context, channel: ChannelId, err: MusicError) {
        self.send_embed(ctx, channel, embeds::error_embed(&err.message()))
            .await;
    }

    async fn send_success(&self, ctx: &Context, channel: ChannelId, title: &str, desc: &str) {
        self.send_embed(ctx, channel, embeds::success_embed(title, desc))
            .await;
    }

    // --- voice gateway plumbing (no songbird) -----------------------------

    /// Look up the voice channel the user is currently connected to, via the
    /// cache. Scoped so the cache reference is dropped before any await.
    fn user_voice_channel(ctx: &Context, guild_id: GuildId, user_id: UserId) -> Option<ChannelId> {
        let guild = ctx.cache.guild(guild_id)?;
        guild
            .voice_states
            .get(&user_id)
            .and_then(|vs| vs.channel_id)
    }

    /// Require the invoker to share the bot's voice channel before letting them
    /// drive playback (skip/stop/pause/disconnect/volume) — otherwise any member
    /// who can type could control or kick the bot from a channel they're not in.
    fn require_same_voice(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<(), MusicError> {
        let bot_id = ctx.cache.current_user().id;
        let bot_vc =
            Self::user_voice_channel(ctx, guild_id, bot_id).ok_or(MusicError::NotConnected)?;
        match Self::user_voice_channel(ctx, guild_id, user_id) {
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
    fn send_voice_state(ctx: &Context, guild_id: GuildId, channel: Option<ChannelId>) {
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
        ctx.shard
            .websocket_message(tokio_tungstenite::tungstenite::Message::Text(payload));
    }

    /// Join `voice_channel`, creating a Lavalink player bound to `text_channel`.
    async fn join(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        voice_channel: ChannelId,
        text_channel: ChannelId,
    ) -> Result<(), MusicError> {
        let lava = self.lava().ok_or(MusicError::NotReady)?;

        Self::send_voice_state(ctx, guild_id, Some(voice_channel));

        let connection_info = lava
            .get_connection_info(guild_id, Duration::from_secs(10))
            .await
            .map_err(|e| MusicError::Failed(format!("Failed to join voice channel: {e}")))?;

        lava.create_player_context_with_data::<PlayerData>(
            guild_id,
            connection_info,
            Arc::new((text_channel, ctx.http.clone())),
        )
        .await
        .map_err(|e| MusicError::Failed(format!("Failed to start the player: {e}")))?;

        Ok(())
    }

    // --- commands ---------------------------------------------------------

    async fn cmd_play(&self, ctx: &Context, msg: &Message, guild_id: GuildId, query: &str) {
        if query.is_empty() {
            self.send_error(
                ctx,
                msg.channel_id,
                MusicError::Failed("Please provide a search query or URL.".to_string()),
            )
            .await;
            return;
        }
        let Some(lava) = self.lava() else {
            self.send_error(ctx, msg.channel_id, MusicError::NotReady)
                .await;
            return;
        };

        // The invoker must be in a voice channel; join it if we're not already in one.
        if lava.get_player_context(guild_id).is_none() {
            let Some(voice) = Self::user_voice_channel(ctx, guild_id, msg.author.id) else {
                self.send_error(ctx, msg.channel_id, MusicError::NotInVoice)
                    .await;
                return;
            };
            if let Err(e) = self.join(ctx, guild_id, voice, msg.channel_id).await {
                self.send_error(ctx, msg.channel_id, e).await;
                return;
            }
        }

        let Some(player) = lava.get_player_context(guild_id) else {
            self.send_error(ctx, msg.channel_id, MusicError::NotConnected)
                .await;
            return;
        };

        // URLs from known media hosts (incl. Spotify, via the LavaSrc plugin) are
        // passed through verbatim; anything else — bare terms, and crucially any
        // non-allowlisted URL — goes through the search source so Lavalink never
        // fetches an arbitrary URL server-side (SSRF).
        let is_url = query.starts_with("http://") || query.starts_with("https://");
        let query = if is_url && Self::is_allowed_media_url(query) {
            query.to_string()
        } else {
            format!("{}:{}", self.state.config.lavalink.search_source, query)
        };

        let loaded = match lava.load_tracks(guild_id, &query).await {
            Ok(t) => t,
            Err(e) => {
                self.send_error(
                    ctx,
                    msg.channel_id,
                    MusicError::Failed(format!("Failed to load tracks: {e}")),
                )
                .await;
                return;
            }
        };

        let mut tracks: Vec<TrackInQueue> = match loaded.data {
            Some(TrackLoadData::Track(t)) => vec![t.into()],
            Some(TrackLoadData::Search(list)) => match list.into_iter().next() {
                Some(t) => vec![t.into()],
                None => {
                    self.send_error(ctx, msg.channel_id, MusicError::TrackNotFound)
                        .await;
                    return;
                }
            },
            Some(TrackLoadData::Playlist(pl)) => pl.tracks.into_iter().map(Into::into).collect(),
            _ => {
                self.send_error(ctx, msg.channel_id, MusicError::TrackNotFound)
                    .await;
                return;
            }
        };

        if tracks.is_empty() {
            self.send_error(ctx, msg.channel_id, MusicError::TrackNotFound)
                .await;
            return;
        }

        // Tag every track with the requester for queue / now-playing displays.
        for t in &mut tracks {
            t.track.user_data = Some(serde_json::json!({ "requester_id": msg.author.id.get() }));
        }

        let added = tracks.len();
        let first = tracks[0].track.info.clone();

        let queue = player.get_queue();
        // Bound the in-memory queue so repeated large-playlist loads can't grow
        // it without limit and exhaust memory.
        let current = queue.get_count().await.unwrap_or(0);
        if current + added > MAX_QUEUE {
            self.send_error(ctx, msg.channel_id, MusicError::QueueFull)
                .await;
            return;
        }
        if let Err(e) = queue.append(tracks.into()) {
            self.send_error(
                ctx,
                msg.channel_id,
                MusicError::Failed(format!("Failed to queue tracks: {e}")),
            )
            .await;
            return;
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
        self.send_success(ctx, msg.channel_id, "Queued", &desc)
            .await;
    }

    async fn cmd_disconnect(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        if let Err(e) = self.require_same_voice(ctx, guild_id, msg.author.id) {
            self.send_error(ctx, msg.channel_id, e).await;
            return;
        }
        let Some(lava) = self.lava() else {
            self.send_error(ctx, msg.channel_id, MusicError::NotReady)
                .await;
            return;
        };
        if lava.get_player_context(guild_id).is_none() {
            self.send_error(ctx, msg.channel_id, MusicError::NotConnected)
                .await;
            return;
        }
        let _ = lava.delete_player(guild_id).await;
        Self::send_voice_state(ctx, guild_id, None);
        self.send_success(
            ctx,
            msg.channel_id,
            "Disconnected",
            "Left the voice channel.",
        )
        .await;
    }

    async fn cmd_pause(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        if let Err(e) = self.require_same_voice(ctx, guild_id, msg.author.id) {
            self.send_error(ctx, msg.channel_id, e).await;
            return;
        }
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
        };
        match player.get_player().await {
            Ok(data) if data.track.is_some() => {
                let _ = player.set_pause(true).await;
                self.send_success(ctx, msg.channel_id, "Paused", "Playback paused.")
                    .await;
            }
            _ => {
                self.send_error(ctx, msg.channel_id, MusicError::NothingPlaying)
                    .await
            }
        }
    }

    async fn cmd_resume(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        if let Err(e) = self.require_same_voice(ctx, guild_id, msg.author.id) {
            self.send_error(ctx, msg.channel_id, e).await;
            return;
        }
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
        };
        match player.get_player().await {
            Ok(data) if data.track.is_some() => {
                let _ = player.set_pause(false).await;
                self.send_success(ctx, msg.channel_id, "Resumed", "Playback resumed.")
                    .await;
            }
            _ => {
                self.send_error(ctx, msg.channel_id, MusicError::NothingPlaying)
                    .await
            }
        }
    }

    async fn cmd_skip(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        if let Err(e) = self.require_same_voice(ctx, guild_id, msg.author.id) {
            self.send_error(ctx, msg.channel_id, e).await;
            return;
        }
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
        };
        match player.get_player().await {
            Ok(data) => match data.track {
                Some(track) => {
                    let _ = player.skip();
                    self.send_success(
                        ctx,
                        msg.channel_id,
                        "Skipped",
                        &format!("Skipped **{}**.", track.info.title),
                    )
                    .await;
                }
                None => {
                    self.send_error(ctx, msg.channel_id, MusicError::NothingPlaying)
                        .await
                }
            },
            Err(_) => {
                self.send_error(ctx, msg.channel_id, MusicError::NothingPlaying)
                    .await
            }
        }
    }

    async fn cmd_stop(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        if let Err(e) = self.require_same_voice(ctx, guild_id, msg.author.id) {
            self.send_error(ctx, msg.channel_id, e).await;
            return;
        }
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
        };
        let _ = player.get_queue().clear();
        let _ = player.stop_now().await;
        self.send_success(
            ctx,
            msg.channel_id,
            "Stopped",
            "Stopped playback and cleared the queue.",
        )
        .await;
    }

    async fn cmd_volume(&self, ctx: &Context, msg: &Message, guild_id: GuildId, args: &str) {
        if let Err(e) = self.require_same_voice(ctx, guild_id, msg.author.id) {
            self.send_error(ctx, msg.channel_id, e).await;
            return;
        }
        let volume: u16 = match args.trim().parse() {
            Ok(v) if (1..=100).contains(&v) => v,
            _ => {
                self.send_error(
                    ctx,
                    msg.channel_id,
                    MusicError::Failed("Volume must be a number between 1 and 100.".to_string()),
                )
                .await;
                return;
            }
        };
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
        };
        let _ = player.set_volume(volume).await;
        self.send_success(
            ctx,
            msg.channel_id,
            "Volume",
            &format!("Volume set to **{volume}%**."),
        )
        .await;
    }

    async fn cmd_nowplaying(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
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
                    self.send_embed(ctx, msg.channel_id, embed).await;
                }
                None => {
                    self.send_error(ctx, msg.channel_id, MusicError::NothingPlaying)
                        .await
                }
            },
            Err(_) => {
                self.send_error(ctx, msg.channel_id, MusicError::NothingPlaying)
                    .await
            }
        }
    }

    async fn cmd_queue(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        let player = match self.player_ctx(guild_id) {
            Ok(p) => p,
            Err(e) => return self.send_error(ctx, msg.channel_id, e).await,
        };

        let now_playing = match player.get_player().await {
            Ok(data) => data.track,
            Err(_) => None,
        };
        let tracks = player.get_queue().get_queue().await.unwrap_or_default();

        if now_playing.is_none() && tracks.is_empty() {
            self.send_error(ctx, msg.channel_id, MusicError::QueueEmpty)
                .await;
            return;
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
        self.send_embed(ctx, msg.channel_id, embed).await;
    }
}

#[async_trait]
impl Cog for MusicCog {
    async fn on_command(&self, ctx: &Context, msg: &Message, inv: &CommandInvocation<'_>) -> bool {
        let Some(guild_id) = msg.guild_id else {
            return false;
        };
        let cmd = inv.command.to_lowercase();
        let args = inv.args;

        match cmd.as_str() {
            "play" | "p" => self.cmd_play(ctx, msg, guild_id, args).await,
            "disconnect" | "dc" | "leave" => self.cmd_disconnect(ctx, msg, guild_id).await,
            "pause" => self.cmd_pause(ctx, msg, guild_id).await,
            "resume" => self.cmd_resume(ctx, msg, guild_id).await,
            "skip" => self.cmd_skip(ctx, msg, guild_id).await,
            "queue" | "q" => self.cmd_queue(ctx, msg, guild_id).await,
            "nowplaying" | "np" => self.cmd_nowplaying(ctx, msg, guild_id).await,
            "volume" | "vol" => self.cmd_volume(ctx, msg, guild_id, args).await,
            "stop" => self.cmd_stop(ctx, msg, guild_id).await,
            _ => return false,
        }
        true
    }
}
