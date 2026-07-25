//! Automod: bot-side message filters (invite links, links, mass mentions,
//! spam) with a configurable punishment, plus join-raid detection and an
//! account-age gate. Layers on top of Discord's native AutoMod (keyword and
//! spam rules), which server owners configure separately — members with
//! Manage Messages are always exempt.

use super::Cog;
use crate::cogs::moderation::{apply_native_timeout, create_case};
use crate::entities::automod_config;
use crate::framework::{Context, Data, Error, send_embed};
use crate::state::AppState;
use crate::utils::ratelimit::RateLimiter;
use crate::utils::{colors, config, perms};
use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use sea_orm::sea_query::OnConflict;
use sea_orm::{EntityTrait, Set};
use serenity::all::{
    Channel, ChannelId, CreateEmbed, CreateMessage, GuildId, Member, Message, Permissions,
};
use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

/// Per-guild automod configuration, mirrored from `automod_config`.
static CONFIG_CACHE: LazyLock<DashMap<u64, automod_config::Model>> = LazyLock::new(DashMap::new);

/// Recent message timestamps per (guild, user) for spam detection.
static MSG_WINDOW: LazyLock<DashMap<(u64, u64), VecDeque<i64>>> = LazyLock::new(DashMap::new);
const MSG_WINDOW_CAP: usize = 50_000;

/// Recent join timestamps per guild for raid detection.
static JOIN_WINDOW: LazyLock<DashMap<u64, VecDeque<i64>>> = LazyLock::new(DashMap::new);
const JOIN_WINDOW_CAP: usize = 10_000;

/// Evict one arbitrary entry from `map` before inserting a new key if the map
/// is already at `cap`. `MSG_WINDOW`/`JOIN_WINDOW` values are mutated in place
/// via `entry().or_default()`, so they can't go through
/// [`crate::utils::cache::bounded_insert`] (which replaces the whole value);
/// this mirrors its "evict arbitrary entries" approach instead.
fn evict_if_full<K: Eq + std::hash::Hash + Clone, V>(map: &DashMap<K, V>, key: &K, cap: usize) {
    if map.len() >= cap && !map.contains_key(key)
        && let Some(victim) = map.iter().next().map(|e| e.key().clone()) {
            map.remove(&victim);
        }
}

/// Substrings that identify a Discord invite link.
const INVITE_MARKERS: [&str; 3] = ["discord.gg/", "discord.com/invite/", "discordapp.com/invite/"];

/// Don't re-punish the same member more often than this (their messages are
/// still deleted in between — this only bounds timeout/kick/warn spam).
const PUNISH_COOLDOWN: Duration = Duration::from_secs(10);

/// Emit at most one raid alert per guild per window.
const RAID_ALERT_COOLDOWN: Duration = Duration::from_secs(60);

pub struct AutomodCog {
    state: Arc<AppState>,
    punish_limiter: RateLimiter<(u64, u64)>,
    raid_limiter: RateLimiter<u64>,
}

impl AutomodCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            punish_limiter: RateLimiter::new(10_000),
            raid_limiter: RateLimiter::new(1_000),
        })
    }

    async fn punish(&self, ctx: &serenity::all::Context, cfg: &automod_config::Model, msg: &Message, violation: &str) {
        let _ = msg.delete(&ctx.http).await;

        let gid = GuildId::new(cfg.guild_id as u64);
        let uid = msg.author.id.get();
        if self.punish_limiter.check((gid.get(), uid), PUNISH_COOLDOWN).is_some() {
            return;
        }

        let bot_id = ctx.cache.current_user().id.get();
        let reason = format!("Automod: {violation}");
        let action_taken = match cfg.punishment.as_str() {
            "warn" => {
                create_case(&self.state, gid, "warn", uid, bot_id, &reason, None).await;
                "message deleted, member warned"
            }
            "timeout" => {
                let secs = cfg.timeout_secs.clamp(60, 28 * 24 * 60 * 60);
                let expires_ts = Utc::now().timestamp() + secs;
                match apply_native_timeout(ctx, &self.state, gid, uid, bot_id, &reason, expires_ts)
                    .await
                {
                    Ok(_) => "message deleted, member timed out",
                    Err(_) => "message deleted (timeout failed)",
                }
            }
            "kick" => match gid.kick_with_reason(&ctx.http, msg.author.id, &reason).await {
                Ok(()) => {
                    create_case(&self.state, gid, "kick", uid, bot_id, &reason, None).await;
                    "message deleted, member kicked"
                }
                Err(_) => "message deleted (kick failed)",
            },
            _ => "message deleted",
        };

        self.log(
            ctx,
            cfg,
            "Automod triggered",
            &format!(
                "**Member:** <@{uid}>\n**Channel:** <#{}>\n**Violation:** {violation}\n**Action:** {action_taken}",
                msg.channel_id.get()
            ),
        )
        .await;
    }

    async fn log(
        &self,
        ctx: &serenity::all::Context,
        cfg: &automod_config::Model,
        title: &str,
        description: &str,
    ) {
        let Some(log_id) = cfg.log_channel_id else {
            return;
        };
        let embed = CreateEmbed::new()
            .title(title)
            .description(description)
            .color(colors::ORANGE);
        let _ = ChannelId::new(log_id as u64)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }
}

