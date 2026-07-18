use super::Cog;
use crate::entities::{
    goodbye_config, sticky_roles, sticky_roles_config, welcome_autoroles, welcome_config,
};
use crate::framework::{Context, Data, Error, send_error, send_plain};
use crate::state::{AppState, GoodbyeConfig, WelcomeConfig};
use crate::tagscript::{self, TagContext};
use crate::utils::roles::{role_rank, top_role};
use crate::utils::{colors, config, embeds, format, interactions, perms};
use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde_json::Value;
use serenity::all::{
    ButtonStyle, Channel, ChannelId, Colour, ComponentInteraction, CreateActionRow,
    CreateAllowedMentions, CreateButton, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, GuildId, Member,
    Permissions, Role, RoleId, Timestamp, User, UserId,
};
use serenity::prelude::Mentionable;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// custom_id namespace for this cog's interactive components. Every component
/// handled here is prefixed with this; `on_component` early-returns otherwise.
const CID_PREFIX: &str = "wel:";
/// Cap on autoroles per guild — each one is an extra `add_member_role` call on
/// every join, so the list must be bounded.
const MAX_AUTOROLES: usize = 25;

pub struct WelcomeCog {
    state: Arc<AppState>,
}

impl WelcomeCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for WelcomeCog {
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        // Hydrate the welcome cache from welcome_config.
        let welcome_count = config::hydrate_cache::<welcome_config::Entity, _>(
            self.state.servers_orm(),
            &self.state.welcome_cache,
            |m| m.guild_id as u64,
            |m| WelcomeConfig {
                channel_id: m.channel_id,
                message: m.message,
                embed_json: m.embed_json,
                enabled: m.enabled,
            },
        )
        .await;

        // Hydrate the goodbye cache from goodbye_config.
        let goodbye_count = config::hydrate_cache::<goodbye_config::Entity, _>(
            self.state.servers_orm(),
            &self.state.goodbye_cache,
            |m| m.guild_id as u64,
            |m| GoodbyeConfig {
                channel_id: m.channel_id,
                message: m.message,
                embed_json: m.embed_json,
                enabled: m.enabled,
            },
        )
        .await;

        tracing::info!("Welcome cache loaded ({welcome_count} welcome, {goodbye_count} goodbye)");
    }

    async fn on_member_join(&self, ctx: &serenity::all::Context, member: &Member) {
        let guild_id = member.guild_id;
        // 1. Welcome message.
        self.send_welcome(ctx, member).await;
        // 2. Autoroles for every new member.
        self.apply_autoroles(ctx, guild_id, member.user.id).await;
        // 3. Sticky roles, if this user has saved roles and the feature is on.
        self.apply_sticky_roles(ctx, guild_id, member.user.id).await;
    }

    async fn on_member_leave(&self, ctx: &serenity::all::Context, guild_id: GuildId, user: &User) {
        // 1. Goodbye message.
        self.send_goodbye(ctx, guild_id, user).await;
        // 2. Persist the leaving member's roles so they can be re-applied on
        //    rejoin (best effort: roles come from the cache).
        self.save_sticky_roles(ctx, guild_id, user).await;
    }

    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let custom_id = interaction.data.custom_id.as_str();
        if !custom_id.starts_with(CID_PREFIX) {
            return;
        }

        // Expected: wel:<w|g>:<enable|disable>:<guild_id>:<author_id>
        let parts: Vec<&str> = custom_id.split(':').collect();
        if parts.len() != 5 {
            return;
        }
        let feature = parts[1];
        let action = parts[2];
        let guild_id: u64 = match parts[3].parse() {
            Ok(g) => g,
            Err(_) => return,
        };
        let author_id: u64 = match parts[4].parse() {
            Ok(a) => a,
            Err(_) => return,
        };

        // Only the user who opened the wizard may toggle it.
        if interaction.user.id.get() != author_id {
            interactions::respond_ephemeral_text(
                ctx,
                interaction,
                "This setup panel isn't for you.",
            )
            .await;
            return;
        }

        let is_welcome = match feature {
            "w" => true,
            "g" => false,
            _ => return,
        };
        let enable = action == "enable";

        set_enabled(&self.state, guild_id, is_welcome, enable).await;

        let embed = status_embed(&self.state, guild_id, is_welcome);
        let buttons = setup_buttons(guild_id, author_id, is_welcome);
        let response = CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .embed(embed)
                .components(vec![CreateActionRow::Buttons(buttons)]),
        );
        let _ = interaction.create_response(&ctx.http, response).await;
    }
}

