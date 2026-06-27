use super::Cog;
use crate::state::AppState;
use crate::utils::colors;
use crate::utils::embeds::error_embed;
use crate::utils::format::loading_bar;
use crate::utils::parse::{parse_role_id, parse_user_id};
use async_trait::async_trait;
use dashmap::DashMap;
use serenity::all::{
    ButtonStyle, ComponentInteraction, Context, CreateActionRow, CreateButton, CreateEmbed,
    CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    EditMessage, GuildId, Http, Member, Message, Permissions, Role, RoleId, Timestamp, UserId,
};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::{Duration, sleep};

/// Component custom_id namespace for this cog. `on_component` early-returns on
/// anything that does not begin with this.
const CID_PREFIX: &str = "role:";
/// Delay between bulk role API calls (DESIGN 7.16: "0.5s sleep between API
/// calls"). The Python `RoleAllView`/`RoleRallView` sleeps 1s; benny-rs follows
/// the spec value.
const BULK_DELAY_MS: u64 = 500;
/// Edit the progress message every N members during a bulk apply.
const PROGRESS_EVERY: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkAction {
    Add,
    Remove,
}

impl BulkAction {
    fn verb(self) -> &'static str {
        match self {
            BulkAction::Add => "add",
            BulkAction::Remove => "remove",
        }
    }
    fn past(self) -> &'static str {
        match self {
            BulkAction::Add => "Added",
            BulkAction::Remove => "Removed",
        }
    }
    fn prep(self) -> &'static str {
        match self {
            BulkAction::Add => "to",
            BulkAction::Remove => "from",
        }
    }
    fn title(self) -> &'static str {
        match self {
            BulkAction::Add => "Bulk Role Add",
            BulkAction::Remove => "Bulk Role Remove",
        }
    }
}

/// A bulk role operation awaiting Start/Cancel, keyed in `pending` by the
/// confirmation message id. Mirrors the `members`/`role` captured by Python's
/// `RoleAllView`.
struct PendingRoleAll {
    guild_id: u64,
    role_id: u64,
    action: BulkAction,
    author_id: u64,
    member_ids: Vec<u64>,
}

pub struct RolesCog {
    state: Arc<AppState>,
    /// confirmation message id -> the bulk op it confirms.
    pending: DashMap<u64, PendingRoleAll>,
}

impl RolesCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            pending: DashMap::new(),
        })
    }
}

// ---- role hierarchy helpers ----------------------------------------------

/// Ordering key reproducing discord.py's `Role.__lt__`: a role is "lower" when
/// its position is lower, or — at equal position — when its id is larger
/// (created later). `role_rank(a) < role_rank(b)` matches Python `a < b`.
fn role_rank(r: &Role) -> (i64, Reverse<u64>) {
    (r.position as i64, Reverse(r.id.get()))
}

/// The highest role a member holds, always including `@everyone` (id == guild
/// id) so the result is never `None` for a member of the guild.
fn top_role<'a>(
    member_roles: &[RoleId],
    roles: &'a HashMap<RoleId, Role>,
    everyone_id: RoleId,
) -> Option<&'a Role> {
    member_roles
        .iter()
        .filter_map(|rid| roles.get(rid))
        .chain(roles.get(&everyone_id))
        .max_by_key(|r| role_rank(r))
}

/// Resolve a role token (`<@&id>`, bare id, or case-insensitive name) to a guild
/// role. Returns the live `Role` so callers can read its position and colour.
fn resolve_role<'a>(roles: &'a HashMap<RoleId, Role>, token: &str) -> Option<&'a Role> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if let Some(id) = parse_role_id(token) {
        if let Some(r) = roles.get(&RoleId::new(id)) {
            return Some(r);
        }
    }
    let lower = token.to_lowercase();
    roles.values().find(|r| r.name.to_lowercase() == lower)
}