#[async_trait]
impl Cog for AutomodCog {
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        config::hydrate_cache::<automod_config::Entity, _>(
            self.state.servers_orm(),
            &CONFIG_CACHE,
            |m| m.guild_id as u64,
            |m| m,
        )
        .await;
        tracing::info!("Automod configs loaded");
    }

    async fn on_message(&self, ctx: &serenity::all::Context, msg: &Message) {
        let Some(guild_id) = msg.guild_id else {
            return;
        };
        let gid = guild_id.get();
        let cfg = match CONFIG_CACHE.get(&gid) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };

        let violation = check_message(&cfg, msg)
            .or_else(|| is_spamming(&cfg, gid, msg.author.id.get()).then_some("message spam"));
        let Some(violation) = violation else {
            return;
        };

        // Staff are exempt. Checked after the cheap filters so most messages
        // never need a permission lookup.
        if perms::has_perm(ctx, guild_id, msg.author.id.get(), Permissions::MANAGE_MESSAGES).await
        {
            return;
        }

        self.punish(ctx, &cfg, msg, violation).await;
    }

    async fn on_member_join(&self, ctx: &serenity::all::Context, member: &Member) {
        let gid = member.guild_id.get();
        let cfg = match CONFIG_CACHE.get(&gid) {
            Some(c) if c.enabled && c.raid_enabled => c.clone(),
            _ => return,
        };
        let now = Utc::now().timestamp();
        let uid = member.user.id.get();

        // Account-age gate.
        if cfg.min_account_age_days > 0 {
            let created = member.user.id.created_at().unix_timestamp();
            let age_days = (now - created) / 86_400;
            if age_days < cfg.min_account_age_days {
                if cfg.raid_action == "kick" {
                    let _ = member
                        .user
                        .dm(
                            &ctx.http,
                            CreateMessage::new().content(format!(
                                "Your account is too new to join this server (minimum age: {} days).",
                                cfg.min_account_age_days
                            )),
                        )
                        .await;
                    let _ = member
                        .guild_id
                        .kick_with_reason(&ctx.http, member.user.id, "Automod: account too new")
                        .await;
                }
                self.log(
                    ctx,
                    &cfg,
                    "Anti-raid: new account",
                    &format!(
                        "**Member:** <@{uid}>\n**Account age:** {age_days} day(s) (minimum {})\n**Action:** {}",
                        cfg.min_account_age_days,
                        if cfg.raid_action == "kick" { "kicked" } else { "alert only" }
                    ),
                )
                .await;
                return;
            }
        }

        // Join-rate raid detection.
        if cfg.raid_joins > 0 && cfg.raid_secs > 0 {
            let count = {
                evict_if_full(&JOIN_WINDOW, &gid, JOIN_WINDOW_CAP);
                let mut w = JOIN_WINDOW.entry(gid).or_default();
                w.push_back(now);
                while w.front().is_some_and(|t| now - t >= cfg.raid_secs) {
                    w.pop_front();
                }
                w.len() as i64
            };
            if count >= cfg.raid_joins {
                if cfg.raid_action == "kick" {
                    let _ = member
                        .guild_id
                        .kick_with_reason(&ctx.http, member.user.id, "Automod: join raid")
                        .await;
                }
                if self.raid_limiter.check(gid, RAID_ALERT_COOLDOWN).is_none() {
                    self.log(
                        ctx,
                        &cfg,
                        "Anti-raid: join surge",
                        &format!(
                            "**{count}** joins in the last **{}s** (threshold {}).\n**Action:** {}",
                            cfg.raid_secs,
                            cfg.raid_joins,
                            if cfg.raid_action == "kick" {
                                "kicking new joiners"
                            } else {
                                "alert only"
                            }
                        ),
                    )
                    .await;
                }
            }
        }
    }
}

