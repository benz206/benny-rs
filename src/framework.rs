//! poise command framework wiring.
//!
//! Commands are defined as `#[poise::command]` functions inside each cog module
//! (see `cogs::<name>::commands()`), serving both prefix and slash invocations.
//! Non-command gateway events are still fanned out to the `Cog` event hooks via
//! [`event_handler`]; poise owns command dispatch, the `CogManager` owns events.

use crate::cogs::CogManager;
use crate::state::AppState;
use crate::utils::embeds::error_embed;
use serenity::all::{CreateEmbed, FullEvent, Interaction};
use std::sync::Arc;

/// poise user data: the shared app state plus the cog manager used to fan
/// non-command events out to every registered cog.
pub struct Data {
    pub state: Arc<AppState>,
    pub cogs: Arc<CogManager>,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

/// Every command across every cog, concatenated into the list poise registers.
/// Add a cog's `commands()` here when it is converted to poise.
pub fn all_commands() -> Vec<poise::Command<Data, Error>> {
    let mut cmds = Vec::new();
    cmds.extend(crate::cogs::base::commands());
    cmds.extend(crate::cogs::info::commands());
    cmds.extend(crate::cogs::tags::commands());
    cmds.extend(crate::cogs::moderation::commands());
    cmds.extend(crate::cogs::prefixes::commands());
    cmds.extend(crate::cogs::settings::commands());
    cmds.extend(crate::cogs::welcome::commands());
    cmds.extend(crate::cogs::logging::commands());
    cmds.extend(crate::cogs::sentinel::commands());
    cmds.extend(crate::cogs::roles::commands());
    cmds.extend(crate::cogs::reminders::commands());
    cmds.extend(crate::cogs::premium::commands());
    cmds.extend(crate::cogs::translate::commands());
    cmds.extend(crate::cogs::dictionary::commands());
    cmds.extend(crate::cogs::ocr::commands());
    cmds.extend(crate::cogs::embed::commands());
    cmds.extend(crate::cogs::afk::commands());
    cmds.extend(crate::cogs::music::commands());
    cmds.extend(crate::cogs::dev::commands());
    cmds.extend(crate::cogs::help::commands());
    cmds
}

// ---- rate limits -----------------------------------------------------------
//
// poise enforces a command's cooldown automatically from `Command::cooldown_config`
// before dispatch, and `builtins::on_error` (which `on_error` below delegates to)
// already replies to `CooldownHit`. So setting the config is all that's needed.

/// Apply per-command cooldowns: a light per-user baseline on everything as
/// anti-spam, plus heavier limits on commands that hit external APIs, spawn a
/// subprocess, or sweep a whole guild. Recurses into subcommands because poise
/// checks the cooldown of the leaf command that actually runs.
pub fn apply_rate_limits(commands: &mut [poise::Command<Data, Error>]) {
    use std::time::Duration;
    let user = |secs| poise::CooldownConfig {
        user: Some(Duration::from_secs(secs)),
        ..Default::default()
    };
    let guild = |secs| poise::CooldownConfig {
        guild: Some(Duration::from_secs(secs)),
        ..Default::default()
    };
    for cmd in commands.iter_mut() {
        let cfg = match cmd.qualified_name.as_str() {
            "ocr" => user(5),
            "version" => user(5),
            "translate" | "define" => user(3),
            "play" => user(2),
            "tag create" | "tag edit" => user(2),
            // Whole-guild role sweep — one HTTP call per member; throttle per guild.
            "role all" | "roleall" => guild(30),
            // Baseline anti-spam for everything else.
            _ => user(1),
        };
        *cmd.cooldown_config.write().unwrap() = cfg;
        apply_rate_limits(&mut cmd.subcommands);
    }
}

// ---- shared reply helpers --------------------------------------------------
//
// Command bodies reply through these so prefix and slash invocations are
// handled uniformly (poise picks the right transport).

/// Send an embed as the command's reply.
pub async fn send_embed(ctx: Context<'_>, embed: CreateEmbed) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Send a standard error embed as the command's reply.
pub async fn send_error(ctx: Context<'_>, text: &str) -> Result<(), Error> {
    send_embed(ctx, error_embed(text)).await
}

// ---- dynamic prefix --------------------------------------------------------

/// Resolve the guild's active prefixes and strip the longest one that the
/// message starts with. Mirrors the old `AppState::parse_command` longest-match
/// so overlapping prefixes (`!` vs `!!`) resolve unambiguously, and falls back
/// to the global default for guilds without a custom prefix (and for DMs).
pub fn dynamic_prefix<'a>(
    _ctx: &'a serenity::all::Context,
    msg: &'a serenity::all::Message,
    data: &'a Data,
) -> poise::BoxFuture<'a, Result<Option<(&'a str, &'a str)>, Error>> {
    Box::pin(async move {
        let content = msg.content.as_str();
        let prefixes = data.state.guild_prefixes(msg.guild_id.map(|g| g.get()));
        let best = prefixes
            .iter()
            .filter(|p| !p.is_empty() && content.starts_with(p.as_str()))
            .map(|p| p.len())
            .max();
        Ok(best.map(|len| content.split_at(len)))
    })
}

// ---- event bridge ----------------------------------------------------------

