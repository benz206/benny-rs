use super::Cog;
use crate::state::{AppState, GoodbyeConfig, WelcomeConfig};
use crate::tagscript::{self, TagContext};
use crate::utils::parse::{parse_channel_id, parse_role_id};
use crate::utils::{colors, format};
use async_trait::async_trait;
use serde_json::Value;
use serenity::all::{
    ButtonStyle, ChannelId, Colour, ComponentInteraction, Context, CreateActionRow, CreateButton,
    CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, GuildId, Member, Message, RoleId, Timestamp,
    User,
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
        let rows: Vec<(i64, Option<i64>, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT guild_id, channel_id, message, embed_json, enabled FROM welcome_config",
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        let welcome_count = rows.len();
        for (guild_id, channel_id, message, embed_json, enabled) in rows {
            self.state.welcome_cache.insert(
                guild_id as u64,
                WelcomeConfig {
                    channel_id,
                    message,
                    embed_json,
                    enabled: enabled != 0,
                },
            );
        }

        // Hydrate the goodbye cache from goodbye_config.
        let rows: Vec<(i64, Option<i64>, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT guild_id, channel_id, message, embed_json, enabled FROM goodbye_config",
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        let goodbye_count = rows.len();
        for (guild_id, channel_id, message, embed_json, enabled) in rows {
            self.state.goodbye_cache.insert(
                guild_id as u64,
                GoodbyeConfig {
                    channel_id,
                    message,
                    embed_json,
                    enabled: enabled != 0,
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
        let table = config_table(is_welcome);
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
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (guild_id, channel_id) VALUES (?, ?) \
             ON CONFLICT(guild_id) DO UPDATE SET channel_id = excluded.channel_id"
        )))
        .bind(guild_id as i64)
        .bind(id as i64)
        .execute(self.state.servers_db())
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
        let table = config_table(is_welcome);
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (guild_id, message) VALUES (?, ?) \
             ON CONFLICT(guild_id) DO UPDATE SET message = excluded.message"
        )))
        .bind(guild_id as i64)
        .bind(arg)
        .execute(self.state.servers_db())
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
            let table = config_table(is_welcome);
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {table} (guild_id, embed_json) VALUES (?, NULL) \
                 ON CONFLICT(guild_id) DO UPDATE SET embed_json = NULL"
            )))
            .bind(guild_id as i64)
            .execute(self.state.servers_db())
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
                let table = config_table(is_welcome);
                let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "INSERT INTO {table} (guild_id, embed_json) VALUES (?, ?) \
                     ON CONFLICT(guild_id) DO UPDATE SET embed_json = excluded.embed_json"
                )))
                .bind(guild_id as i64)
                .bind(arg)
                .execute(self.state.servers_db())
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

    async fn handle_autorole_cmd(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        sub: &str,
        arg: &str,
    ) {
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
                let _ = sqlx::query("DELETE FROM welcome_autoroles WHERE guild_id = ?")
                    .bind(guild_id as i64)
                    .execute(self.state.servers_db())
                    .await;
                let _ = sqlx::query(
                    "INSERT OR IGNORE INTO welcome_autoroles (guild_id, role_id) VALUES (?, ?)",
                )
                .bind(guild_id as i64)
                .bind(role_id as i64)
                .execute(self.state.servers_db())
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
                let res = sqlx::query(
                    "INSERT OR IGNORE INTO welcome_autoroles (guild_id, role_id) VALUES (?, ?)",
                )
                .bind(guild_id as i64)
                .bind(role_id as i64)
                .execute(self.state.servers_db())
                .await;
                let text = match res {
                    Ok(r) if r.rows_affected() > 0 => format!("Added autorole <@&{role_id}>."),
                    Ok(_) => format!("<@&{role_id}> is already an autorole."),
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
                let res =
                    sqlx::query("DELETE FROM welcome_autoroles WHERE guild_id = ? AND role_id = ?")
                        .bind(guild_id as i64)
                        .bind(role_id as i64)
                        .execute(self.state.servers_db())
                        .await;
                let text = match res {
                    Ok(r) if r.rows_affected() > 0 => format!("Removed autorole <@&{role_id}>."),
                    _ => format!("<@&{role_id}> was not an autorole."),
                };
                let _ = msg.channel_id.say(&ctx.http, text).await;
            }
            "list" | "current" | "show" => {
                let rows: Vec<(i64,)> =
                    sqlx::query_as("SELECT role_id FROM welcome_autoroles WHERE guild_id = ?")
                        .bind(guild_id as i64)
                        .fetch_all(self.state.servers_db())
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
                    .map(|(id,)| format!("<@&{}>", *id as u64))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Autoroles applied on join: {list}"))
                    .await;
            }
            "clear" => {
                let _ = sqlx::query("DELETE FROM welcome_autoroles WHERE guild_id = ?")
                    .bind(guild_id as i64)
                    .execute(self.state.servers_db())
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
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT role_id FROM welcome_autoroles WHERE guild_id = ?")
                .bind(guild_id.get() as i64)
                .fetch_all(self.state.servers_db())
                .await
                .unwrap_or_default();
        for (role_id,) in rows {
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
        let row: Option<(String,)> =
            sqlx::query_as("SELECT role_ids FROM sticky_roles WHERE guild_id = ? AND user_id = ?")
                .bind(guild_id.get() as i64)
                .bind(user_id.get() as i64)
                .fetch_optional(self.state.servers_db())
                .await
                .ok()
                .flatten();
        let Some((role_ids,)) = row else {
            return;
        };
        for part in role_ids.split(',') {
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
        let _ = sqlx::query(
            "INSERT INTO sticky_roles (guild_id, user_id, role_ids) VALUES (?, ?, ?) \
             ON CONFLICT(guild_id, user_id) DO UPDATE SET role_ids = excluded.role_ids",
        )
        .bind(guild_id.get() as i64)
        .bind(user.id.get() as i64)
        .bind(ids)
        .execute(self.state.servers_db())
        .await;
    }

    // ---- small DB / cache helpers ----------------------------------------

    /// Set the enabled flag for welcome/goodbye and refresh the cache.
    async fn set_enabled(&self, guild_id: u64, is_welcome: bool, enabled: bool) {
        let table = config_table(is_welcome);
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (guild_id, enabled) VALUES (?, ?) \
             ON CONFLICT(guild_id) DO UPDATE SET enabled = excluded.enabled"
        )))
        .bind(guild_id as i64)
        .bind(enabled as i64)
        .execute(self.state.servers_db())
        .await;
        self.reload(guild_id, is_welcome).await;
    }

    /// Reload a single guild's welcome/goodbye row into the cache.
    async fn reload(&self, guild_id: u64, is_welcome: bool) {
        let table = config_table(is_welcome);
        let row: Option<(Option<i64>, String, Option<String>, i64)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT channel_id, message, embed_json, enabled FROM {table} WHERE guild_id = ?"
            )))
            .bind(guild_id as i64)
            .fetch_optional(self.state.servers_db())
            .await
            .ok()
            .flatten();
        let Some((channel_id, message, embed_json, enabled)) = row else {
            return;
        };
        if is_welcome {
            self.state.welcome_cache.insert(
                guild_id,
                WelcomeConfig {
                    channel_id,
                    message,
                    embed_json,
                    enabled: enabled != 0,
                },
            );
        } else {
            self.state.goodbye_cache.insert(
                guild_id,
                GoodbyeConfig {
                    channel_id,
                    message,
                    embed_json,
                    enabled: enabled != 0,
                },
            );
        }
    }

    async fn sticky_enabled(&self, guild_id: u64) -> bool {
        let v: Option<i64> =
            sqlx::query_scalar("SELECT enabled FROM sticky_roles_config WHERE guild_id = ?")
                .bind(guild_id as i64)
                .fetch_optional(self.state.servers_db())
                .await
                .ok()
                .flatten();
        v.map(|e| e != 0).unwrap_or(false)
    }

    async fn set_sticky_enabled(&self, guild_id: u64, enabled: bool) {
        let _ = sqlx::query(
            "INSERT INTO sticky_roles_config (guild_id, enabled) VALUES (?, ?) \
             ON CONFLICT(guild_id) DO UPDATE SET enabled = excluded.enabled",
        )
        .bind(guild_id as i64)
        .bind(enabled as i64)
        .execute(self.state.servers_db())
        .await;
    }
}

/// Table name for the welcome/goodbye config (interpolated into trusted,
/// hand-written SQL only — never user input).
fn config_table(is_welcome: bool) -> &'static str {
    if is_welcome {
        "welcome_config"
    } else {
        "goodbye_config"
    }
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
    let mut create = CreateMessage::new();
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
/// in every string field (mirrors the Python `process_embed`).
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
