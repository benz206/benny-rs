use super::Cog;
use crate::state::AppState;
use crate::utils::colors;
use crate::utils::ratelimit::RateLimiter;
use async_trait::async_trait;
use serenity::all::{
    ChannelType, Context, CreateEmbed, CreateMessage, Guild, GuildChannel, GuildId, Message,
    Timestamp, UnavailableGuild, UserId,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tracing::{info, warn};

/// Throttles thread auto-join to at most once per guild per window, so a
/// burst of thread creations doesn't fire an unthrottled HTTP call per thread.
static THREAD_JOIN_LIMITER: LazyLock<RateLimiter<u64>> = LazyLock::new(|| RateLimiter::new(8192));

/// Bot lifecycle / internals cog: auto-leave policy, thread auto-join,
/// command logging, and guild join/remove logging.
///
/// The 15s gateway-latency loop lives in `state::start_latency_task` (spawned
/// from `main.rs`) and is intentionally NOT duplicated here.
pub struct EventsCog {
    state: Arc<AppState>,
    /// Set true the first time `on_ready` fires. Until then we cannot tell a
    /// genuine join apart from the initial guild-sync stream, so the auto-leave
    /// policy stays disabled (conservative — never leave during sync).
    ready_seen: AtomicBool,
    /// Guild ids the bot was already a member of as of the most recent
    /// `on_ready`. A `guild_create` whose id is in this set is the gateway
    /// re-sending an existing guild (startup sync or a post-outage reconnect),
    /// NOT a genuine join — discord.py gets this separation for free via its
    /// distinct `on_guild_join` event; serenity collapses both into
    /// `guild_create`, so we reconstruct it here.
    known_guilds: Mutex<HashSet<GuildId>>,
}

impl EventsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            ready_seen: AtomicBool::new(false),
            known_guilds: Mutex::new(HashSet::new()),
        })
    }

    /// True if any configured bot owner is a member of `guild`:
    /// trusted/owner servers are exempt from the auto-leave policy.
    async fn owner_present(&self, ctx: &Context, guild: &Guild) -> bool {
        for &owner in &self.state.config.owners {
            let uid = UserId::new(owner);
            // Cheap cache/payload check first, then an HTTP fetch fallback.
            if guild.members.contains_key(&uid) {
                return true;
            }
            if guild.id.member(&ctx.http, uid).await.is_ok() {
                return true;
            }
        }
        false
    }

    /// Best-effort policy notice + leave. Announces in a `#general`-ish text
    /// channel (falling back to the first text channel), then leaves the guild.
    async fn notify_and_leave(
        &self,
        ctx: &Context,
        guild: &Guild,
        bots: usize,
        humans: usize,
        total: usize,
        pct: f64,
    ) {
        let embed = CreateEmbed::new()
            .title("Sorry!")
            .description(format!(
                "Your server has **{bots} Bots** compared to **{total} Members**\n\
                 Either:\n\
                 - Have `6+` humans (Currently **{humans}** humans)\n\
                 - Lower your server's percentage of bots to under 20% \
                 (Currently **{pct}%** bots)"
            ))
            .color(colors::RED)
            .timestamp(Timestamp::now());

        let target_id = guild
            .channels
            .values()
            .filter(|c| c.kind == ChannelType::Text)
            .find(|c| c.name.contains("general"))
            .or_else(|| {
                guild
                    .channels
                    .values()
                    .find(|c| c.kind == ChannelType::Text)
            })
            .map(|c| c.id);

        if let Some(cid) = target_id {
            let _ = cid
                .send_message(&ctx.http, CreateMessage::new().embed(embed))
                .await;
        }

        match guild.id.leave(&ctx.http).await {
            Ok(_) => info!(
                "AUTOLEFT {} ({}) | {pct}% bots, {humans} humans of {total}",
                guild.name, guild.id
            ),
            Err(e) => warn!("Failed to auto-leave {} ({}): {e}", guild.name, guild.id),
        }
    }
}

#[async_trait]
impl Cog for EventsCog {
    async fn on_ready(&self, ctx: &Context) {
        // Snapshot the guilds we are already in. After READY, serenity's cache
        // holds the full guild-id set (available + unavailable), so this is the
        // complete baseline against which later joins are detected.
        let guilds: HashSet<GuildId> = ctx.cache.guilds().into_iter().collect();
        let count = guilds.len();
        if let Ok(mut known) = self.known_guilds.lock() {
            *known = guilds;
        }
        self.ready_seen.store(true, Ordering::SeqCst);
        info!("Events cog ready; tracking {count} known guild(s) for join detection");
    }