impl WelcomeCog {
    // ---- send welcome / goodbye ------------------------------------------

    async fn send_welcome(&self, ctx: &serenity::all::Context, member: &Member) {
        let guild_id = member.guild_id.get();
        let config = match self.state.welcome_cache.get(&guild_id) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let Some(channel_id) = config.channel_id else {
            return;
        };
        let channel = ChannelId::new(channel_id as u64);

        let mut tctx = build_context(ctx, member.guild_id, &member.user);
        let output = tagscript::run(&config.message, &mut tctx);
        let embed = match config.embed_json.as_deref() {
            Some(json) => render_stored_embed(json, &mut tctx),
            None => output.embed.as_ref().map(embeds::json_to_embed),
        };
        send_output(ctx, channel, output.content, embed).await;
    }

    async fn send_goodbye(&self, ctx: &serenity::all::Context, guild_id: GuildId, user: &User) {
        let gid = guild_id.get();
        let config = match self.state.goodbye_cache.get(&gid) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let Some(channel_id) = config.channel_id else {
            return;
        };
        let channel = ChannelId::new(channel_id as u64);

        let mut tctx = build_context(ctx, guild_id, user);
        let output = tagscript::run(&config.message, &mut tctx);
        let embed = match config.embed_json.as_deref() {
            Some(json) => render_stored_embed(json, &mut tctx),
            None => output.embed.as_ref().map(embeds::json_to_embed),
        };
        send_output(ctx, channel, output.content, embed).await;
    }

    // ---- autoroles / sticky roles at join/leave --------------------------

    async fn apply_autoroles(
        &self,
        ctx: &serenity::all::Context,
        guild_id: GuildId,
        user_id: UserId,
    ) {
        let rows = welcome_autoroles::Entity::find()
            .filter(welcome_autoroles::Column::GuildId.eq(guild_id.get() as i64))
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();
        for row in rows {
            let role_id = row.role_id;
            let _ = ctx
                .http
                .add_member_role(
                    guild_id,
                    user_id,
                    RoleId::new(role_id as u64),
                    Some("Autorole"),
                )
                .await;
        }
    }

    async fn apply_sticky_roles(
        &self,
        ctx: &serenity::all::Context,
        guild_id: GuildId,
        user_id: UserId,
    ) {
        if !sticky_enabled(&self.state, guild_id.get()).await {
            return;
        }
        let Some(m) =
            sticky_roles::Entity::find_by_id((guild_id.get() as i64, user_id.get() as i64))
                .one(self.state.servers_orm())
                .await
                .ok()
                .flatten()
        else {
            return;
        };
        for part in m.role_ids.split(',').take(MAX_AUTOROLES) {
            if let Ok(rid) = part.trim().parse::<u64>() {
                let _ = ctx
                    .http
                    .add_member_role(guild_id, user_id, RoleId::new(rid), Some("Sticky role"))
                    .await;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }

    async fn save_sticky_roles(
        &self,
        ctx: &serenity::all::Context,
        guild_id: GuildId,
        user: &User,
    ) {
        if !sticky_enabled(&self.state, guild_id.get()).await {
            return;
        }
        // The leave event carries only a User; recover roles from the cache
        // (best effort — the member may already be evicted).
        let roles: Vec<RoleId> = ctx
            .cache
            .guild(guild_id)
            .and_then(|g| g.members.get(&user.id).map(|m| m.roles.clone()))
            .unwrap_or_default();
        if roles.is_empty() {
            return;
        }
        let ids = roles
            .iter()
            .map(|r| r.get().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = sticky_roles::Entity::insert(sticky_roles::ActiveModel {
            guild_id: Set(guild_id.get() as i64),
            user_id: Set(user.id.get() as i64),
            role_ids: Set(ids),
        })
        .on_conflict(
            OnConflict::columns([sticky_roles::Column::GuildId, sticky_roles::Column::UserId])
                .update_column(sticky_roles::Column::RoleIds)
                .to_owned(),
        )
        .exec(self.state.servers_orm())
        .await;
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![welcome(), goodbye(), autorole(), stickyrole()]
}

// ---- welcome ----------------------------------------------------------------

/// Configure welcome messages for new members.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Welcome",
    aliases("welc"),
    subcommand_required,
    subcommands(
        "welcome_setup",
        "welcome_channel",
        "welcome_message",
        "welcome_embed",
        "welcome_enable",
        "welcome_disable"
    )
)]
async fn welcome(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Show the current welcome configuration and enable/disable buttons.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "setup",
    required_permissions = "MANAGE_GUILD"
)]
async fn welcome_setup(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let author_id = ctx.author().id.get();
    let state = &ctx.data().state;
    let embed = status_embed(state, guild_id, true);
    let buttons = setup_buttons(guild_id, author_id, true);
    ctx.send(
        poise::CreateReply::default()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(buttons)]),
    )
    .await?;
    Ok(())
}

