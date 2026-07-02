//! Starboard: Carl-bot style "wall of fame" — messages that collect enough
//! reactions of a configured emoji are mirrored into a dedicated channel.
//! Re-reacting keeps the mirrored post's star count in sync, and it is
//! removed again if the count drops back below the threshold.

use super::Cog;
use crate::entities::{starboard_config, starboard_posts};
use crate::framework::{Context, Data, Error, send_embed, send_error, send_plain};
use crate::state::AppState;
use crate::utils::colors;
use crate::utils::format::truncate;
use async_trait::async_trait;
use dashmap::DashMap;
use sea_orm::sea_query::OnConflict;
use sea_orm::{EntityTrait, Set};
use serenity::all::{
    Channel, ChannelId, CreateEmbed, CreateEmbedAuthor, CreateMessage, EditMessage, Reaction,
    Timestamp,
};
use std::sync::{Arc, LazyLock};

/// Per-guild starboard configuration, mirrored from `starboard_config`.
static CONFIG_CACHE: LazyLock<DashMap<u64, starboard_config::Model>> = LazyLock::new(DashMap::new);

pub struct StarboardCog {
    state: Arc<AppState>,
}

impl StarboardCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    /// Re-count stars on the reacted-to message and create/update/remove its
    /// starboard entry accordingly. Shared by add and remove reaction hooks.
    async fn recount(&self, ctx: &serenity::all::Context, reaction: &Reaction) {
        let Some(gid) = reaction.guild_id else {
            return;
        };
        let gid_u64 = gid.get();
        let Some(cfg) = CONFIG_CACHE.get(&gid_u64).map(|c| c.clone()) else {
            return;
        };
        if !cfg.enabled {
            return;
        }
        let Some(starboard_channel) = cfg.channel_id else {
            return;
        };

        if reaction.emoji.to_string() != cfg.emoji {
            return;
        }
        // Don't re-star posts already sitting in the starboard channel.
        if reaction.channel_id.get() == starboard_channel as u64 {
            return;
        }

        let Ok(msg) = reaction
            .channel_id
            .message(&ctx.http, reaction.message_id)
            .await
        else {
            return;
        };

        let Ok(users) = msg
            .reaction_users(&ctx.http, reaction.emoji.clone(), Some(100), None)
            .await
        else {
            return;
        };

        let count = users
            .iter()
            .filter(|u| !u.bot)
            .filter(|u| cfg.self_star || u.id != msg.author.id)
            .count() as i64;

        let gid_i64 = gid_u64 as i64;
        let msg_id_i64 = msg.id.get() as i64;
        let existing = starboard_posts::Entity::find_by_id((gid_i64, msg_id_i64))
            .one(self.state.servers_orm())
            .await
            .ok()
            .flatten();

        if count >= cfg.threshold {
            let content = format!("⭐ **{count}** | <#{}>", msg.channel_id.get());

            let starboard_message_id = match &existing {
                Some(row) => {
                    let _ = ChannelId::new(starboard_channel as u64)
                        .edit_message(
                            &ctx.http,
                            row.starboard_message_id as u64,
                            EditMessage::new().content(content),
                        )
                        .await;
                    row.starboard_message_id
                }
                None => {
                    let avatar = msg
                        .author
                        .avatar_url()
                        .unwrap_or_else(|| msg.author.default_avatar_url());
                    let mut embed = CreateEmbed::new()
                        .author(CreateEmbedAuthor::new(&msg.author.name).icon_url(avatar))
                        .description(truncate(&msg.content, 1000))
                        .field(
                            "Source",
                            format!("[Jump to message]({})", msg.link()),
                            false,
                        )
                        .timestamp(Timestamp::now())
                        .color(colors::GOLD);
                    if let Some(att) = msg.attachments.iter().find(|a| {
                        a.content_type
                            .as_deref()
                            .is_some_and(|c| c.starts_with("image/"))
                    }) {
                        embed = embed.image(att.url.clone());
                    }

                    let Ok(sent) = ChannelId::new(starboard_channel as u64)
                        .send_message(&ctx.http, CreateMessage::new().content(content).embed(embed))
                        .await
                    else {
                        return;
                    };
                    sent.id.get() as i64
                }
            };

            let active = starboard_posts::ActiveModel {
                guild_id: Set(gid_i64),
                message_id: Set(msg_id_i64),
                starboard_message_id: Set(starboard_message_id),
                star_count: Set(count),
            };
            let _ = starboard_posts::Entity::insert(active)
                .on_conflict(
                    OnConflict::columns([
                        starboard_posts::Column::GuildId,
                        starboard_posts::Column::MessageId,
                    ])
                    .update_columns([
                        starboard_posts::Column::StarboardMessageId,
                        starboard_posts::Column::StarCount,
                    ])
                    .to_owned(),
                )
                .exec(self.state.servers_orm())
                .await;
        } else if let Some(row) = existing {
            let _ = ChannelId::new(starboard_channel as u64)
                .delete_message(&ctx.http, row.starboard_message_id as u64)
                .await;
            let _ = starboard_posts::Entity::delete_by_id((gid_i64, msg_id_i64))
                .exec(self.state.servers_orm())
                .await;
        }
    }
}

#[async_trait]
impl Cog for StarboardCog {
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        let rows = starboard_config::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();
        for m in rows {
            CONFIG_CACHE.insert(m.guild_id as u64, m);
        }
        tracing::info!("Starboard cache loaded ({} guild(s))", CONFIG_CACHE.len());
    }

    async fn on_reaction_add(&self, ctx: &serenity::all::Context, reaction: Reaction) {
        self.recount(ctx, &reaction).await;
    }

    async fn on_reaction_remove(&self, ctx: &serenity::all::Context, reaction: Reaction) {
        self.recount(ctx, &reaction).await;
    }
}