#[async_trait]
impl Cog for RolesCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let guild_id = match msg.guild_id {
            Some(g) => g,
            None => return,
        };
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        let mut it = body.splitn(2, ' ');
        let Some(cmd) = it.next() else { return };
        let rest = it.next().unwrap_or("").trim();

        match cmd {
            // Top-level bulk command: `roleall <role>` / `roleall remove <role>`.
            "roleall" => {
                let (action, role_token) = parse_bulk_args(rest);
                self.cmd_roleall(ctx, msg, guild_id, action, role_token)
                    .await;
            }
            // The `role` command group.
            "role" => {
                let mut sit = rest.splitn(2, ' ');
                let sub = sit.next().unwrap_or("");
                let sub_rest = sit.next().unwrap_or("").trim();
                match sub {
                    "add" => self.cmd_role_add(ctx, msg, guild_id, sub_rest).await,
                    "remove" => self.cmd_role_remove(ctx, msg, guild_id, sub_rest).await,
                    "custom" | "c" => self.cmd_role_custom(ctx, msg, guild_id, sub_rest).await,
                    // `role all`/`role bulk <role>` mirror base.py's `role all`.
                    "all" | "bulk" => {
                        let (action, role_token) = parse_bulk_args(sub_rest);
                        self.cmd_roleall(ctx, msg, guild_id, action, role_token)
                            .await;
                    }
                    _ => {
                        self.reply_error(
                            ctx,
                            msg,
                            "Usage: `role add|remove <@member> <@role>`, \
                             `role custom <@member> <+role|-role|!role ...>`, or `roleall <@role>`",
                        )
                        .await;
                    }
                }
            }
            _ => {}
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let custom_id = interaction.data.custom_id.as_str();
        if !custom_id.starts_with(CID_PREFIX) {
            return;
        }

        // Expected: role:<start|cancel>:<author_id>
        let parts: Vec<&str> = custom_id.split(':').collect();
        if parts.len() != 3 {
            return;
        }
        let action = parts[1];
        let author_id: u64 = match parts[2].parse() {
            Ok(a) => a,
            Err(_) => return,
        };

        // Only the invoker may resolve their own confirmation.
        if interaction.user.id.get() != author_id {
            let _ = interaction
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("This confirmation isn't for you."),
                    ),
                )
                .await;
            return;
        }

        let message_id = interaction.message.id.get();

        match action {
            "cancel" => {
                self.pending.remove(&message_id);
                // Faithful to RoleAllView/RoleRallView: the prompt is deleted.
                let _ = interaction
                    .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
                    .await;
                let _ = interaction.message.delete(&ctx.http).await;
            }
            "start" => {
                let Some((_, op)) = self.pending.remove(&message_id) else {
                    // Op already consumed or evicted (e.g. after a restart).
                    let _ = interaction
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .ephemeral(true)
                                    .content("This confirmation has expired."),
                            ),
                        )
                        .await;
                    return;
                };

                // Acknowledge by flipping the prompt into the in-progress state
                // and stripping the buttons, then run the apply off the gateway.
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::UpdateMessage(
                            CreateInteractionResponseMessage::new()
                                .embed(in_progress_embed(op.action, op.role_id))
                                .components(vec![]),
                        ),
                    )
                    .await;

                let http = ctx.http.clone();
                let channel_id = interaction.channel_id;
                tokio::spawn(async move {
                    run_bulk(http, channel_id, message_id, op).await;
                });
            }
            _ => {}
        }
    }
}

/// Split the argument tail of a bulk command into `(action, role_token)`. A
/// leading `remove` selects removal; everything else is the role (which may be a
/// multi-word role name).
fn parse_bulk_args(rest: &str) -> (BulkAction, &str) {
    if let Some(r) = rest.strip_prefix("remove ") {
        (BulkAction::Remove, r.trim())
    } else if rest == "remove" {
        (BulkAction::Remove, "")
    } else {
        (BulkAction::Add, rest)
    }
}

