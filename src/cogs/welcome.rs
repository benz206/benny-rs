use super::Cog;
use crate::entities::{
    goodbye_config, sticky_roles, sticky_roles_config, welcome_autoroles, welcome_config,
};
use crate::state::{AppState, GoodbyeConfig, WelcomeConfig};
use crate::tagscript::{self, TagContext};
use crate::utils::parse::{parse_channel_id, parse_role_id};
use crate::utils::roles::{role_rank, top_role};
use crate::utils::{colors, format, perms};
use async_trait::async_trait;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, Set};
use serde_json::Value;
use serenity::all::{
    ButtonStyle, ChannelId, Colour, ComponentInteraction, Context, CreateActionRow,
    CreateAllowedMentions, CreateButton, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, GuildId, Member,
    Message, Permissions, RoleId, Timestamp, User, UserId,
};
use serenity::prelude::Mentionable;
use std::collections::HashMap;
use std::sync::Arc;

/// custom_id namespace for this cog's interactive components. Every component
/// handled here is prefixed with this; `on_component` early-returns otherwise.
const CID_PREFIX: &str = "wel:";

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
    async fn on_ready(&self, _ctx: &Context) {
        // Hydrate the welcome cache from welcome_config.
        let rows = welcome_config::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        let welcome_count = rows.len();
        for m in rows {
            self.state.welcome_cache.insert(
                m.guild_id as u64,
                WelcomeConfig {
                    channel_id: m.channel_id,
                    message: m.message,
                    embed_json: m.embed_json,
                    enabled: m.enabled,
                },
            );
        }

        // Hydrate the goodbye cache from goodbye_config.
        let rows = goodbye_config::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        let goodbye_count = rows.len();
        for m in rows {
            self.state.goodbye_cache.insert(
                m.guild_id as u64,
                GoodbyeConfig {
                    channel_id: m.channel_id,
                    message: m.message,
                    embed_json: m.embed_json,
                    enabled: m.enabled,
                },
            );
        }

        tracing::info!("Welcome cache loaded ({welcome_count} welcome, {goodbye_count} goodbye)");
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        let (cmd, rest) = split_first(body);

        match cmd {
            "welcome" | "welc" => {
                let (sub, arg) = split_first(rest);
                self.handle_config_cmd(ctx, msg, guild_id, true, sub, arg.trim())
                    .await;
            }
            "goodbye" | "leave" => {
                let (sub, arg) = split_first(rest);
                self.handle_config_cmd(ctx, msg, guild_id, false, sub, arg.trim())
                    .await;
            }
            "autorole" | "autoroles" => {
                let (sub, arg) = split_first(rest);
                self.handle_autorole_cmd(ctx, msg, guild_id, sub, arg.trim())
                    .await;
            }
            "stickyrole" | "stickyroles" => {
                let (sub, _) = split_first(rest);
                self.handle_stickyrole_cmd(ctx, msg, guild_id, sub).await;
            }
            _ => {}
        }
    }

    async fn on_member_join(&self, ctx: &Context, member: &Member) {
        let guild_id = member.guild_id;

        // 1. Welcome message.
        self.send_welcome(ctx, member).await;
        // 2. Autoroles for every new member.
        self.apply_autoroles(ctx, guild_id, member.user.id).await;
        // 3. Sticky roles, if this user has saved roles and the feature is on.
        self.apply_sticky_roles(ctx, guild_id, member.user.id).await;
    }

    async fn on_member_leave(&self, ctx: &Context, guild_id: GuildId, user: &User) {
        // 1. Goodbye message.
        self.send_goodbye(ctx, guild_id, user).await;
        // 2. Persist the leaving member's roles so they can be re-applied on
        //    rejoin (best effort: roles come from the cache).
        self.save_sticky_roles(ctx, guild_id, user).await;
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
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
            let _ = interaction
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("This setup panel isn't for you."),
                    ),
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

        self.set_enabled(guild_id, is_welcome, enable).await;

        let embed = self.status_embed(guild_id, is_welcome);
        let buttons = self.setup_buttons(guild_id, author_id, is_welcome);
        let response = CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .embed(embed)
                .components(vec![CreateActionRow::Buttons(buttons)]),
        );
        let _ = interaction.create_response(&ctx.http, response).await;
    }
}

impl WelcomeCog {
    // ---- configuration commands ------------------------------------------