/// Set the channel where welcome messages are sent.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "channel",
    required_permissions = "MANAGE_GUILD"
)]
async fn welcome_channel(
    ctx: Context<'_>,
    #[description = "Channel to send welcome messages in"] channel: Channel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let id = channel.id().get() as i64;
    upsert_config(state, guild_id, true, ConfigField::Channel(Some(id))).await;
    reload(state, guild_id, true).await;
    ctx.say(format!("Channel set to <#{}>.", channel.id().get()))
        .await?;
    Ok(())
}

/// Set the welcome message template (TagScript supported).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "message",
    required_permissions = "MANAGE_GUILD"
)]
async fn welcome_message(
    ctx: Context<'_>,
    #[description = "Message template. Supports TagScript variables like {member.mention} and {server.name}."]
    #[rest]
    template: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    upsert_config(state, guild_id, true, ConfigField::Message(template)).await;
    reload(state, guild_id, true).await;
    ctx.say("Message template updated.").await?;
    Ok(())
}

/// Set a custom JSON embed for the welcome message, or pass "clear" to remove it.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "embed",
    required_permissions = "MANAGE_GUILD"
)]
async fn welcome_embed(
    ctx: Context<'_>,
    #[description = "JSON embed object, or \"clear\" to remove"]
    #[rest]
    json: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    if matches!(json.trim(), "clear" | "none" | "remove" | "off") {
        upsert_config(state, guild_id, true, ConfigField::Embed(None)).await;
        reload(state, guild_id, true).await;
        ctx.say("Custom embed cleared.").await?;
        return Ok(());
    }
    let parsed: Result<Value, _> = serde_json::from_str(&json);
    match parsed {
        Ok(Value::Object(_)) => {
            upsert_config(state, guild_id, true, ConfigField::Embed(Some(json))).await;
            reload(state, guild_id, true).await;
            ctx.say("Custom embed saved. It will be sent on join/leave.")
                .await?;
        }
        _ => {
            ctx.say(
                "Please provide a valid JSON embed object, or `embed clear` to remove it. \
                 Example: `welcome embed {\"title\":\"Hi {member.name}\",\"color\":5763719}`",
            )
            .await?;
        }
    }
    Ok(())
}

/// Enable welcome messages.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    required_permissions = "MANAGE_GUILD"
)]
async fn welcome_enable(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    set_enabled(state, guild_id, true, true).await;
    ctx.say("Welcome messages enabled.").await?;
    Ok(())
}

/// Disable welcome messages.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    required_permissions = "MANAGE_GUILD"
)]
async fn welcome_disable(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    set_enabled(state, guild_id, true, false).await;
    ctx.say("Welcome messages disabled.").await?;
    Ok(())
}

// ---- goodbye ----------------------------------------------------------------

/// Configure goodbye messages for departing members.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Welcome",
    aliases("leave"),
    subcommand_required,
    subcommands(
        "goodbye_setup",
        "goodbye_channel",
        "goodbye_message",
        "goodbye_embed",
        "goodbye_enable",
        "goodbye_disable"
    )
)]
async fn goodbye(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Show the current goodbye configuration and enable/disable buttons.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "setup",
    required_permissions = "MANAGE_GUILD"
)]
async fn goodbye_setup(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let author_id = ctx.author().id.get();
    let state = &ctx.data().state;
    let embed = status_embed(state, guild_id, false);
    let buttons = setup_buttons(guild_id, author_id, false);
    ctx.send(
        poise::CreateReply::default()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(buttons)]),
    )
    .await?;
    Ok(())
}