/// Stateless message filters. Returns what was violated, if anything.
fn check_message(cfg: &automod_config::Model, msg: &Message) -> Option<&'static str> {
    let content = msg.content.to_lowercase();
    if cfg.anti_invite && INVITE_MARKERS.iter().any(|m| content.contains(m)) {
        return Some("invite link");
    }
    if cfg.anti_link && (content.contains("http://") || content.contains("https://")) {
        return Some("link");
    }
    if cfg.mention_limit > 0
        && (msg.mentions.len() + msg.mention_roles.len()) as i64 >= cfg.mention_limit
    {
        return Some("mass mentions");
    }
    None
}

/// Sliding-window spam check; records this message's timestamp.
fn is_spamming(cfg: &automod_config::Model, gid: u64, uid: u64) -> bool {
    if cfg.spam_msgs <= 0 || cfg.spam_secs <= 0 {
        return false;
    }
    let now = Utc::now().timestamp();
    let key = (gid, uid);
    evict_if_full(&MSG_WINDOW, &key, MSG_WINDOW_CAP);
    let mut w = MSG_WINDOW.entry(key).or_default();
    w.push_back(now);
    while w.front().is_some_and(|t| now - *t >= cfg.spam_secs) {
        w.pop_front();
    }
    (w.len() as i64) >= cfg.spam_msgs
}

/// Config row with defaults for a guild that has never touched automod.
pub(crate) fn default_model(gid: u64) -> automod_config::Model {
    automod_config::Model {
        guild_id: gid as i64,
        enabled: false,
        log_channel_id: None,
        anti_invite: true,
        anti_link: false,
        mention_limit: 8,
        spam_msgs: 8,
        spam_secs: 5,
        punishment: "delete".to_string(),
        timeout_secs: 600,
        raid_enabled: false,
        raid_joins: 10,
        raid_secs: 30,
        min_account_age_days: 0,
        raid_action: "alert".to_string(),
    }
}

/// Load-modify-upsert the guild's config and refresh the cache.
pub(crate) async fn update_config<F: FnOnce(&mut automod_config::Model)>(
    state: &AppState,
    gid: u64,
    f: F,
) -> Result<automod_config::Model, sea_orm::DbErr> {
    let mut model = automod_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .unwrap_or_else(|| default_model(gid));
    f(&mut model);

    let active = automod_config::ActiveModel {
        guild_id: Set(model.guild_id),
        enabled: Set(model.enabled),
        log_channel_id: Set(model.log_channel_id),
        anti_invite: Set(model.anti_invite),
        anti_link: Set(model.anti_link),
        mention_limit: Set(model.mention_limit),
        spam_msgs: Set(model.spam_msgs),
        spam_secs: Set(model.spam_secs),
        punishment: Set(model.punishment.clone()),
        timeout_secs: Set(model.timeout_secs),
        raid_enabled: Set(model.raid_enabled),
        raid_joins: Set(model.raid_joins),
        raid_secs: Set(model.raid_secs),
        min_account_age_days: Set(model.min_account_age_days),
        raid_action: Set(model.raid_action.clone()),
    };
    automod_config::Entity::insert(active)
        .on_conflict(
            OnConflict::column(automod_config::Column::GuildId)
                .update_columns([
                    automod_config::Column::Enabled,
                    automod_config::Column::LogChannelId,
                    automod_config::Column::AntiInvite,
                    automod_config::Column::AntiLink,
                    automod_config::Column::MentionLimit,
                    automod_config::Column::SpamMsgs,
                    automod_config::Column::SpamSecs,
                    automod_config::Column::Punishment,
                    automod_config::Column::TimeoutSecs,
                    automod_config::Column::RaidEnabled,
                    automod_config::Column::RaidJoins,
                    automod_config::Column::RaidSecs,
                    automod_config::Column::MinAccountAgeDays,
                    automod_config::Column::RaidAction,
                ])
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await?;

    CONFIG_CACHE.insert(gid, model.clone());
    Ok(model)
}

/// Run `f` against the guild's config and confirm with `msg`, handling errors
/// uniformly.
async fn apply_setting<F: FnOnce(&mut automod_config::Model)>(
    ctx: Context<'_>,
    f: F,
    msg: String,
) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    config::apply_setting(
        ctx,
        "automod",
        msg,
        "Failed to save the automod config.",
        update_config(&ctx.data().state, gid, f),
    )
    .await
}

