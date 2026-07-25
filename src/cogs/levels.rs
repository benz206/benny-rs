//! MEE6-style leveling/XP: members earn XP for chatting (rate-limited per
//! guild+user), level up on a cubic curve, and optionally get announced and
//! granted role rewards at configured milestones.

use super::Cog;
use crate::entities::{levels_config, levels_rewards, levels_users};
use crate::framework::{Context, Data, Error, send_embed, send_error, send_plain};
use crate::state::AppState;
use crate::utils::{colors, config};
use crate::utils::ratelimit::RateLimiter;
use async_trait::async_trait;
use dashmap::DashMap;
use rand::RngExt;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use serenity::all::{ChannelId, CreateEmbed, CreateMessage, Message, RoleId};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

/// Per-guild leveling configuration, mirrored from `levels_config`.
static CONFIG_CACHE: LazyLock<DashMap<u64, levels_config::Model>> = LazyLock::new(DashMap::new);

/// Per-guild level-role rewards as `(level, role_id)`, mirrored from
/// `levels_rewards`.
static REWARDS_CACHE: LazyLock<DashMap<u64, Vec<(i64, i64)>>> = LazyLock::new(DashMap::new);

pub struct LevelsCog {
    state: Arc<AppState>,
    /// Per-(guild, user) XP-award cooldown.
    xp_limiter: RateLimiter<(u64, u64)>,
}

impl LevelsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            xp_limiter: RateLimiter::new(50_000),
        })
    }
}

#[async_trait]
impl Cog for LevelsCog {
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        config::hydrate_cache::<levels_config::Entity, _>(
            self.state.servers_orm(),
            &CONFIG_CACHE,
            |m| m.guild_id as u64,
            |m| m,
        )
        .await;

        let rewards = levels_rewards::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();
        for m in rewards {
            REWARDS_CACHE
                .entry(m.guild_id as u64)
                .or_default()
                .push((m.level, m.role_id));
        }

        tracing::info!("Levels configs loaded");
    }

    async fn on_message(&self, ctx: &serenity::all::Context, msg: &Message) {
        let Some(guild_id) = msg.guild_id else {
            return;
        };
        let gid = guild_id.get();
        let uid = msg.author.id.get();

        let cfg = match CONFIG_CACHE.get(&gid) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };

        if self.state.starts_with_prefix(msg) {
            return;
        }

        if self
            .xp_limiter
            .check((gid, uid), Duration::from_secs(cfg.cooldown_secs.max(1) as u64))
            .is_some()
        {
            return;
        }

        let award = rand::rng().random_range(cfg.xp_min..=cfg.xp_max.max(cfg.xp_min));

        let old_xp = match levels_users::Entity::find_by_id((gid as i64, uid as i64))
            .one(self.state.servers_orm())
            .await
        {
            Ok(row) => row.map(|r| r.xp).unwrap_or(0),
            Err(e) => {
                tracing::error!(error = ?e, "failed to load levels_users row");
                return;
            }
        };
        let new_xp = old_xp + award;

        let active = levels_users::ActiveModel {
            guild_id: Set(gid as i64),
            user_id: Set(uid as i64),
            xp: Set(new_xp),
        };
        if let Err(e) = levels_users::Entity::insert(active)
            .on_conflict(
                OnConflict::columns([levels_users::Column::GuildId, levels_users::Column::UserId])
                    .update_column(levels_users::Column::Xp)
                    .to_owned(),
            )
            .exec(self.state.servers_orm())
            .await
        {
            tracing::error!(error = ?e, "failed to upsert levels_users xp");
            return;
        }

        let (old_level, _, _) = level_from_xp(old_xp);
        let (new_level, _, _) = level_from_xp(new_xp);
        if new_level <= old_level {
            return;
        }

        if cfg.announce {
            let channel_id = cfg
                .levelup_channel_id
                .map(|c| ChannelId::new(c as u64))
                .unwrap_or(msg.channel_id);
            let _ = channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new()
                        .content(format!("🎉 <@{uid}> reached **level {new_level}**!")),
                )
                .await;
        }

        let rewards = REWARDS_CACHE
            .get(&gid)
            .map(|r| r.clone())
            .unwrap_or_default();
        for (level, role_id) in rewards {
            if level > old_level && level <= new_level {
                let _ = ctx
                    .http
                    .add_member_role(
                        guild_id,
                        msg.author.id,
                        RoleId::new(role_id as u64),
                        Some("Level reward"),
                    )
                    .await;
            }
        }
    }
}

// ---- level curve ------------------------------------------------------------

/// XP required to go from `level` to `level + 1` (MEE6 curve).
fn xp_for_level(level: i64) -> i64 {
    5 * level * level + 50 * level + 100
}

/// Convert total accumulated XP into `(level, xp_into_level, xp_needed_for_next)`.
pub(crate) fn level_from_xp(total: i64) -> (i64, i64, i64) {
    let mut level = 0i64;
    let mut remaining = total;
    loop {
        let needed = xp_for_level(level);
        if remaining < needed {
            return (level, remaining, needed);
        }
        remaining -= needed;
        level += 1;
    }
}