/// Bridge serenity gateway events to the `Cog` event hooks. poise dispatches
/// commands itself; everything else is fanned out to the cogs here, preserving
/// the existing event-driven features (AFK, sentinel, logging, welcome, ...).
pub async fn event_handler(
    ctx: &serenity::all::Context,
    event: &FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    let cogs = &data.cogs;
    match event {
        FullEvent::Ready { .. } => cogs.dispatch_ready(ctx).await,
        FullEvent::Message { new_message } => {
            if new_message.author.bot {
                return Ok(());
            }
            cogs.dispatch_message(ctx, new_message).await;
        }
        FullEvent::GuildMemberAddition { new_member } => {
            cogs.dispatch_member_join(ctx, new_member).await
        }
        FullEvent::GuildMemberRemoval { guild_id, user, .. } => {
            cogs.dispatch_member_leave(ctx, *guild_id, user).await
        }
        FullEvent::GuildMemberUpdate {
            old_if_available,
            new,
            event,
        } => {
            cogs.dispatch_member_update(ctx, old_if_available.clone(), new.clone(), event)
                .await
        }
        FullEvent::MessageUpdate {
            old_if_available,
            new,
            event,
        } => {
            cogs.dispatch_message_update(ctx, old_if_available.clone(), new.clone(), event)
                .await
        }
        FullEvent::MessageDelete {
            channel_id,
            deleted_message_id,
            guild_id,
        } => {
            cogs.dispatch_message_delete(ctx, *channel_id, *deleted_message_id, *guild_id)
                .await
        }
        FullEvent::ReactionAdd { add_reaction } => {
            cogs.dispatch_reaction_add(ctx, add_reaction.clone()).await
        }
        FullEvent::GuildCreate { guild, .. } => cogs.dispatch_guild_create(ctx, guild).await,
        FullEvent::GuildDelete { incomplete, full } => {
            cogs.dispatch_guild_delete(ctx, *incomplete, full.clone())
                .await
        }
        FullEvent::GuildBanAddition {
            guild_id,
            banned_user,
        } => cogs.dispatch_member_ban(ctx, *guild_id, banned_user).await,
        FullEvent::GuildBanRemoval {
            guild_id,
            unbanned_user,
        } => cogs.dispatch_member_unban(ctx, *guild_id, unbanned_user).await,
        FullEvent::ChannelCreate { channel } => cogs.dispatch_channel_create(ctx, channel).await,
        FullEvent::ChannelDelete { channel, .. } => {
            cogs.dispatch_channel_delete(ctx, channel).await
        }
        FullEvent::GuildRoleCreate { new } => cogs.dispatch_role_create(ctx, new).await,
        FullEvent::GuildRoleDelete {
            guild_id,
            removed_role_id,
            removed_role_data_if_available,
        } => {
            cogs.dispatch_role_delete(
                ctx,
                *guild_id,
                *removed_role_id,
                removed_role_data_if_available.clone(),
            )
            .await
        }
        FullEvent::ThreadCreate { thread } => cogs.dispatch_thread_create(ctx, thread).await,
        FullEvent::VoiceStateUpdate { old, new } => {
            // Forward to lavalink-rs so it can build voice connection info.
            if let (Some(lava), Some(guild_id)) = (data.state.lavalink.get(), new.guild_id) {
                lava.handle_voice_state_update(
                    guild_id,
                    new.channel_id,
                    new.user_id,
                    new.session_id.clone(),
                );
            }
            cogs.dispatch_voice_state_update(ctx, old.clone(), new).await;
        }
        FullEvent::VoiceServerUpdate { event } => {
            if let (Some(lava), Some(guild_id)) = (data.state.lavalink.get(), event.guild_id) {
                lava.handle_voice_server_update(
                    guild_id,
                    event.token.clone(),
                    event.endpoint.clone(),
                );
            }
        }
        FullEvent::InteractionCreate { interaction } => match interaction {
            // Command/Autocomplete interactions are handled by poise itself.
            Interaction::Component(c) => cogs.dispatch_component(ctx, c).await,
            Interaction::Modal(m) => cogs.dispatch_modal(ctx, m).await,
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

// ---- framework hooks -------------------------------------------------------

/// Log a handled command invocation (replaces `CogManager::log_command`).
pub async fn pre_command(ctx: Context<'_>) {
    tracing::info!(
        guild_id = ctx.guild_id().map(|g| g.get()),
        channel_id = ctx.channel_id().get(),
        user_id = ctx.author().id.get(),
        command = %ctx.command().qualified_name,
        "command invoked by {} ({})",
        ctx.author().name,
        ctx.author().id,
    );
}

/// Central command-error handler: log the failure and surface a friendly
/// message; delegate the rest (missing-permissions, argument-parse, guild-only,
/// cooldowns, ...) to poise's built-in handler which already replies sensibly.
pub async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    if let poise::FrameworkError::Command { error, ctx, .. } = error {
        tracing::error!(
            command = %ctx.command().qualified_name,
            error = %error,
            "command returned an error",
        );
        let _ = ctx.say(format!("Something went wrong: {error}")).await;
    } else if let Err(e) = poise::builtins::on_error(error).await {
        tracing::error!(error = %e, "error while handling a framework error");
    }
}