/// Set the channel where goodbye messages are sent.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "channel",
    required_permissions = "MANAGE_GUILD"
)]
async fn goodbye_channel(
    ctx: Context<'_>,
    #[description = "Channel to send goodbye messages in"] channel: Channel,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let id = channel.id().get() as i64;
    upsert_config(state, guild_id, false, ConfigField::Channel(Some(id))).await;
    reload(state, guild_id, false).await;
    ctx.say(format!("Channel set to <#{}>.", channel.id().get()))
        .await?;
    Ok(())
}

/// Set the goodbye message template (TagScript supported).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "message",
    required_permissions = "MANAGE_GUILD"
)]
async fn goodbye_message(
    ctx: Context<'_>,
    #[description = "Message template. Supports TagScript variables like {member.mention} and {server.name}."]
    #[rest]
    template: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    upsert_config(state, guild_id, false, ConfigField::Message(template)).await;
    reload(state, guild_id, false).await;
    ctx.say("Message template updated.").await?;
    Ok(())
}

/// Set a custom JSON embed for the goodbye message, or pass "clear" to remove it.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "embed",
    required_permissions = "MANAGE_GUILD"
)]
async fn goodbye_embed(
    ctx: Context<'_>,
    #[description = "JSON embed object, or \"clear\" to remove"]
    #[rest]
    json: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    if matches!(json.trim(), "clear" | "none" | "remove" | "off") {
        upsert_config(state, guild_id, false, ConfigField::Embed(None)).await;
        reload(state, guild_id, false).await;
        ctx.say("Custom embed cleared.").await?;
        return Ok(());
    }
    let parsed: Result<Value, _> = serde_json::from_str(&json);
    match parsed {
        Ok(Value::Object(_)) => {
            upsert_config(state, guild_id, false, ConfigField::Embed(Some(json))).await;
            reload(state, guild_id, false).await;
            ctx.say("Custom embed saved. It will be sent on join/leave.")
                .await?;
        }
        _ => {
            ctx.say(
                "Please provide a valid JSON embed object, or `embed clear` to remove it. \
                 Example: `goodbye embed {\"title\":\"Bye {member.name}\",\"color\":5763719}`",
            )
            .await?;
        }
    }
    Ok(())
}

/// Enable goodbye messages.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    required_permissions = "MANAGE_GUILD"
)]
async fn goodbye_enable(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    set_enabled(state, guild_id, false, true).await;
    ctx.say("Goodbye messages enabled.").await?;
    Ok(())
}

/// Disable goodbye messages.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    required_permissions = "MANAGE_GUILD"
)]
async fn goodbye_disable(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    set_enabled(state, guild_id, false, false).await;
    ctx.say("Goodbye messages disabled.").await?;
    Ok(())
}

// ---- autorole ---------------------------------------------------------------