// ---- config helpers ---------------------------------------------------------

/// Config row defaults for a guild that has never touched leveling.
pub(crate) fn default_model(gid: u64) -> levels_config::Model {
    levels_config::Model {
        guild_id: gid as i64,
        enabled: false,
        announce: true,
        levelup_channel_id: None,
        xp_min: 15,
        xp_max: 25,
        cooldown_secs: 60,
    }
}

/// Load-modify-upsert the guild's leveling config and refresh the cache.
pub(crate) async fn update_config<F: FnOnce(&mut levels_config::Model)>(
    state: &AppState,
    gid: u64,
    f: F,
) -> Result<levels_config::Model, sea_orm::DbErr> {
    let mut model = levels_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .unwrap_or_else(|| default_model(gid));
    f(&mut model);

    let active = levels_config::ActiveModel {
        guild_id: Set(model.guild_id),
        enabled: Set(model.enabled),
        announce: Set(model.announce),
        levelup_channel_id: Set(model.levelup_channel_id),
        xp_min: Set(model.xp_min),
        xp_max: Set(model.xp_max),
        cooldown_secs: Set(model.cooldown_secs),
    };
    levels_config::Entity::insert(active)
        .on_conflict(
            OnConflict::column(levels_config::Column::GuildId)
                .update_columns([
                    levels_config::Column::Enabled,
                    levels_config::Column::Announce,
                    levels_config::Column::LevelupChannelId,
                    levels_config::Column::XpMin,
                    levels_config::Column::XpMax,
                    levels_config::Column::CooldownSecs,
                ])
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await?;

    CONFIG_CACHE.insert(gid, model.clone());
    Ok(model)
}

/// Run `f` against the guild's leveling config and confirm with `msg`, handling
/// errors uniformly.
async fn apply_setting<F: FnOnce(&mut levels_config::Model)>(
    ctx: Context<'_>,
    f: F,
    msg: String,
) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    config::apply_setting(
        ctx,
        "levels",
        msg,
        "Failed to save the levels config.",
        update_config(&ctx.data().state, gid, f),
    )
    .await
}

/// Reload a guild's role rewards from the DB into `REWARDS_CACHE`.
pub(crate) async fn refresh_rewards_cache(state: &AppState, gid: u64) {
    let rows = levels_rewards::Entity::find()
        .filter(levels_rewards::Column::GuildId.eq(gid as i64))
        .all(state.servers_orm())
        .await
        .unwrap_or_default();
    let list = rows.into_iter().map(|r| (r.level, r.role_id)).collect();
    REWARDS_CACHE.insert(gid, list);
}

// ---- commands ---------------------------------------------------------------

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![rank(), leaderboard(), levels()]
}

/// Show a member's current level and XP progress.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Levels",
    aliases("level")
)]
async fn rank(
    ctx: Context<'_>,
    #[description = "Member"] member: Option<serenity::all::User>,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let gid = ctx.guild_id().unwrap().get();
    let target = member.as_ref().unwrap_or_else(|| ctx.author());
    let uid = target.id.get();

    let row = match levels_users::Entity::find_by_id((gid as i64, uid as i64))
        .one(state.servers_orm())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load levels_users row");
            return send_error(ctx, "Failed to load rank data.").await;
        }
    };
    let Some(row) = row else {
        return send_plain(ctx, format!("**{}** has no XP yet.", target.name)).await;
    };

    let (level, xp_into, needed) = level_from_xp(row.xp);
    let embed = CreateEmbed::new()
        .title(format!("{} — Level {level}", target.name))
        .description(format!(
            "**Total XP:** {}\n**Progress:** {xp_into}/{needed} XP to level {}",
            row.xp,
            level + 1
        ))
        .color(colors::BLURPLE);
    send_embed(ctx, embed).await
}

/// Show the server's top 10 members by XP.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Levels",
    aliases("lb", "top")
)]
async fn leaderboard(ctx: Context<'_>) -> Result<(), Error> {
    let state = &ctx.data().state;
    let gid = ctx.guild_id().unwrap().get();

    let rows = match levels_users::Entity::find()
        .filter(levels_users::Column::GuildId.eq(gid as i64))
        .order_by_desc(levels_users::Column::Xp)
        .limit(10)
        .all(state.servers_orm())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load levels leaderboard");
            return send_error(ctx, "Failed to load the leaderboard.").await;
        }
    };

    if rows.is_empty() {
        return send_plain(ctx, "No one has earned any XP yet.").await;
    }

    let mut desc = String::new();
    for (i, row) in rows.iter().enumerate() {
        let (level, _, _) = level_from_xp(row.xp);
        desc.push_str(&format!(
            "**{}.** <@{}> — level {level} ({} XP)\n",
            i + 1,
            row.user_id,
            row.xp
        ));
    }

    let embed = CreateEmbed::new()
        .title("Leaderboard")
        .description(desc)
        .color(colors::BLURPLE);
    send_embed(ctx, embed).await
}

