//! Shared authorization helpers for command cogs.
//!
//! Previously `invoker_perms` / `require_perm` were copy-pasted into
//! `moderation.rs` and `roles.rs`; this is the single source of truth so every
//! cog gates writes the same way.

use crate::utils::embeds::error_embed;
use serenity::all::{Context, CreateMessage, GuildId, Message, Permissions, UserId};

/// Effective guild permissions for `user_id`, preferring the gateway cache and
/// falling back to a partial-guild HTTP fetch on a cold cache.
pub async fn invoker_perms(ctx: &Context, guild_id: GuildId, user_id: u64) -> Option<Permissions> {
    let member = guild_id
        .member(&ctx.http, UserId::new(user_id))
        .await
        .ok()?;
    if let Some(guild) = ctx.cache.guild(guild_id) {
        return Some(guild.member_permissions(&member));
    }
    let partial = guild_id.to_partial_guild(&ctx.http).await.ok()?;
    Some(partial.member_permissions(&member))
}

/// True when `user_id` holds ADMINISTRATOR or `perm` in `guild_id`.
pub async fn has_perm(ctx: &Context, guild_id: GuildId, user_id: u64, perm: Permissions) -> bool {
    invoker_perms(ctx, guild_id, user_id)
        .await
        .map(|p| p.contains(Permissions::ADMINISTRATOR) || p.contains(perm))
        .unwrap_or(false)
}

/// Enforce that the message author holds `perm` (ADMINISTRATOR always passes).
/// On failure, replies with an error embed and returns false.
pub async fn require_perm(
    ctx: &Context,
    msg: &Message,
    guild_id: GuildId,
    perm: Permissions,
    label: &str,
) -> bool {
    if has_perm(ctx, guild_id, msg.author.id.get(), perm).await {
        return true;
    }
    let _ = msg
        .channel_id
        .send_message(
            &ctx.http,
            CreateMessage::new().embed(error_embed(&format!(
                "You need the **{label}** permission to use this command."
            ))),
        )
        .await;
    false
}