/// Manage roles automatically assigned to new members.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Welcome",
    aliases("autoroles"),
    subcommand_required,
    subcommands(
        "autorole_set",
        "autorole_add",
        "autorole_remove",
        "autorole_list",
        "autorole_clear"
    )
)]
async fn autorole(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Replace all autoroles with a single role.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "set",
    required_permissions = "MANAGE_ROLES"
)]
async fn autorole_set(
    ctx: Context<'_>,
    #[description = "Role to assign to every new member (replaces existing autoroles)"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let role_id = role.id.get();

    if let Some(err) = autorole_block(sctx, guild_id, ctx.author().id.get(), role_id).await {
        return send_error(ctx, &err).await;
    }

    let _ = welcome_autoroles::Entity::delete_many()
        .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
        .exec(state.servers_orm())
        .await;
    let _ = welcome_autoroles::Entity::insert(welcome_autoroles::ActiveModel {
        guild_id: Set(guild_id as i64),
        role_id: Set(role_id as i64),
    })
    .on_conflict(
        OnConflict::columns([
            welcome_autoroles::Column::GuildId,
            welcome_autoroles::Column::RoleId,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec(state.servers_orm())
    .await;
    send_plain(ctx, format!("Autorole set to <@&{role_id}>.")).await
}

/// Add a role to the autorole list.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "add",
    required_permissions = "MANAGE_ROLES"
)]
async fn autorole_add(
    ctx: Context<'_>,
    #[description = "Role to add to the autorole list"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let role_id = role.id.get();

    if let Some(err) = autorole_block(sctx, guild_id, ctx.author().id.get(), role_id).await {
        return send_error(ctx, &err).await;
    }

    // Cap the list: every stored autorole is one extra `add_member_role` HTTP
    // call on every join, so an unbounded list amplifies per-join work.
    let count = welcome_autoroles::Entity::find()
        .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
        .count(state.servers_orm())
        .await
        .unwrap_or(0);
    if count as usize >= MAX_AUTOROLES {
        return send_error(
            ctx,
            &format!("You can have at most {MAX_AUTOROLES} autoroles."),
        )
        .await;
    }

    let res = welcome_autoroles::Entity::insert(welcome_autoroles::ActiveModel {
        guild_id: Set(guild_id as i64),
        role_id: Set(role_id as i64),
    })
    .on_conflict(
        OnConflict::columns([
            welcome_autoroles::Column::GuildId,
            welcome_autoroles::Column::RoleId,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec(state.servers_orm())
    .await;
    let text = match res {
        Ok(_) => format!("Added autorole <@&{role_id}>."),
        Err(DbErr::RecordNotInserted) => format!("<@&{role_id}> is already an autorole."),
        Err(_) => "Database error.".to_string(),
    };
    send_plain(ctx, text).await
}

/// Remove a role from the autorole list.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "remove",
    required_permissions = "MANAGE_ROLES"
)]
async fn autorole_remove(
    ctx: Context<'_>,
    #[description = "Role to remove from the autorole list"] role: Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let role_id = role.id.get();

    let res = welcome_autoroles::Entity::delete_many()
        .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
        .filter(welcome_autoroles::Column::RoleId.eq(role_id as i64))
        .exec(state.servers_orm())
        .await;
    let text = match res {
        Ok(r) if r.rows_affected > 0 => format!("Removed autorole <@&{role_id}>."),
        _ => format!("<@&{role_id}> was not an autorole."),
    };
    send_plain(ctx, text).await
}

/// List all configured autoroles.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "list",
    required_permissions = "MANAGE_ROLES"
)]
async fn autorole_list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;

    let rows = welcome_autoroles::Entity::find()
        .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
        .all(state.servers_orm())
        .await
        .unwrap_or_default();
    if rows.is_empty() {
        ctx.say("No autoroles configured. Add one with `autorole add <@role>`.")
            .await?;
        return Ok(());
    }
    let list = rows
        .iter()
        .map(|m| format!("<@&{}>", m.role_id as u64))
        .collect::<Vec<_>>()
        .join(", ");
    send_plain(ctx, format!("Autoroles applied on join: {list}")).await?;
    Ok(())
}

/// Remove all configured autoroles.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "clear",
    required_permissions = "MANAGE_ROLES"
)]
async fn autorole_clear(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let _ = welcome_autoroles::Entity::delete_many()
        .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
        .exec(state.servers_orm())
        .await;
    ctx.say("Cleared all autoroles.").await?;
    Ok(())
}

// ---- stickyrole -------------------------------------------------------------

/// Persist members' roles across leave/rejoin.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Welcome",
    aliases("stickyroles"),
    subcommand_required,
    subcommands("stickyrole_enable", "stickyrole_disable")
)]
async fn stickyrole(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Enable sticky roles — members' roles are restored when they rejoin.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "enable",
    required_permissions = "MANAGE_ROLES"
)]
async fn stickyrole_enable(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    set_sticky_enabled(state, guild_id, true).await;
    ctx.say("Sticky roles enabled. Members' roles will be restored when they rejoin.")
        .await?;
    Ok(())
}

/// Disable sticky roles.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "disable",
    required_permissions = "MANAGE_ROLES"
)]
async fn stickyrole_disable(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    set_sticky_enabled(state, guild_id, false).await;
    ctx.say("Sticky roles disabled.").await?;
    Ok(())
}

// ---- shared helpers ---------------------------------------------------------