impl RolesCog {
    async fn reply_error(&self, ctx: &Context, msg: &Message, text: &str) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(error_embed(text)))
            .await;
    }

    async fn reply_embed(&self, ctx: &Context, msg: &Message, embed: CreateEmbed) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    /// Effective guild permissions of `user_id`, preferring the cached guild and
    /// falling back to a partial-guild fetch (mirrors ModerationCog).
    async fn invoker_perms(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        user_id: u64,
    ) -> Option<Permissions> {
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

    /// Enforce that the invoker holds all of `required` (ADMINISTRATOR always
    /// passes). Sends an error embed and returns false on failure.
    async fn require_perms(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        required: Permissions,
        label: &str,
    ) -> bool {
        let ok = self
            .invoker_perms(ctx, guild_id, msg.author.id.get())
            .await
            .map(|p| p.contains(Permissions::ADMINISTRATOR) || p.contains(required))
            .unwrap_or(false);
        if !ok {
            self.reply_error(
                ctx,
                msg,
                &format!("You need the **{label}** permission to use this command."),
            )
            .await;
        }
        ok
    }

    /// The bot's own top-role rank in this guild, used to refuse assigning roles
    /// at or above the bot's hierarchy position.
    async fn bot_top_rank(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        roles: &HashMap<RoleId, Role>,
    ) -> (i64, Reverse<u64>) {
        let everyone = RoleId::new(guild_id.get());
        let bot_id = ctx.cache.current_user().id;
        let default = role_rank(roles.get(&everyone).expect("@everyone always present"));
        match guild_id.member(&ctx.http, bot_id).await {
            Ok(bot) => top_role(&bot.roles, roles, everyone)
                .map(role_rank)
                .unwrap_or(default),
            Err(_) => default,
        }
    }

    // ---- single-member commands ------------------------------------------

    async fn cmd_role_add(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perms(
                ctx,
                msg,
                guild_id,
                Permissions::MANAGE_ROLES,
                "Manage Roles",
            )
            .await
        {
            return;
        }
        let mut a = rest.splitn(2, ' ');
        let member_token = a.next().unwrap_or("").trim();
        let role_token = a.next().unwrap_or("").trim();
        let Some(member_id) = parse_user_id(member_token) else {
            self.reply_error(ctx, msg, "Usage: `role add <@member> <@role>`")
                .await;
            return;
        };

        let Some((roles, author_member, target_member)) =
            self.fetch_ctx(ctx, msg, guild_id, member_id).await
        else {
            return;
        };
        let everyone = RoleId::new(guild_id.get());

        let Some(role) = resolve_role(&roles, role_token) else {
            self.reply_error(ctx, msg, "Could not find that role.")
                .await;
            return;
        };
        let role_id = role.id;
        let role_rank_v = role_rank(role);
        let role_colour = role.colour;

        let author_top = top_role(&author_member.roles, &roles, everyone).unwrap();
        let member_top = top_role(&target_member.roles, &roles, everyone).unwrap();

        if role_rank(author_top) < role_rank(member_top) {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "You cannot add <@&{role_id}> to <@{member_id}> as their highest role is \
                     higher than your highest role (<@&{}>).",
                    member_top.id
                ),
            )
            .await;
            return;
        }
        if role_rank_v > role_rank(member_top) {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "You cannot add <@&{role_id}> to <@{member_id}> as it's higher than their \
                     top role (<@&{}>).",
                    member_top.id
                ),
            )
            .await;
            return;
        }

        let bot_top = self.bot_top_rank(ctx, guild_id, &roles).await;
        if role_rank_v >= bot_top {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "I cannot assign <@&{role_id}> as it is not below my highest role in the \
                     hierarchy."
                ),
            )
            .await;
            return;
        }

        if let Err(e) = ctx
            .http
            .add_member_role(guild_id, UserId::new(member_id), role_id, None)
            .await
        {
            self.reply_error(ctx, msg, &format!("Failed to add role: {e}"))
                .await;
            return;
        }

        let embed = CreateEmbed::new()
            .title("Role Added")
            .description(format!("Added <@&{role_id}> to <@{member_id}>"))
            .color(role_colour)
            .footer(CreateEmbedFooter::new(format!("Role ID: {role_id}")))
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_role_remove(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perms(
                ctx,
                msg,
                guild_id,
                Permissions::MANAGE_ROLES,
                "Manage Roles",
            )
            .await
        {
            return;
        }
        let mut a = rest.splitn(2, ' ');
        let member_token = a.next().unwrap_or("").trim();
        let role_token = a.next().unwrap_or("").trim();
        let Some(member_id) = parse_user_id(member_token) else {
            self.reply_error(ctx, msg, "Usage: `role remove <@member> <@role>`")
                .await;
            return;
        };

        let Some((roles, author_member, target_member)) =
            self.fetch_ctx(ctx, msg, guild_id, member_id).await
        else {
            return;
        };
        let everyone = RoleId::new(guild_id.get());

        let Some(role) = resolve_role(&roles, role_token) else {
            self.reply_error(ctx, msg, "Could not find that role.")
                .await;
            return;
        };
        let role_id = role.id;
        let role_colour = role.colour;

        let author_top = top_role(&author_member.roles, &roles, everyone).unwrap();
        let member_top = top_role(&target_member.roles, &roles, everyone).unwrap();

        if role_rank(author_top) < role_rank(member_top) {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "You cannot remove <@&{role_id}> from <@{member_id}> as their highest role \
                     (<@&{}>) is higher than your highest role (<@&{}>).",
                    member_top.id, author_top.id
                ),
            )
            .await;
            return;
        }
        if role_id == member_top.id {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "You cannot remove <@&{role_id}> from <@{member_id}> as it's their highest \
                     role (<@&{}>).",
                    member_top.id
                ),
            )
            .await;
            return;
        }

        if let Err(e) = ctx
            .http
            .remove_member_role(guild_id, UserId::new(member_id), role_id, None)
            .await
        {
            self.reply_error(ctx, msg, &format!("Failed to remove role: {e}"))
                .await;
            return;
        }

        let embed = CreateEmbed::new()
            .title("Role Removed")
            .description(format!("Removed <@&{role_id}> from <@{member_id}>"))
            .color(role_colour)
            .footer(CreateEmbedFooter::new(format!("Role ID: {role_id}")))
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    /// `role custom <member> <+role|-role|!role ...>`: `+` adds, `-` removes, `!`
    /// toggles per current membership. Replicates base.py's `role_custom_command`.
    async fn cmd_role_custom(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perms(
                ctx,
                msg,
                guild_id,
                Permissions::MANAGE_ROLES,
                "Manage Roles",
            )
            .await
        {
            return;
        }
        let mut a = rest.splitn(2, ' ');
        let member_token = a.next().unwrap_or("").trim();
        let role_str = a.next().unwrap_or("").trim();
        let Some(member_id) = parse_user_id(member_token) else {
            self.reply_error(
                ctx,
                msg,
                "Usage: `role custom <@member> <+role|-role|!role ...>`",
            )
            .await;
            return;
        };
        if role_str.is_empty() {
            self.reply_error(
                ctx,
                msg,
                "Usage: `role custom <@member> <+role|-role|!role ...>`",
            )
            .await;
            return;
        }

        let Some((roles, author_member, target_member)) =
            self.fetch_ctx(ctx, msg, guild_id, member_id).await
        else {
            return;
        };
        let everyone = RoleId::new(guild_id.get());

        // Partition tokens into add/remove sets (+ add, - remove, ! toggle).
        let mut add: Vec<RoleId> = Vec::new();
        let mut remove: Vec<RoleId> = Vec::new();
        for token in role_str.split_whitespace() {
            let sigil = token.chars().next();
            // Tokens without a +/-/! sigil are ignored, like base.py.
            if !matches!(sigil, Some('+') | Some('-') | Some('!')) {
                continue;
            }
            let name = &token[1..]; // safe: matched sigils are 1-byte ASCII
            let Some(role) = resolve_role(&roles, name) else {
                self.reply_error(ctx, msg, &format!("Could not find role `{name}`."))
                    .await;
                return;
            };
            match sigil {
                Some('+') => add.push(role.id),
                Some('-') => remove.push(role.id),
                Some('!') => {
                    if target_member.roles.contains(&role.id) {
                        remove.push(role.id);
                    } else {
                        add.push(role.id);
                    }
                }
                _ => {}
            }
        }

        let author_top = top_role(&author_member.roles, &roles, everyone).unwrap();
        let member_top = top_role(&target_member.roles, &roles, everyone).unwrap();

        if role_rank(author_top) < role_rank(member_top) {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "You cannot manage these roles from <@{member_id}> as their highest role \
                     (<@&{}>) is higher than your highest role (<@&{}>).",
                    member_top.id, author_top.id
                ),
            )
            .await;
            return;
        }
        for rid in &add {
            if let Some(r) = roles.get(rid) {
                if role_rank(r) > role_rank(member_top) {
                    self.reply_error(
                        ctx,
                        msg,
                        &format!(
                            "You cannot add <@&{rid}> to <@{member_id}> as it's higher than their \
                             top role (<@&{}>).",
                            member_top.id
                        ),
                    )
                    .await;
                    return;
                }
            }
        }
        for rid in &remove {
            if *rid == member_top.id {
                self.reply_error(
                    ctx,
                    msg,
                    &format!(
                        "You cannot remove <@&{rid}> from <@{member_id}> as it's their highest \
                         role (<@&{}>).",
                        member_top.id
                    ),
                )
                .await;
                return;
            }
        }

        // Bot hierarchy guard on every role we will add.
        let bot_top = self.bot_top_rank(ctx, guild_id, &roles).await;
        for rid in &add {
            if let Some(r) = roles.get(rid) {
                if role_rank(r) >= bot_top {
                    self.reply_error(
                        ctx,
                        msg,
                        &format!(
                            "I cannot assign <@&{rid}> as it is not below my highest role in the \
                             hierarchy."
                        ),
                    )
                    .await;
                    return;
                }
            }
        }

        for rid in &add {
            let _ = ctx
                .http
                .add_member_role(guild_id, UserId::new(member_id), *rid, None)
                .await;
        }
        for rid in &remove {
            let _ = ctx
                .http
                .remove_member_role(guild_id, UserId::new(member_id), *rid, None)
                .await;
        }

        // Colour/footer come from the last touched role (remove wins over add),
        // matching the trailing `role` binding in base.py.
        let last = remove.last().or_else(|| add.last()).copied();
        let colour = last
            .and_then(|rid| roles.get(&rid))
            .map(|r| r.colour)
            .unwrap_or(colors::BLURPLE);

        let added_str = if add.is_empty() {
            "None".to_string()
        } else {
            add.iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let removed_str = if remove.is_empty() {
            "None".to_string()
        } else {
            remove
                .iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut embed = CreateEmbed::new()
            .title("Role Custom")
            .color(colour)
            .timestamp(Timestamp::now())
            .field("Added", added_str, true)
            .field("Removed", removed_str, true);
        if let Some(rid) = last {
            embed = embed.footer(CreateEmbedFooter::new(format!("Role ID: {rid}")));
        }
        self.reply_embed(ctx, msg, embed).await;
    }

    /// Fetch the roles map plus the invoking and target members, replying with an
    /// error embed on any failure. Returns `None` if the command should abort.
    async fn fetch_ctx(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        member_id: u64,
    ) -> Option<(HashMap<RoleId, Role>, Member, Member)> {
        let roles = match guild_id.roles(&ctx.http).await {
            Ok(r) => r,
            Err(e) => {
                self.reply_error(ctx, msg, &format!("Failed to fetch roles: {e}"))
                    .await;
                return None;
            }
        };
        let author_member = match guild_id.member(&ctx.http, msg.author.id).await {
            Ok(m) => m,
            Err(e) => {
                self.reply_error(ctx, msg, &format!("Failed to fetch your member info: {e}"))
                    .await;
                return None;
            }
        };
        let target_member = match guild_id.member(&ctx.http, UserId::new(member_id)).await {
            Ok(m) => m,
            Err(_) => {
                self.reply_error(ctx, msg, "Could not find that member.")
                    .await;
                return None;
            }
        };
        Some((roles, author_member, target_member))
    }

    // ---- bulk command ----------------------------------------------------

    async fn cmd_roleall(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        action: BulkAction,
        role_token: &str,
    ) {
        // DESIGN 7.16: requires Manage Roles + Manage Guild.
        if !self
            .require_perms(
                ctx,
                msg,
                guild_id,
                Permissions::MANAGE_ROLES | Permissions::MANAGE_GUILD,
                "Manage Roles and Manage Server",
            )
            .await
        {
            return;
        }

        if role_token.is_empty() {
            self.reply_error(
                ctx,
                msg,
                "Usage: `roleall <@role>` | `roleall remove <@role>`",
            )
            .await;
            return;
        }

        let roles = match guild_id.roles(&ctx.http).await {
            Ok(r) => r,
            Err(e) => {
                self.reply_error(ctx, msg, &format!("Failed to fetch roles: {e}"))
                    .await;
                return;
            }
        };
        let Some(role) = resolve_role(&roles, role_token) else {
            self.reply_error(ctx, msg, "Could not find that role.")
                .await;
            return;
        };
        let role_id = role.id;
        let role_rank_v = role_rank(role);

        // The bot must sit above the role to manage it for anyone.
        let bot_top = self.bot_top_rank(ctx, guild_id, &roles).await;
        if role_rank_v >= bot_top {
            self.reply_error(
                ctx,
                msg,
                &format!(
                    "I cannot manage <@&{role_id}> as it is not below my highest role in the \
                     hierarchy."
                ),
            )
            .await;
            return;
        }

        let status = msg
            .channel_id
            .say(&ctx.http, "Fetching members...")
            .await
            .ok();

        let members = match fetch_all_members(&ctx.http, guild_id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = ?e, "failed to fetch members for roleall");
                self.reply_error(ctx, msg, "Failed to fetch members.").await;
                return;
            }
        };

        // Affected = non-bot members who would actually change. For Add: those
        // lacking the role; for Remove: those holding it.
        let member_ids: Vec<u64> = members
            .iter()
            .filter(|m| !m.user.bot)
            .filter(|m| match action {
                BulkAction::Add => !m.roles.contains(&role_id),
                BulkAction::Remove => m.roles.contains(&role_id),
            })
            .map(|m| m.user.id.get())
            .collect();
        let count = member_ids.len();

        let author_id = msg.author.id.get();
        let confirm = CreateButton::new(format!("role:start:{author_id}"))
            .label("Start")
            .style(ButtonStyle::Success)
            .emoji('✅');
        let cancel = CreateButton::new(format!("role:cancel:{author_id}"))
            .label("Cancel")
            .style(ButtonStyle::Danger)
            .emoji('❌');

        let embed = CreateEmbed::new()
            .title(action.title())
            .description(format!(
                "This will {} <@&{role_id}> {} **{count}** members, are you sure you want to do \
                 this?\n\nThis will take roughly `{count}` seconds to complete.\n\n**You will not \
                 be able to cancel this action once it starts.**",
                action.verb(),
                action.prep(),
            ))
            .color(colors::CYAN)
            .timestamp(Timestamp::now());

        let builder = CreateMessage::new()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(vec![confirm, cancel])]);

        let sent = match msg.channel_id.send_message(&ctx.http, builder).await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = ?e, "failed to send roleall prompt");
                return;
            }
        };

        // Tidy up the transient "Fetching members..." status message.
        if let Some(s) = status {
            let _ = s.delete(&ctx.http).await;
        }

        self.pending.insert(
            sent.id.get(),
            PendingRoleAll {
                guild_id: guild_id.get(),
                role_id: role_id.get(),
                action,
                author_id,
                member_ids,
            },
        );
    }
}