    async fn handle_config_cmd(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        is_welcome: bool,
        sub: &str,
        arg: &str,
    ) {
        let name = if is_welcome { "welcome" } else { "goodbye" };
        // Every subcommand writes guild-wide greeting config — require Manage Server.
        if !perms::require_perm(
            ctx,
            msg,
            GuildId::new(guild_id),
            Permissions::MANAGE_GUILD,
            "Manage Server",
        )
        .await
        {
            return;
        }
        match sub {
            "setup" | "" => self.cmd_setup(ctx, msg, guild_id, is_welcome).await,
            "channel" => self.cmd_channel(ctx, msg, guild_id, is_welcome, arg).await,
            "message" | "msg" => self.cmd_message(ctx, msg, guild_id, is_welcome, arg).await,
            "embed" => self.cmd_embed(ctx, msg, guild_id, is_welcome, arg).await,
            "enable" | "on" => {
                self.set_enabled(guild_id, is_welcome, true).await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("{name} messages enabled."))
                    .await;
            }
            "disable" | "off" => {
                self.set_enabled(guild_id, is_welcome, false).await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("{name} messages disabled."))
                    .await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "Usage: `{name} setup` | `{name} channel <#ch>` | `{name} message <template>` | `{name} embed <json>` | `{name} enable` | `{name} disable`"
                        ),
                    )
                    .await;
            }
        }
    }

    async fn cmd_channel(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        is_welcome: bool,
        arg: &str,
    ) {
        let Some(id) = parse_channel_id(arg) else {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Please mention a channel or provide a channel ID.",
                )
                .await;
            return;
        };
        self.upsert_config(guild_id, is_welcome, ConfigField::Channel(Some(id as i64)))
            .await;
        self.reload(guild_id, is_welcome).await;
        let _ = msg
            .channel_id
            .say(&ctx.http, format!("Channel set to <#{id}>."))
            .await;
    }

    async fn cmd_message(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        is_welcome: bool,
        arg: &str,
    ) {
        if arg.is_empty() {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Please provide a message template. Variables: `{member}` `{member.mention}` `{member.name}` `{member.id}` `{member.avatar}` `{server.name}` `{server.member_count}`. TagScript `{embed(...)}` blocks are supported.",
                )
                .await;
            return;
        }
        self.upsert_config(guild_id, is_welcome, ConfigField::Message(arg.to_string()))
            .await;
        self.reload(guild_id, is_welcome).await;
        let _ = msg
            .channel_id
            .say(&ctx.http, "Message template updated.")
            .await;
    }

    async fn cmd_embed(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        is_welcome: bool,
        arg: &str,
    ) {
        // `embed clear` / `embed none` removes the stored embed.
        if matches!(arg, "clear" | "none" | "remove" | "off") {
            self.upsert_config(guild_id, is_welcome, ConfigField::Embed(None))
                .await;
            self.reload(guild_id, is_welcome).await;
            let _ = msg.channel_id.say(&ctx.http, "Custom embed cleared.").await;
            return;
        }

        // Otherwise expect a JSON embed object. TagScript is resolved per-field
        // at send time, so it is stored verbatim here.
        let parsed: Result<Value, _> = serde_json::from_str(arg);
        match parsed {
            Ok(Value::Object(_)) => {
                self.upsert_config(
                    guild_id,
                    is_welcome,
                    ConfigField::Embed(Some(arg.to_string())),
                )
                .await;
                self.reload(guild_id, is_welcome).await;
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Custom embed saved. It will be sent on join/leave.",
                    )
                    .await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Please provide a valid JSON embed object, or `embed clear` to remove it. Example: `welcome embed {\"title\":\"Hi {member.name}\",\"color\":5763719}`",
                    )
                    .await;
            }
        }
    }

    /// Interactive setup wizard: shows the current configuration plus
    /// Enable/Disable buttons. Channel and message are set via subcommands.
    async fn cmd_setup(&self, ctx: &Context, msg: &Message, guild_id: u64, is_welcome: bool) {
        let author_id = msg.author.id.get();
        let embed = self.status_embed(guild_id, is_welcome);
        let buttons = self.setup_buttons(guild_id, author_id, is_welcome);
        let builder = CreateMessage::new()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(buttons)]);
        let _ = msg.channel_id.send_message(&ctx.http, builder).await;
    }

    fn setup_buttons(&self, guild_id: u64, author_id: u64, is_welcome: bool) -> Vec<CreateButton> {
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

    /// Snapshot the current config into a status embed for the wizard.
    fn status_embed(&self, guild_id: u64, is_welcome: bool) -> CreateEmbed {
        let (channel_id, enabled, message, has_embed) = if is_welcome {
            match self.state.welcome_cache.get(&guild_id) {
                Some(c) => (
                    c.channel_id,
                    c.enabled,
                    c.message.clone(),
                    c.embed_json.is_some(),
                ),
                None => (None, false, String::new(), false),
            }
        } else {
            match self.state.goodbye_cache.get(&guild_id) {
                Some(c) => (
                    c.channel_id,
                    c.enabled,
                    c.message.clone(),
                    c.embed_json.is_some(),
                ),
                None => (None, false, String::new(), false),
            }
        };

        let title = if is_welcome {
            "Welcome Setup"
        } else {
            "Goodbye Setup"
        };
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

    // ---- autorole commands -----------------------------------------------

    /// Reject autorole targets a member shouldn't be able to hand out to every
    /// future joiner: `@everyone`, managed/integration roles, and roles at or
    /// above the bot's or the invoker's highest role. Returns an error string
    /// when the role must be refused, or `None` when it is safe to store.
    /// Best-effort: if the role table can't be loaded, the Manage Roles gate
    /// above is the remaining protection.
    async fn autorole_block(
        &self,
        ctx: &Context,
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

    async fn handle_autorole_cmd(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        sub: &str,
        arg: &str,
    ) {
        // Configuring autoroles can hand a role to every future member, so the
        // mutating subcommands require Manage Roles (read-only `list` stays open).
        if !matches!(sub, "list" | "current" | "show")
            && !perms::require_perm(
                ctx,
                msg,
                GuildId::new(guild_id),
                Permissions::MANAGE_ROLES,
                "Manage Roles",
            )
            .await
        {
            return;
        }
        match sub {
            // Replace the entire autorole set with a single role.
            "set" => {
                let Some(role_id) = parse_role_id(arg) else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: `autorole set <@role>`")
                        .await;
                    return;
                };
                if let Some(err) = self
                    .autorole_block(ctx, guild_id, msg.author.id.get(), role_id)
                    .await
                {
                    let _ = msg.channel_id.say(&ctx.http, err).await;
                    return;
                }
                let _ = welcome_autoroles::Entity::delete_many()
                    .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
                    .exec(self.state.servers_orm())
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
                .exec(self.state.servers_orm())
                .await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Autorole set to <@&{role_id}>."))
                    .await;
            }
            "add" => {
                let Some(role_id) = parse_role_id(arg) else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: `autorole add <@role>`")
                        .await;
                    return;
                };
                if let Some(err) = self
                    .autorole_block(ctx, guild_id, msg.author.id.get(), role_id)
                    .await
                {
                    let _ = msg.channel_id.say(&ctx.http, err).await;
                    return;
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
                .exec(self.state.servers_orm())
                .await;
                let text = match res {
                    Ok(_) => format!("Added autorole <@&{role_id}>."),
                    Err(DbErr::RecordNotInserted) => {
                        format!("<@&{role_id}> is already an autorole.")
                    }
                    Err(_) => "Database error.".to_string(),
                };
                let _ = msg.channel_id.say(&ctx.http, text).await;
            }
            "remove" | "delete" | "del" => {
                let Some(role_id) = parse_role_id(arg) else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: `autorole remove <@role>`")
                        .await;
                    return;
                };
                let res = welcome_autoroles::Entity::delete_many()
                    .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
                    .filter(welcome_autoroles::Column::RoleId.eq(role_id as i64))
                    .exec(self.state.servers_orm())
                    .await;
                let text = match res {
                    Ok(r) if r.rows_affected > 0 => format!("Removed autorole <@&{role_id}>."),
                    _ => format!("<@&{role_id}> was not an autorole."),
                };
                let _ = msg.channel_id.say(&ctx.http, text).await;
            }
            "list" | "current" | "show" => {
                let rows = welcome_autoroles::Entity::find()
                    .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
                    .all(self.state.servers_orm())
                    .await
                    .unwrap_or_default();
                if rows.is_empty() {
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            "No autoroles configured. Add one with `autorole add <@role>`.",
                        )
                        .await;
                    return;
                }
                let list = rows
                    .iter()
                    .map(|m| format!("<@&{}>", m.role_id as u64))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Autoroles applied on join: {list}"))
                    .await;
            }
            "clear" => {
                let _ = welcome_autoroles::Entity::delete_many()
                    .filter(welcome_autoroles::Column::GuildId.eq(guild_id as i64))
                    .exec(self.state.servers_orm())
                    .await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Cleared all autoroles.")
                    .await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `autorole set <@role>` | `autorole add <@role>` | `autorole remove <@role>` | `autorole list` | `autorole clear`",
                    )
                    .await;
            }
        }
    }

    // ---- sticky role commands --------------------------------------------

    async fn handle_stickyrole_cmd(&self, ctx: &Context, msg: &Message, guild_id: u64, sub: &str) {
        // Toggling sticky roles is a guild-wide policy change — require Manage
        // Server (the status readout stays open).
        if matches!(sub, "enable" | "on" | "disable" | "off")
            && !perms::require_perm(
                ctx,
                msg,
                GuildId::new(guild_id),
                Permissions::MANAGE_GUILD,
                "Manage Server",
            )
            .await
        {
            return;
        }
        match sub {
            "enable" | "on" => {
                self.set_sticky_enabled(guild_id, true).await;
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Sticky roles enabled. Members' roles will be restored when they rejoin.",
                    )
                    .await;
            }
            "disable" | "off" => {
                self.set_sticky_enabled(guild_id, false).await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Sticky roles disabled.")
                    .await;
            }
            _ => {
                let enabled = self.sticky_enabled(guild_id).await;
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "Sticky roles are currently **{}**. Usage: `stickyrole enable` | `stickyrole disable`",
                            if enabled { "enabled" } else { "disabled" }
                        ),
                    )
                    .await;
            }
        }
    }

    // ---- send welcome / goodbye ------------------------------------------

    async fn send_welcome(&self, ctx: &Context, member: &Member) {
        let guild_id = member.guild_id.get();
        let config = match self.state.welcome_cache.get(&guild_id) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let Some(channel_id) = config.channel_id else {
            return;
        };
        let channel = ChannelId::new(channel_id as u64);

        let mut tctx = self.build_context(ctx, member.guild_id, &member.user);
        let output = tagscript::run(&config.message, &mut tctx);
        let embed = match config.embed_json.as_deref() {
            Some(json) => render_stored_embed(json, &mut tctx),
            None => output.embed.as_ref().map(json_to_embed),
        };
        send_output(ctx, channel, output.content, embed).await;
    }

    async fn send_goodbye(&self, ctx: &Context, guild_id: GuildId, user: &User) {
        let gid = guild_id.get();
        let config = match self.state.goodbye_cache.get(&gid) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let Some(channel_id) = config.channel_id else {
            return;
        };
        let channel = ChannelId::new(channel_id as u64);

        let mut tctx = self.build_context(ctx, guild_id, user);
        let output = tagscript::run(&config.message, &mut tctx);
        let embed = match config.embed_json.as_deref() {
            Some(json) => render_stored_embed(json, &mut tctx),
            None => output.embed.as_ref().map(json_to_embed),
        };
        send_output(ctx, channel, output.content, embed).await;
    }

    /// Build a TagScript context for the (joining/leaving) member. Populates
    /// the engine's native `{user.*}`/`{server.*}` fields and aliases the
    /// `{member.*}` family through `vars` (the engine has no `member` base).
    fn build_context(&self, ctx: &Context, guild_id: GuildId, user: &User) -> TagContext {
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

    // ---- autoroles / sticky roles at join/leave --------------------------

    async fn apply_autoroles(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        user_id: serenity::all::UserId,
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
        ctx: &Context,
        guild_id: GuildId,
        user_id: serenity::all::UserId,
    ) {
        if !self.sticky_enabled(guild_id.get()).await {
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
        for part in m.role_ids.split(',') {
            if let Ok(rid) = part.trim().parse::<u64>() {
                let _ = ctx
                    .http
                    .add_member_role(guild_id, user_id, RoleId::new(rid), Some("Sticky role"))
                    .await;
            }
        }
    }

    async fn save_sticky_roles(&self, ctx: &Context, guild_id: GuildId, user: &User) {
        if !self.sticky_enabled(guild_id.get()).await {
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

    // ---- small DB / cache helpers ----------------------------------------

    /// Set the enabled flag for welcome/goodbye and refresh the cache.
    /// Upsert one column of a guild's welcome/goodbye config row. The two
    /// tables are structurally identical; `is_welcome` selects which one.
    /// Unset columns fall back to their schema defaults on first insert.
    async fn upsert_config(&self, guild_id: u64, is_welcome: bool, field: ConfigField) {
        let gid = guild_id as i64;
        let conn = self.state.servers_orm();
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

    async fn set_enabled(&self, guild_id: u64, is_welcome: bool, enabled: bool) {
        self.upsert_config(guild_id, is_welcome, ConfigField::Enabled(enabled))
            .await;
        self.reload(guild_id, is_welcome).await;
    }

    /// Reload a single guild's welcome/goodbye row into the cache.
    async fn reload(&self, guild_id: u64, is_welcome: bool) {
        if is_welcome {
            let Some(m) = welcome_config::Entity::find_by_id(guild_id as i64)
                .one(self.state.servers_orm())
                .await
                .ok()
                .flatten()
            else {
                return;
            };
            self.state.welcome_cache.insert(
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
                .one(self.state.servers_orm())
                .await
                .ok()
                .flatten()
            else {
                return;
            };
            self.state.goodbye_cache.insert(
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

    async fn sticky_enabled(&self, guild_id: u64) -> bool {
        sticky_roles_config::Entity::find_by_id(guild_id as i64)
            .one(self.state.servers_orm())
            .await
            .ok()
            .flatten()
            .map(|m| m.enabled)
            .unwrap_or(false)
    }

    async fn set_sticky_enabled(&self, guild_id: u64, enabled: bool) {
        let _ = sticky_roles_config::Entity::insert(sticky_roles_config::ActiveModel {
            guild_id: Set(guild_id as i64),
            enabled: Set(enabled),
        })
        .on_conflict(
            OnConflict::column(sticky_roles_config::Column::GuildId)
                .update_column(sticky_roles_config::Column::Enabled)
                .to_owned(),
        )
        .exec(self.state.servers_orm())
        .await;
    }
}

/// Which column of a guild's welcome/goodbye config row to upsert.
enum ConfigField {
    Channel(Option<i64>),
    Message(String),
    Embed(Option<String>),
    Enabled(bool),
}

/// Split off the first whitespace-delimited token, returning it plus the
/// remainder (leading whitespace trimmed).
fn split_first(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim_start()),
        None => (s, ""),
    }
}

/// Send rendered content and/or embed; no-op when both are empty.
async fn send_output(
    ctx: &Context,
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
    if let Some(author) = v.get("author") {
        if let Some(name) = author.get("name").and_then(|x| x.as_str()) {
            let mut a = CreateEmbedAuthor::new(render_str(name, ctx));
            if let Some(icon) = author.get("icon_url").and_then(|x| x.as_str()) {
                a = a.icon_url(render_str(icon, ctx));
            }
            embed = embed.author(a);
        }
    }

    Some(embed)
}

/// Deserialize an already-rendered TagScript embed JSON object into a
/// `CreateEmbed` (no TagScript resolution; mirrors `tags.rs::json_to_embed`).
fn json_to_embed(v: &Value) -> CreateEmbed {
    let mut embed = CreateEmbed::new();

    if let Some(title) = v.get("title").and_then(|x| x.as_str()) {
        embed = embed.title(title);
    }
    if let Some(desc) = v.get("description").and_then(|x| x.as_str()) {
        embed = embed.description(desc);
    }
    if let Some(color) = v.get("color").and_then(|x| x.as_u64()) {
        embed = embed.color(Colour(color as u32));
    }
    if let Some(url) = v.get("url").and_then(|x| x.as_str()) {
        embed = embed.url(url);
    }
    if let Some(fields) = v.get("fields").and_then(|x| x.as_array()) {
        for f in fields {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("\u{200B}");
            let value = f
                .get("value")
                .and_then(|x| x.as_str())
                .unwrap_or("\u{200B}");
            let inline = f.get("inline").and_then(|x| x.as_bool()).unwrap_or(false);
            embed = embed.field(name, value, inline);
        }
    }
    if let Some(url) = v
        .get("thumbnail")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.thumbnail(url);
    }
    if let Some(url) = v
        .get("image")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.image(url);
    }
    if let Some(text) = v
        .get("footer")
        .and_then(|t| t.get("text"))
        .and_then(|x| x.as_str())
    {
        embed = embed.footer(CreateEmbedFooter::new(text));
    }
    if let Some(author) = v.get("author") {
        if let Some(name) = author.get("name").and_then(|x| x.as_str()) {
            let mut a = CreateEmbedAuthor::new(name);
            if let Some(icon) = author.get("icon_url").and_then(|x| x.as_str()) {
                a = a.icon_url(icon);
            }
            embed = embed.author(a);
        }
    }

    embed
}