// ---- commands ---------------------------------------------------------------

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![automod(), antiraid()]
}

/// Punishment applied to automod violations.
#[derive(poise::ChoiceParameter, Clone, Copy)]
enum PunishmentChoice {
    #[name = "delete"]
    Delete,
    #[name = "warn"]
    Warn,
    #[name = "timeout"]
    Timeout,
    #[name = "kick"]
    Kick,
}

impl PunishmentChoice {
    fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Warn => "warn",
            Self::Timeout => "timeout",
            Self::Kick => "kick",
        }
    }
}

/// Response to raid signals (join surges / too-new accounts).
#[derive(poise::ChoiceParameter, Clone, Copy)]
enum RaidActionChoice {
    #[name = "alert"]
    Alert,
    #[name = "kick"]
    Kick,
}

/// Configure bot-side message filters (invites, links, mentions, spam).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_GUILD",
    subcommands(
        "am_enable",
        "am_disable",
        "am_config",
        "am_invites",
        "am_links",
        "am_mentions",
        "am_spam",
        "am_punishment",
        "am_logchannel"
    ),
    subcommand_required
)]
async fn automod(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Turn automod on.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_enable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.enabled = true,
        "Automod **enabled**. Check `automod config` for the active filters.".to_string(),
    )
    .await
}

/// Turn automod off.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_disable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(ctx, |c| c.enabled = false, "Automod **disabled**.".to_string()).await
}

/// Show the current automod configuration.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "config",
    aliases("show"),
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_config(ctx: Context<'_>) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    let cfg = CONFIG_CACHE
        .get(&gid)
        .map(|c| c.clone())
        .unwrap_or_else(|| default_model(gid));

    let onoff = |b: bool| if b { "on" } else { "off" };
    let log = cfg
        .log_channel_id
        .map(|c| format!("<#{c}>"))
        .unwrap_or_else(|| "not set".to_string());
    let mentions = if cfg.mention_limit > 0 {
        format!("limit {}", cfg.mention_limit)
    } else {
        "off".to_string()
    };
    let spam = if cfg.spam_msgs > 0 && cfg.spam_secs > 0 {
        format!("{} msgs / {}s", cfg.spam_msgs, cfg.spam_secs)
    } else {
        "off".to_string()
    };
    let raid = if cfg.raid_joins > 0 && cfg.raid_secs > 0 {
        format!("{} joins / {}s", cfg.raid_joins, cfg.raid_secs)
    } else {
        "off".to_string()
    };
    let age = if cfg.min_account_age_days > 0 {
        format!("{} day(s)", cfg.min_account_age_days)
    } else {
        "off".to_string()
    };

    let embed = CreateEmbed::new()
        .title("Automod configuration")
        .description(format!(
            "**Automod:** {}\n**Log channel:** {log}\n**Invite filter:** {}\n**Link filter:** {}\n\
             **Mass mentions:** {mentions}\n**Spam:** {spam}\n**Punishment:** {} ({}s timeout)\n\n\
             **Anti-raid:** {}\n**Join rate:** {raid}\n**Min account age:** {age}\n**Raid action:** {}",
            onoff(cfg.enabled),
            onoff(cfg.anti_invite),
            onoff(cfg.anti_link),
            cfg.punishment,
            cfg.timeout_secs,
            onoff(cfg.raid_enabled),
            cfg.raid_action,
        ))
        .color(colors::BLURPLE);
    send_embed(ctx, embed).await
}

/// Toggle the Discord-invite filter.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "invites",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_invites(
    ctx: Context<'_>,
    #[description = "Delete Discord invite links?"] enabled: bool,
) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.anti_invite = enabled,
        format!("Invite filter **{}**.", if enabled { "on" } else { "off" }),
    )
    .await
}

/// Toggle the link filter (all http/https links).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "links",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_links(
    ctx: Context<'_>,
    #[description = "Delete all links?"] enabled: bool,
) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.anti_link = enabled,
        format!("Link filter **{}**.", if enabled { "on" } else { "off" }),
    )
    .await
}

/// Set the mass-mention limit (0 disables).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "mentions",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_mentions(
    ctx: Context<'_>,
    #[description = "Mentions per message before automod triggers (0 = off)"]
    #[min = 0]
    #[max = 50]
    limit: i64,
) -> Result<(), Error> {
    let msg = if limit > 0 {
        format!("Mass-mention limit set to **{limit}**.")
    } else {
        "Mass-mention filter **off**.".to_string()
    };
    apply_setting(ctx, |c| c.mention_limit = limit, msg).await
}