/// Page through every guild member (the gateway returns at most 1000 per call).
async fn fetch_all_members(http: &Http, guild_id: GuildId) -> Result<Vec<Member>, serenity::Error> {
    let mut all: Vec<Member> = Vec::new();
    let mut after: Option<UserId> = None;
    loop {
        let batch = guild_id.members(http, Some(1000), after).await?;
        let n = batch.len();
        if let Some(last) = batch.last() {
            after = Some(last.user.id);
        }
        all.extend(batch);
        if n < 1000 {
            break;
        }
    }
    Ok(all)
}

// ---- bulk apply (runs off the gateway via tokio::spawn) -------------------

fn in_progress_embed(action: BulkAction, role_id: u64) -> CreateEmbed {
    CreateEmbed::new()
        .title(format!("{} - In Progress", action.title()))
        .description(format!(
            "Starting to {} <@&{role_id}> {} everyone...\nThis message will edit itself as it \
             progresses.",
            action.verb(),
            action.prep(),
        ))
        .color(colors::YELLOW)
        .timestamp(Timestamp::now())
}

fn progress_embed(
    action: BulkAction,
    role_id: u64,
    processed: usize,
    total: usize,
    success: usize,
    fail: usize,
) -> CreateEmbed {
    let bar = loading_bar(processed as u64, total as u64, 20);
    CreateEmbed::new()
        .title(format!("{} - In Progress", action.title()))
        .description(format!(
            "{} <@&{role_id}> {} members...\n\n`{bar}`\n{processed}/{total} processed \u{2022} \
             {success} ok \u{2022} {fail} failed",
            action.past(),
            action.prep(),
        ))
        .color(colors::YELLOW)
        .timestamp(Timestamp::now())
}