/// Reject autorole targets a member shouldn't be able to hand out to every
/// future joiner: `@everyone`, managed/integration roles, and roles at or
/// above the bot's or the invoker's highest role. Returns an error string
/// when the role must be refused, or `None` when it is safe to store.
async fn autorole_block(
    ctx: &serenity::all::Context,
    guild_id: u64,
    invoker_id: u64,
    role_id: u64,
) -> Option<String> {
    let gid = GuildId::new(guild_id);
    let everyone = RoleId::new(guild_id);
    if role_id == guild_id {
        return Some("You can't use @everyone as an autorole.".to_string());
    }

    let roles = match ctx.cache.guild(gid).map(|g| g.roles.clone()) {
        Some(r) => r,
        None => gid.roles(&ctx.http).await.ok()?,
    };
    let role = match roles.get(&RoleId::new(role_id)) {
        Some(r) => r,
        None => return Some("That role no longer exists.".to_string()),
    };
    if role.managed {
        return Some(
            "That role is managed by an integration and can't be assigned.".to_string(),
        );
    }
    let target = role_rank(role);

    let bot_id = ctx.cache.current_user().id;
    if let Ok(bot) = gid.member(&ctx.http, bot_id).await
        && let Some(bot_top) = top_role(&bot.roles, &roles, everyone)
        && target >= role_rank(bot_top)
    {
        return Some(format!(
            "I can't assign <@&{role_id}> as an autorole — it isn't below my highest role."
        ));
    }

    // An admin/owner may configure any assignable role; otherwise a member
    // can't set a role above their own highest.
    if !perms::has_perm(ctx, gid, invoker_id, Permissions::ADMINISTRATOR).await
        && let Ok(member) = gid.member(&ctx.http, UserId::new(invoker_id)).await
        && let Some(inv_top) = top_role(&member.roles, &roles, everyone)
        && target >= role_rank(inv_top)
    {
        return Some(format!(
            "You can't set <@&{role_id}> as an autorole — it isn't below your highest role."
        ));
    }
    None
}

/// Upsert one column of a guild's welcome/goodbye config row. The two
/// tables are structurally identical; `is_welcome` selects which one.
/// Unset columns fall back to their schema defaults on first insert.
async fn upsert_config(state: &AppState, guild_id: u64, is_welcome: bool, field: ConfigField) {
    let gid = guild_id as i64;
    let conn = state.servers_orm();
    if is_welcome {
        use welcome_config::Column as C;
        let mut am = welcome_config::ActiveModel {
            guild_id: Set(gid),
            ..Default::default()
        };
        let col = match field {
            ConfigField::Channel(v) => {
                am.channel_id = Set(v);
                C::ChannelId
            }
            ConfigField::Message(v) => {
                am.message = Set(v);
                C::Message
            }
            ConfigField::Embed(v) => {
                am.embed_json = Set(v);
                C::EmbedJson
            }
            ConfigField::Enabled(v) => {
                am.enabled = Set(v);
                C::Enabled
            }
        };
        let _ = welcome_config::Entity::insert(am)
            .on_conflict(OnConflict::column(C::GuildId).update_column(col).to_owned())
            .exec(conn)
            .await;
    } else {
        use goodbye_config::Column as C;
        let mut am = goodbye_config::ActiveModel {
            guild_id: Set(gid),
            ..Default::default()
        };
        let col = match field {
            ConfigField::Channel(v) => {
                am.channel_id = Set(v);
                C::ChannelId
            }
            ConfigField::Message(v) => {
                am.message = Set(v);
                C::Message
            }
            ConfigField::Embed(v) => {
                am.embed_json = Set(v);
                C::EmbedJson
            }
            ConfigField::Enabled(v) => {
                am.enabled = Set(v);
                C::Enabled
            }
        };
        let _ = goodbye_config::Entity::insert(am)
            .on_conflict(OnConflict::column(C::GuildId).update_column(col).to_owned())
            .exec(conn)
            .await;
    }
}