    async fn on_guild_create(&self, ctx: &Context, guild: &Guild) {
        // Keep the dashboard API's membership mirror current for every
        // guild_create (genuine join, startup sync, or reconnect) — idempotent.
        self.state.guild_set.insert(guild.id.get(), ());

        // Only act on genuine joins, never the startup/reconnect guild sync.
        if !self.ready_seen.load(Ordering::SeqCst) {
            return;
        }
        {
            let mut known = match self.known_guilds.lock() {
                Ok(k) => k,
                Err(_) => return,
            };
            if known.contains(&guild.id) {
                return; // already known -> sync/reconnect, not a join
            }
            // Mark processed so a duplicate GUILD_CREATE won't re-trigger.
            known.insert(guild.id);
        }

        // Trusted/owner servers bypass the policy entirely (no leave).
        if self.owner_present(ctx, guild).await {
            info!(
                "Joined guild {} ({}) — owner present, auto-leave policy skipped",
                guild.name, guild.id
            );
            return;
        }

        let (bots, humans, total, pct) = bot_ratio(guild);
        info!(
            "JOINED {} ({}) | {humans} humans / {bots} bots / {total} members ({pct}% bots)",
            guild.name, guild.id
        );

        if total == 0 {
            return; // no members loaded — cannot evaluate the policy
        }

        // Auto-leave policy: leave if >20% of members are bots OR fewer than 5 humans.
        if pct > 20.0 || humans < 5 {
            self.notify_and_leave(ctx, guild, bots, humans, total, pct)
                .await;
        }
    }

    async fn on_guild_delete(
        &self,
        _ctx: &Context,
        incomplete: UnavailableGuild,
        full: Option<Guild>,
    ) {
        // A guild going *unavailable* (outage) is not a real removal; ignore it
        // so reconnections don't masquerade as leaves.
        if incomplete.unavailable {
            return;
        }
        // A genuine removal — drop it from the dashboard API membership mirror.
        self.state.guild_set.remove(&incomplete.id.get());
        // Forget the guild so a future re-join is detected as a genuine join.
        if let Ok(mut known) = self.known_guilds.lock() {
            known.remove(&incomplete.id);
        }
        match &full {
            Some(g) => {
                let (bots, humans, total, pct) = bot_ratio(g);
                info!(
                    "LEFT {} ({}) | {humans} humans / {bots} bots / {total} members ({pct}% bots)",
                    g.name, incomplete.id
                );
            }
            None => info!("LEFT guild {} (uncached)", incomplete.id),
        }
    }

    async fn on_thread_create(&self, ctx: &Context, thread: &GuildChannel) {
        // Whenever possible, join newly created threads (throttled per guild).
        if THREAD_JOIN_LIMITER
            .check(thread.guild_id.get(), Duration::from_secs(5))
            .is_some()
        {
            return;
        }
        if let Err(e) = ctx.http.join_thread_channel(thread.id).await {
            warn!("Failed to join thread {} ({}): {e}", thread.name, thread.id);
        }
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        // Command logging only — dispatching/execution is owned by other cogs.
        if msg.author.bot {
            return;
        }
        let prefix = self.state.prefix();
        if prefix.is_empty() || !msg.content.trim_start().starts_with(prefix) {
            return;
        }

        let (guild_name, channel_name) = match msg.guild_id.and_then(|gid| ctx.cache.guild(gid)) {
            Some(g) => {
                let cn = g
                    .channels
                    .get(&msg.channel_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| msg.channel_id.to_string());
                (g.name.clone(), cn)
            }
            None => ("DM".to_string(), msg.channel_id.to_string()),
        };

        info!(
            "{guild_name} / {channel_name} / {}: {}",
            msg.author.name, msg.content
        );
    }
}

/// Compute `(bots, humans, total, bot_percentage)` from a guild's loaded
/// members. Percentage is `trunc((bots/total)*10000)/100`
/// (two-decimal precision), and is 0.0 when no members are loaded.
pub(crate) fn bot_ratio(guild: &Guild) -> (usize, usize, usize, f64) {
    let total = guild.members.len();
    let bots = guild.members.values().filter(|m| m.user.bot).count();
    let humans = total.saturating_sub(bots);
    let pct = if total == 0 {
        0.0
    } else {
        ((bots as f64 / total as f64) * 10000.0).trunc() / 100.0
    };
    (bots, humans, total, pct)
}