fn finished_embed(
    action: BulkAction,
    role_id: u64,
    total: usize,
    success: usize,
    fail: usize,
) -> CreateEmbed {
    CreateEmbed::new()
        .title(format!("{} - Finished", action.title()))
        .description(format!(
            "{} <@&{role_id}> {} **{success}** members.\n{fail} failures out of {total} processed.",
            action.past(),
            action.prep(),
        ))
        .color(colors::GREEN)
        .timestamp(Timestamp::now())
}

/// Apply a confirmed bulk op: ~0.5s between calls, periodic progress edits, and
/// a final summary edit. Targets are pre-filtered, so each call is a real change.
async fn run_bulk(
    http: Arc<Http>,
    channel_id: serenity::all::ChannelId,
    message_id: u64,
    op: PendingRoleAll,
) {
    let guild_id = GuildId::new(op.guild_id);
    let role_id = RoleId::new(op.role_id);
    let total = op.member_ids.len();
    let mut success = 0usize;
    let mut fail = 0usize;
    let msg_id = serenity::all::MessageId::new(message_id);

    for (i, uid) in op.member_ids.iter().enumerate() {
        let user_id = UserId::new(*uid);
        let result = match op.action {
            BulkAction::Add => http.add_member_role(guild_id, user_id, role_id, None).await,
            BulkAction::Remove => {
                http.remove_member_role(guild_id, user_id, role_id, None)
                    .await
            }
        };
        match result {
            Ok(_) => success += 1,
            Err(_) => fail += 1,
        }

        sleep(Duration::from_millis(BULK_DELAY_MS)).await;

        let processed = i + 1;
        if processed % PROGRESS_EVERY == 0 && processed != total {
            let _ = channel_id
                .edit_message(
                    &http,
                    msg_id,
                    EditMessage::new().embed(progress_embed(
                        op.action, op.role_id, processed, total, success, fail,
                    )),
                )
                .await;
        }
    }

    let _ = channel_id
        .edit_message(
            &http,
            msg_id,
            EditMessage::new().embed(finished_embed(op.action, op.role_id, total, success, fail)),
        )
        .await;
}