// ---- config helpers ---------------------------------------------------------

/// Config row defaults for a guild that never touched the starboard.
fn default_model(gid: u64) -> starboard_config::Model {
    starboard_config::Model {
        guild_id: gid as i64,
        enabled: false,
        channel_id: None,
        threshold: 3,
        emoji: "⭐".to_string(),
        self_star: false,
    }
}

/// Load-modify-upsert a guild's starboard config and refresh the cache.
async fn update_config<F: FnOnce(&mut starboard_config::Model)>(
    state: &AppState,
    gid: u64,
    f: F,
) -> Result<starboard_config::Model, sea_orm::DbErr> {
    let mut model = starboard_config::Entity::find_by_id(gid as i64)
        .one(state.servers_orm())
        .await?
        .unwrap_or_else(|| default_model(gid));

    f(&mut model);

    let active = starboard_config::ActiveModel {
        guild_id: Set(model.guild_id),
        enabled: Set(model.enabled),
        channel_id: Set(model.channel_id),
        threshold: Set(model.threshold),
        emoji: Set(model.emoji.clone()),
        self_star: Set(model.self_star),
    };
    starboard_config::Entity::insert(active)
        .on_conflict(
            OnConflict::column(starboard_config::Column::GuildId)
                .update_columns([
                    starboard_config::Column::Enabled,
                    starboard_config::Column::ChannelId,
                    starboard_config::Column::Threshold,
                    starboard_config::Column::Emoji,
                    starboard_config::Column::SelfStar,
                ])
                .to_owned(),
        )
        .exec(state.servers_orm())
        .await?;

    CONFIG_CACHE.insert(gid, model.clone());
    Ok(model)
}

/// Run `f` on the guild's config and confirm with `msg`, handling errors
/// uniformly.
async fn apply_setting<F: FnOnce(&mut starboard_config::Model)>(
    ctx: Context<'_>,
    f: F,
    msg: String,
) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    match update_config(&ctx.data().state, gid, f).await {
        Ok(_) => send_plain(ctx, msg).await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to save starboard config");
            send_error(ctx, "Failed to save starboard config.").await
        }
    }
}

// ---- commands ---------------------------------------------------------------

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![starboard()]
}

/// Configure the starboard (wall of fame for highly-reacted messages).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Starboard",
    required_permissions = "MANAGE_GUILD",
    subcommands(
        "sb_enable",
        "sb_disable",
        "sb_channel",
        "sb_threshold",
        "sb_emoji",
        "sb_selfstar",
        "sb_config"
    ),
    subcommand_required
)]
async fn starboard(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Turn the starboard on.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_enable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.enabled = true,
        "Starboard **enabled**.".to_string(),
    )
    .await
}

/// Turn the starboard off.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_disable(ctx: Context<'_>) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.enabled = false,
        "Starboard **disabled**.".to_string(),
    )
    .await
}

/// Set the channel starred messages are mirrored into.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "channel",
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_channel(
    ctx: Context<'_>,
    #[description = "Channel to post starred messages in"] channel: Channel,
) -> Result<(), Error> {
    let id = channel.id().get() as i64;
    apply_setting(
        ctx,
        |c| c.channel_id = Some(id),
        format!("Starboard channel set to <#{id}>."),
    )
    .await
}

/// Set the number of stars a message needs to be posted.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "threshold",
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_threshold(
    ctx: Context<'_>,
    #[description = "Stars required before a message is posted"]
    #[min = 1]
    #[max = 50]
    count: i64,
) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.threshold = count,
        format!("Starboard threshold set to **{count}**."),
    )
    .await
}

/// Set the emoji that counts as a star.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "emoji",
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_emoji(
    ctx: Context<'_>,
    #[description = "Emoji to count reactions of"] emoji: String,
) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.emoji = emoji.clone(),
        format!("Starboard emoji set to {emoji}."),
    )
    .await
}

/// Allow (or disallow) a message's own author to star it.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "selfstar",
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_selfstar(
    ctx: Context<'_>,
    #[description = "Let authors star their own messages?"] allowed: bool,
) -> Result<(), Error> {
    apply_setting(
        ctx,
        |c| c.self_star = allowed,
        format!(
            "Self-starring **{}**.",
            if allowed { "allowed" } else { "disallowed" }
        ),
    )
    .await
}

/// Show the current starboard configuration.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "config",
    aliases("show"),
    category = "Starboard",
    required_permissions = "MANAGE_GUILD"
)]
async fn sb_config(ctx: Context<'_>) -> Result<(), Error> {
    let gid = ctx.guild_id().unwrap().get();
    let cfg = CONFIG_CACHE
        .get(&gid)
        .map(|c| c.clone())
        .unwrap_or_else(|| default_model(gid));

    let onoff = |b: bool| if b { "on" } else { "off" };
    let channel = cfg
        .channel_id
        .map(|c| format!("<#{c}>"))
        .unwrap_or_else(|| "not set".to_string());

    let embed = CreateEmbed::new()
        .title("Starboard configuration")
        .field("Enabled", onoff(cfg.enabled), true)
        .field("Channel", channel, true)
        .field("Threshold", cfg.threshold.to_string(), true)
        .field("Emoji", cfg.emoji.clone(), true)
        .field("Self-star", onoff(cfg.self_star), true)
        .color(colors::GOLD);
    send_embed(ctx, embed).await
}