/// Configure the leveling system.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Levels",
    required_permissions = "MANAGE_GUILD",
    subcommands(
        "lv_enable",
        "lv_disable",
        "lv_config",
        "lv_channel",
        "lv_announce",
        "lv_reward",
        "lv_unreward",
        "lv_rewards"
    ),
    subcommand_required
)]
async fn levels(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Turn leveling on.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_enable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(ctx, |c| c.enabled = true, "Leveling **enabled**.".to_string()).await
}

/// Turn leveling off.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_disable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(ctx, |c| c.enabled = false, "Leveling **disabled**.".to_string()).await
}

/// Show the current leveling configuration.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "config",
    aliases("show"),
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_config(ctx: Context<'_>) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    let cfg = CONFIG_CACHE
        .get(&gid)
        .map(|c| c.clone())
        .unwrap_or_else(|| default_model(gid));

    let channel = cfg
        .levelup_channel_id
        .map(|c| format!("<#{c}>"))
        .unwrap_or_else(|| "same channel".to_string());
    let desc = format!(
        "**Enabled:** {}\n**Announce level-ups:** {}\n**Announce channel:** {}\n\
         **XP per message:** {}-{}\n**Cooldown:** {}s",
        cfg.enabled, cfg.announce, channel, cfg.xp_min, cfg.xp_max, cfg.cooldown_secs,
    );
    let embed = CreateEmbed::new()
        .title("Levels Config")
        .description(desc)
        .color(colors::BLURPLE);
    send_embed(ctx, embed).await
}

/// Set (or clear) the channel level-up announcements are sent to.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "channel",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_channel(
    ctx: Context<'_>,
    #[description = "Channel for level-up announcements (omit to announce in the same channel)"]
    channel: Option<serenity::all::Channel>,
) -> Result<(), Error> {
    let id = channel.map(|c| c.id().get() as i64);
    let msg = match id {
        Some(c) => format!("Level-up announcements will be sent to <#{c}>."),
        None => {
            "Level-up announcements will be sent in the same channel the message was sent in."
                .to_string()
        }
    };
    apply_setting(ctx, |c| c.levelup_channel_id = id, msg).await
}

/// Toggle level-up announcement messages.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "announce",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_announce(
    ctx: Context<'_>,
    #[description = "Announce level-ups"] enabled: bool,
) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.announce = enabled,
        format!(
            "Level-up announcements **{}**.",
            if enabled { "on" } else { "off" }
        ),
    )
    .await
}

/// Grant a role automatically when a member reaches a level.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "reward",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_reward(
    ctx: Context<'_>,
    #[description = "Level at which the role is granted"]
    #[min = 1]
    #[max = 1000]
    level: i64,
    #[description = "Role to grant"] role: serenity::all::Role,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let gid = ctx.guild_id().unwrap().get();
    let role_id = role.id.get() as i64;

    let active = levels_rewards::ActiveModel {
        guild_id: Set(gid as i64),
        level: Set(level),
        role_id: Set(role_id),
    };
    if let Err(e) = levels_rewards::Entity::insert(active)
        .on_conflict(
            OnConflict::columns([levels_rewards::Column::GuildId, levels_rewards::Column::Level])
                .update_column(levels_rewards::Column::RoleId)
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await
    {
        tracing::error!(error = ?e, "failed to save level reward");
        return send_error(ctx, "Failed to save the level reward.").await;
    }

    refresh_rewards_cache(state, gid).await;
    send_plain(
        ctx,
        format!("Members reaching level **{level}** will now receive <@&{role_id}>."),
    )
    .await
}

/// Remove a level's role reward.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "unreward",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_unreward(
    ctx: Context<'_>,
    #[description = "Level of the reward to remove"] level: i64,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let gid = ctx.guild_id().unwrap().get();

    match levels_rewards::Entity::delete_by_id((gid as i64, level))
        .exec(state.servers_orm())
        .await
    {
        Ok(res) if res.rows_affected > 0 => {
            refresh_rewards_cache(state, gid).await;
            send_plain(ctx, format!("Removed the level **{level}** reward.")).await
        }
        Ok(_) => send_error(ctx, &format!("No reward is configured for level {level}.")).await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to remove level reward");
            send_error(ctx, "Failed to remove the level reward.").await
        }
    }
}

/// List configured level-role rewards.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "rewards",
    category = "Levels",
    required_permissions = "MANAGE_GUILD"
)]
async fn lv_rewards(ctx: Context<'_>) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    let mut rewards = REWARDS_CACHE
        .get(&gid)
        .map(|r| r.clone())
        .unwrap_or_default();
    rewards.sort_by_key(|(level, _)| *level);

    if rewards.is_empty() {
        return send_plain(ctx, "No level rewards are configured.").await;
    }

    let desc = rewards
        .iter()
        .map(|(level, role_id)| format!("**Level {level}:** <@&{role_id}>"))
        .collect::<Vec<_>>()
        .join("\n");
    let embed = CreateEmbed::new()
        .title("Level Rewards")
        .description(desc)
        .color(colors::BLURPLE);
    send_embed(ctx, embed).await
}