/// Reload a single guild's welcome/goodbye row into the cache.
async fn reload(state: &AppState, guild_id: u64, is_welcome: bool) {
    if is_welcome {
        let Some(m) = welcome_config::Entity::find_by_id(guild_id as i64)
            .one(state.servers_orm())
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        state.welcome_cache.insert(
            guild_id,
            WelcomeConfig {
                channel_id: m.channel_id,
                message: m.message,
                embed_json: m.embed_json,
                enabled: m.enabled,
            },
        );
    } else {
        let Some(m) = goodbye_config::Entity::find_by_id(guild_id as i64)
            .one(state.servers_orm())
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        state.goodbye_cache.insert(
            guild_id,
            GoodbyeConfig {
                channel_id: m.channel_id,
                message: m.message,
                embed_json: m.embed_json,
                enabled: m.enabled,
            },
        );
    }
}

/// Set the enabled flag for welcome/goodbye and refresh the cache.
async fn set_enabled(state: &AppState, guild_id: u64, is_welcome: bool, enabled: bool) {
    upsert_config(state, guild_id, is_welcome, ConfigField::Enabled(enabled)).await;
    reload(state, guild_id, is_welcome).await;
}

async fn sticky_enabled(state: &AppState, guild_id: u64) -> bool {
    sticky_roles_config::Entity::find_by_id(guild_id as i64)
        .one(state.servers_orm())
        .await
        .ok()
        .flatten()
        .map(|m| m.enabled)
        .unwrap_or(false)
}