/// Set the spam window (0 for either value disables).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "spam",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_spam(
    ctx: Context<'_>,
    #[description = "Messages allowed in the window (0 = off)"]
    #[min = 0]
    #[max = 60]
    messages: i64,
    #[description = "Window length in seconds (0 = off)"]
    #[min = 0]
    #[max = 300]
    seconds: i64,
) -> Result<(), Error> {
    let msg = if messages > 0 && seconds > 0 {
        format!("Spam filter: more than **{messages}** messages in **{seconds}s** triggers automod.")
    } else {
        "Spam filter **off**.".to_string()
    };
    apply_setting(
        ctx,
        |c| {
            c.spam_msgs = messages;
            c.spam_secs = seconds;
        },
        msg,
    )
    .await
}

/// Set what happens to violators (offending messages are always deleted).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "punishment",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_punishment(
    ctx: Context<'_>,
    #[description = "Punishment for violations"] action: PunishmentChoice,
    #[description = "Timeout length in seconds if punishment is timeout (default 600)"]
    #[min = 60]
    #[max = 2419200]
    timeout_seconds: Option<i64>,
) -> Result<(), Error> {
    let secs = timeout_seconds.unwrap_or(600);
    apply_setting(
        ctx,
        |c| {
            c.punishment = action.as_str().to_string();
            c.timeout_secs = secs;
        },
        format!("Automod punishment set to **{}**.", action.as_str()),
    )
    .await
}

/// Set (or clear) the channel automod reports to.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "logchannel",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn am_logchannel(
    ctx: Context<'_>,
    #[description = "Channel for automod reports (omit to clear)"] channel: Option<Channel>,
) -> Result<(), Error> {
    let id = channel.map(|c| c.id().get() as i64);
    let msg = match id {
        Some(c) => format!("Automod log channel set to <#{c}>."),
        None => "Automod log channel cleared.".to_string(),
    };
    apply_setting(ctx, |c| c.log_channel_id = id, msg).await
}

/// Configure join-raid detection and the account-age gate.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_GUILD",
    subcommands("ar_enable", "ar_disable", "ar_age", "ar_rate", "ar_action"),
    subcommand_required
)]
async fn antiraid(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Turn anti-raid on (requires automod to be enabled too).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn ar_enable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.raid_enabled = true,
        "Anti-raid **enabled** (active while automod is enabled).".to_string(),
    )
    .await
}

/// Turn anti-raid off.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn ar_disable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(ctx, |c| c.raid_enabled = false, "Anti-raid **disabled**.".to_string()).await
}

/// Require accounts to be at least this old to join (0 disables).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "age",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn ar_age(
    ctx: Context<'_>,
    #[description = "Minimum account age in days (0 = off)"]
    #[min = 0]
    #[max = 365]
    days: i64,
) -> Result<(), Error> {
    let msg = if days > 0 {
        format!("Minimum account age set to **{days}** day(s).")
    } else {
        "Account-age gate **off**.".to_string()
    };
    apply_setting(ctx, |c| c.min_account_age_days = days, msg).await
}

/// Set the join-rate that counts as a raid (0 for either value disables).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "rate",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn ar_rate(
    ctx: Context<'_>,
    #[description = "Joins in the window before a raid is flagged (0 = off)"]
    #[min = 0]
    #[max = 100]
    joins: i64,
    #[description = "Window length in seconds (0 = off)"]
    #[min = 0]
    #[max = 600]
    seconds: i64,
) -> Result<(), Error> {
    let msg = if joins > 0 && seconds > 0 {
        format!("Raid detection: **{joins}** joins in **{seconds}s** raises the alarm.")
    } else {
        "Join-rate raid detection **off**.".to_string()
    };
    apply_setting(
        ctx,
        |c| {
            c.raid_joins = joins;
            c.raid_secs = seconds;
        },
        msg,
    )
    .await
}

/// Set the anti-raid response.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "action",
    category = "Moderation",
    required_permissions = "MANAGE_GUILD"
)]
async fn ar_action(
    ctx: Context<'_>,
    #[description = "alert = log only, kick = remove flagged joiners"] action: RaidActionChoice,
) -> Result<(), Error> {
    let name = match action {
        RaidActionChoice::Alert => "alert",
        RaidActionChoice::Kick => "kick",
    };
    apply_setting(
        ctx,
        |c| c.raid_action = name.to_string(),
        format!("Anti-raid action set to **{name}**."),
    )
    .await
}