async fn set_sticky_enabled(state: &AppState, guild_id: u64, enabled: bool) {
    let _ = sticky_roles_config::Entity::insert(sticky_roles_config::ActiveModel {
        guild_id: Set(guild_id as i64),
        enabled: Set(enabled),
    })
    .on_conflict(
        OnConflict::column(sticky_roles_config::Column::GuildId)
            .update_column(sticky_roles_config::Column::Enabled)
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;
}

/// Snapshot the current config into a status embed for the setup wizard.
fn status_embed(state: &AppState, guild_id: u64, is_welcome: bool) -> CreateEmbed {
    let (channel_id, enabled, message, has_embed) = if is_welcome {
        match state.welcome_cache.get(&guild_id) {
            Some(c) => (
                c.channel_id,
                c.enabled,
                c.message.clone(),
                c.embed_json.is_some(),
            ),
            None => (None, false, String::new(), false),
        }
    } else {
        match state.goodbye_cache.get(&guild_id) {
            Some(c) => (
                c.channel_id,
                c.enabled,
                c.message.clone(),
                c.embed_json.is_some(),
            ),
            None => (None, false, String::new(), false),
        }
    };

    let title = if is_welcome { "Welcome Setup" } else { "Goodbye Setup" };
    let name = if is_welcome { "welcome" } else { "goodbye" };
    let channel_str = channel_id
        .map(|id| format!("<#{id}>"))
        .unwrap_or_else(|| "Not set".to_string());
    let message_preview = if message.is_empty() {
        "*default*".to_string()
    } else {
        format!("```{}```", format::truncate(&message, 300))
    };

    CreateEmbed::new()
        .title(title)
        .color(if enabled { colors::GREEN } else { colors::GRAY })
        .field("Status", if enabled { "Enabled" } else { "Disabled" }, true)
        .field("Channel", channel_str, true)
        .field("Custom Embed", if has_embed { "Yes" } else { "No" }, true)
        .field("Message", message_preview, false)
        .field(
            "Variables",
            "`{member}` `{member.mention}` `{member.name}` `{member.id}` `{member.avatar}` `{server.name}` `{server.member_count}`",
            false,
        )
        .footer(CreateEmbedFooter::new(format!(
            "Set channel: {name} channel <#ch>  |  Set message: {name} message <template>"
        )))
        .timestamp(Timestamp::now())
}

fn setup_buttons(guild_id: u64, author_id: u64, is_welcome: bool) -> Vec<CreateButton> {
    let f = if is_welcome { "w" } else { "g" };
    let enable = CreateButton::new(format!("wel:{f}:enable:{guild_id}:{author_id}"))
        .label("Enable")
        .style(ButtonStyle::Success)
        .emoji('✅');
    let disable = CreateButton::new(format!("wel:{f}:disable:{guild_id}:{author_id}"))
        .label("Disable")
        .style(ButtonStyle::Danger)
        .emoji('🚫');
    vec![enable, disable]
}

/// Build a TagScript context for the (joining/leaving) member.
fn build_context(ctx: &serenity::all::Context, guild_id: GuildId, user: &User) -> TagContext {
    let mut server_name = String::new();
    let mut member_count = String::new();
    let mut server_icon = String::new();
    if let Some(guild) = ctx.cache.guild(guild_id) {
        server_name = guild.name.clone();
        member_count = guild.member_count.to_string();
        server_icon = guild.icon_url().unwrap_or_default();
    }

    let mention = user.mention().to_string();
    let avatar = user.face();

    // `{member}` / `{member.*}` aliases (engine resolves vars by full name).
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("member".to_string(), mention.clone());
    vars.insert("member.mention".to_string(), mention.clone());
    vars.insert("member.name".to_string(), user.name.clone());
    vars.insert("member.id".to_string(), user.id.get().to_string());
    vars.insert("member.avatar".to_string(), avatar.clone());

    TagContext {
        user_name: user.name.clone(),
        user_mention: mention,
        user_id: user.id.get().to_string(),
        user_avatar: avatar,
        user_discriminator: user
            .discriminator
            .map(|d| d.to_string())
            .unwrap_or_default(),
        server_name,
        server_id: guild_id.get().to_string(),
        server_member_count: member_count,
        server_icon,
        vars,
        ..Default::default()
    }
}

/// Send rendered content and/or embed; no-op when both are empty.
async fn send_output(
    ctx: &serenity::all::Context,
    channel: ChannelId,
    content: String,
    embed: Option<CreateEmbed>,
) {
    let has_content = !content.trim().is_empty();
    if !has_content && embed.is_none() {
        return;
    }
    // Greeting templates are member-authored and interpolate the joining user's
    // name, so suppress all pings (@everyone / roles / arbitrary users).
    let mut create = CreateMessage::new().allowed_mentions(CreateAllowedMentions::new());
    if has_content {
        create = create.content(content);
    }
    if let Some(e) = embed {
        create = create.embed(e);
    }
    let _ = channel.send_message(&ctx.http, create).await;
}

/// Render a single string through TagScript, returning only its text body.
fn render_str(s: &str, ctx: &mut TagContext) -> String {
    tagscript::run(s, ctx).content
}

/// Build a `CreateEmbed` from a stored embed JSON object, resolving TagScript
/// in every string field.
fn render_stored_embed(json_str: &str, ctx: &mut TagContext) -> Option<CreateEmbed> {
    let v: Value = serde_json::from_str(json_str).ok()?;
    let mut embed = CreateEmbed::new();

    if let Some(t) = v.get("title").and_then(|x| x.as_str()) {
        embed = embed.title(render_str(t, ctx));
    }
    if let Some(d) = v.get("description").and_then(|x| x.as_str()) {
        embed = embed.description(render_str(d, ctx));
    }
    if let Some(c) = v.get("color").and_then(|x| x.as_u64()) {
        embed = embed.color(Colour(c as u32));
    }
    if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
        embed = embed.url(render_str(u, ctx));
    }
    if let Some(fields) = v.get("fields").and_then(|x| x.as_array()) {
        for f in fields {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("\u{200B}");
            let value = f
                .get("value")
                .and_then(|x| x.as_str())
                .unwrap_or("\u{200B}");
            let inline = f.get("inline").and_then(|x| x.as_bool()).unwrap_or(false);
            embed = embed.field(render_str(name, ctx), render_str(value, ctx), inline);
        }
    }
    if let Some(u) = v
        .get("thumbnail")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.thumbnail(render_str(u, ctx));
    }
    if let Some(u) = v
        .get("image")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.image(render_str(u, ctx));
    }
    if let Some(t) = v
        .get("footer")
        .and_then(|t| t.get("text"))
        .and_then(|x| x.as_str())
    {
        embed = embed.footer(CreateEmbedFooter::new(render_str(t, ctx)));
    }
    if let Some(author) = v.get("author")
        && let Some(name) = author.get("name").and_then(|x| x.as_str()) {
            let mut a = CreateEmbedAuthor::new(render_str(name, ctx));
            if let Some(icon) = author.get("icon_url").and_then(|x| x.as_str()) {
                a = a.icon_url(render_str(icon, ctx));
            }
            embed = embed.author(a);
        }

    Some(embed)
}

/// Which column of a guild's welcome/goodbye config row to upsert.
enum ConfigField {
    Channel(Option<i64>),
    Message(String),
    Embed(Option<String>),
    Enabled(bool),
}
