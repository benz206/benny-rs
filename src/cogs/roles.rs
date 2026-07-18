use super::Cog;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::colors;
use crate::utils::format::loading_bar;
use crate::utils::parse::parse_role_id;
use crate::utils::roles::{role_rank, top_role};
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, ComponentInteraction, CreateActionRow, CreateButton, CreateEmbed,
    CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage, EditMessage,
    GuildId, Http, Member, Role, RoleId, Timestamp, UserId,
};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::time::{Duration, sleep};

/// Component custom_id namespace for this cog. `on_component` early-returns on
/// anything that does not begin with this.
const CID_PREFIX: &str = "role:";
/// Delay between bulk role API calls (0.5 s).
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

/// A bulk role operation awaiting Start/Cancel, keyed in `PENDING` by the
/// confirmation message id.
struct PendingRoleAll {
    guild_id: u64,
    role_id: u64,
    action: BulkAction,
    author_id: u64,
    member_ids: Vec<u64>,
}

static PENDING: LazyLock<dashmap::DashMap<u64, PendingRoleAll>> =
    LazyLock::new(dashmap::DashMap::new);

pub struct RolesCog {
    #[allow(dead_code)]
    state: Arc<AppState>,
}

impl RolesCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for RolesCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
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
                PENDING.remove(&message_id);
                let _ = interaction
                    .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
                    .await;
                let _ = interaction.message.delete(&ctx.http).await;
            }
            "start" => {
                let Some((_, op)) = PENDING.remove(&message_id) else {
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

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![role(), roleall()]
}

// ---- role subcommand group ------------------------------------------------

/// Manage roles for server members.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Roles",
    subcommand_required,
    subcommands("role_add", "role_remove", "role_custom", "role_all")
)]
async fn role(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Add a role to a member.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Roles",
    required_permissions = "MANAGE_ROLES",
    rename = "add"
)]
async fn role_add(
    ctx: Context<'_>,
    #[description = "Member"] member: serenity::all::Member,
    #[description = "Role"] role: serenity::all::Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let roles = match guild_id.roles(&sctx.http).await {
        Ok(r) => r,
        Err(e) => return send_error(ctx, &format!("Failed to fetch roles: {e}")).await,
    };
    let author_member = match guild_id.member(&sctx.http, ctx.author().id).await {
        Ok(m) => m,
        Err(e) => return send_error(ctx, &format!("Failed to fetch your member info: {e}")).await,
    };

    let role_id = role.id;
    let role_rank_v = role_rank(&role);
    let role_colour = role.colour;
    let everyone = RoleId::new(guild_id.get());
    let member_id = member.user.id;

    let Some(author_top) = top_role(&author_member.roles, &roles, everyone) else {
        return send_error(ctx, "Could not determine your top role.").await;
    };
    let Some(member_top) = top_role(&member.roles, &roles, everyone) else {
        return send_error(ctx, "Could not determine the member's top role.").await;
    };

    if role_rank(author_top) < role_rank(member_top) {
        return send_error(
            ctx,
            &format!(
                "You cannot add <@&{role_id}> to <@{member_id}> as their highest role is \
                 higher than your highest role (<@&{}>).",
                member_top.id
            ),
        )
        .await;
    }
    if role_rank_v > role_rank(member_top) {
        return send_error(
            ctx,
            &format!(
                "You cannot add <@&{role_id}> to <@{member_id}> as it's higher than their \
                 top role (<@&{}>).",
                member_top.id
            ),
        )
        .await;
    }

    let bot_top = bot_top_rank(sctx, guild_id, &roles).await;
    if role_rank_v >= bot_top {
        return send_error(
            ctx,
            &format!(
                "I cannot assign <@&{role_id}> as it is not below my highest role in the \
                 hierarchy."
            ),
        )
        .await;
    }

    if let Err(e) = sctx
        .http
        .add_member_role(guild_id, member_id, role_id, None)
        .await
    {
        return send_error(ctx, &format!("Failed to add role: {e}")).await;
    }

    let embed = CreateEmbed::new()
        .title("Role Added")
        .description(format!("Added <@&{role_id}> to <@{member_id}>"))
        .color(role_colour)
        .footer(CreateEmbedFooter::new(format!("Role ID: {role_id}")))
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Remove a role from a member.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Roles",
    required_permissions = "MANAGE_ROLES",
    rename = "remove"
)]
async fn role_remove(
    ctx: Context<'_>,
    #[description = "Member"] member: serenity::all::Member,
    #[description = "Role"] role: serenity::all::Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let roles = match guild_id.roles(&sctx.http).await {
        Ok(r) => r,
        Err(e) => return send_error(ctx, &format!("Failed to fetch roles: {e}")).await,
    };
    let author_member = match guild_id.member(&sctx.http, ctx.author().id).await {
        Ok(m) => m,
        Err(e) => return send_error(ctx, &format!("Failed to fetch your member info: {e}")).await,
    };

    let role_id = role.id;
    let role_colour = role.colour;
    let everyone = RoleId::new(guild_id.get());
    let member_id = member.user.id;

    let Some(author_top) = top_role(&author_member.roles, &roles, everyone) else {
        return send_error(ctx, "Could not determine your top role.").await;
    };
    let Some(member_top) = top_role(&member.roles, &roles, everyone) else {
        return send_error(ctx, "Could not determine the member's top role.").await;
    };

    if role_rank(author_top) < role_rank(member_top) {
        return send_error(
            ctx,
            &format!(
                "You cannot remove <@&{role_id}> from <@{member_id}> as their highest role \
                 (<@&{}>) is higher than your highest role (<@&{}>).",
                member_top.id, author_top.id
            ),
        )
        .await;
    }
    if role_id == member_top.id {
        return send_error(
            ctx,
            &format!(
                "You cannot remove <@&{role_id}> from <@{member_id}> as it's their highest \
                 role (<@&{}>).",
                member_top.id
            ),
        )
        .await;
    }

    if let Err(e) = sctx
        .http
        .remove_member_role(guild_id, member_id, role_id, None)
        .await
    {
        return send_error(ctx, &format!("Failed to remove role: {e}")).await;
    }

    let embed = CreateEmbed::new()
        .title("Role Removed")
        .description(format!("Removed <@&{role_id}> from <@{member_id}>"))
        .color(role_colour)
        .footer(CreateEmbedFooter::new(format!("Role ID: {role_id}")))
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Apply custom role changes to a member (+add, -remove, !toggle).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Roles",
    required_permissions = "MANAGE_ROLES",
    rename = "custom",
    aliases("c")
)]
async fn role_custom(
    ctx: Context<'_>,
    #[description = "Member"] member: serenity::all::Member,
    #[description = "Role changes (+role/-role/!role ...)"]
    #[rest]
    roles: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let guild_roles = match guild_id.roles(&sctx.http).await {
        Ok(r) => r,
        Err(e) => return send_error(ctx, &format!("Failed to fetch roles: {e}")).await,
    };
    let author_member = match guild_id.member(&sctx.http, ctx.author().id).await {
        Ok(m) => m,
        Err(e) => return send_error(ctx, &format!("Failed to fetch your member info: {e}")).await,
    };

    let everyone = RoleId::new(guild_id.get());
    let member_id = member.user.id;

    // Partition tokens into add/remove sets (+ add, - remove, ! toggle).
    let mut add: Vec<RoleId> = Vec::new();
    let mut remove: Vec<RoleId> = Vec::new();
    for token in roles.split_whitespace() {
        let sigil = token.chars().next();
        // Tokens without a +/-/! sigil are ignored.
        if !matches!(sigil, Some('+') | Some('-') | Some('!')) {
            continue;
        }
        let name = &token[1..]; // safe: matched sigils are 1-byte ASCII
        let Some(role) = resolve_role(&guild_roles, name) else {
            return send_error(ctx, &format!("Could not find role `{name}`.")).await;
        };
        match sigil {
            Some('+') => add.push(role.id),
            Some('-') => remove.push(role.id),
            Some('!') => {
                if member.roles.contains(&role.id) {
                    remove.push(role.id);
                } else {
                    add.push(role.id);
                }
            }
            _ => {}
        }
    }

    let Some(author_top) = top_role(&author_member.roles, &guild_roles, everyone) else {
        return send_error(ctx, "Could not determine your top role.").await;
    };
    let Some(member_top) = top_role(&member.roles, &guild_roles, everyone) else {
        return send_error(ctx, "Could not determine the member's top role.").await;
    };

    if role_rank(author_top) < role_rank(member_top) {
        return send_error(
            ctx,
            &format!(
                "You cannot manage these roles from <@{member_id}> as their highest role \
                 (<@&{}>) is higher than your highest role (<@&{}>).",
                member_top.id, author_top.id
            ),
        )
        .await;
    }
    for rid in &add {
        if let Some(r) = guild_roles.get(rid)
            && role_rank(r) > role_rank(member_top) {
                return send_error(
                    ctx,
                    &format!(
                        "You cannot add <@&{rid}> to <@{member_id}> as it's higher than their \
                         top role (<@&{}>).",
                        member_top.id
                    ),
                )
                .await;
            }
    }
    for rid in &remove {
        if *rid == member_top.id {
            return send_error(
                ctx,
                &format!(
                    "You cannot remove <@&{rid}> from <@{member_id}> as it's their highest \
                     role (<@&{}>).",
                    member_top.id
                ),
            )
            .await;
        }
    }

    // Bot hierarchy guard on every role we will add.
    let bot_top = bot_top_rank(sctx, guild_id, &guild_roles).await;
    for rid in &add {
        if let Some(r) = guild_roles.get(rid)
            && role_rank(r) >= bot_top {
                return send_error(
                    ctx,
                    &format!(
                        "I cannot assign <@&{rid}> as it is not below my highest role in the \
                         hierarchy."
                    ),
                )
                .await;
            }
    }

    for rid in &add {
        let _ = sctx
            .http
            .add_member_role(guild_id, member_id, *rid, None)
            .await;
    }
    for rid in &remove {
        let _ = sctx
            .http
            .remove_member_role(guild_id, member_id, *rid, None)
            .await;
    }

    // Colour/footer come from the last touched role (remove wins over add).
    let last = remove.last().or_else(|| add.last()).copied();
    let colour = last
        .and_then(|rid| guild_roles.get(&rid))
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
    send_embed(ctx, embed).await
}

/// Bulk-add (or remove) a role from every member in the server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Roles",
    required_permissions = "MANAGE_ROLES",
    rename = "all",
    aliases("bulk")
)]
async fn role_all(
    ctx: Context<'_>,
    #[description = "Role"] role: serenity::all::Role,
    #[description = "Remove instead of add"] remove: Option<bool>,
) -> Result<(), Error> {
    do_roleall(ctx, role, remove).await
}

// ---- top-level roleall command --------------------------------------------

/// Bulk-add (or remove) a role from every member in the server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Roles",
    required_permissions = "MANAGE_ROLES",
    aliases("removeall", "rall")
)]
async fn roleall(
    ctx: Context<'_>,
    #[description = "Role to apply to all members"] role: serenity::all::Role,
    #[description = "Remove instead of add"] remove: Option<bool>,
) -> Result<(), Error> {
    do_roleall(ctx, role, remove).await
}

// ---- shared bulk logic ----------------------------------------------------

async fn do_roleall(
    ctx: Context<'_>,
    role: serenity::all::Role,
    remove: Option<bool>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let action = if remove.unwrap_or(false) {
        BulkAction::Remove
    } else {
        BulkAction::Add
    };

    let roles = match guild_id.roles(&sctx.http).await {
        Ok(r) => r,
        Err(e) => return send_error(ctx, &format!("Failed to fetch roles: {e}")).await,
    };

    let role_id = role.id;
    let role_rank_v = role_rank(&role);

    // The bot must sit above the role to manage it for anyone.
    let bot_top = bot_top_rank(sctx, guild_id, &roles).await;
    if role_rank_v >= bot_top {
        return send_error(
            ctx,
            &format!(
                "I cannot manage <@&{role_id}> as it is not below my highest role in the \
                 hierarchy."
            ),
        )
        .await;
    }

    let status = ctx
        .channel_id()
        .say(sctx, "Fetching members...")
        .await
        .ok();

    let members = match fetch_all_members(&sctx.http, guild_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = ?e, "failed to fetch members for roleall");
            return send_error(ctx, "Failed to fetch members.").await;
        }
    };

    // Affected = non-bot members who would actually change.
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

    let author_id = ctx.author().id.get();
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

    let handle = ctx
        .send(
            poise::CreateReply::default()
                .embed(embed)
                .components(vec![CreateActionRow::Buttons(vec![confirm, cancel])]),
        )
        .await?;
    let sent = handle.message().await?;

    // Tidy up the transient "Fetching members..." status message.
    if let Some(s) = status {
        let _ = s.delete(sctx).await;
    }

    crate::utils::cache::bounded_insert(
        &PENDING,
        sent.id.get(),
        PendingRoleAll {
            guild_id: guild_id.get(),
            role_id: role_id.get(),
            action,
            author_id,
            member_ids,
        },
        500,
    );

    Ok(())
}

// ---- role hierarchy helpers -----------------------------------------------

/// Resolve a role token (`<@&id>`, bare id, or case-insensitive name) to a guild
/// role. Returns the live `Role` so callers can read its position and colour.
fn resolve_role<'a>(roles: &'a HashMap<RoleId, Role>, token: &str) -> Option<&'a Role> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if let Some(id) = parse_role_id(token)
        && let Some(r) = roles.get(&RoleId::new(id)) {
            return Some(r);
        }
    let lower = token.to_lowercase();
    roles.values().find(|r| r.name.to_lowercase() == lower)
}

/// The bot's own top-role rank in this guild, used to refuse assigning roles
/// at or above the bot's hierarchy position.
async fn bot_top_rank(
    ctx: &serenity::all::Context,
    guild_id: GuildId,
    roles: &HashMap<RoleId, Role>,
) -> (i64, Reverse<u64>) {
    let everyone = RoleId::new(guild_id.get());
    let bot_id = ctx.cache.current_user().id;
    // @everyone (id == guild id) is normally always present; if a cold-cache
    // fetch ever lacks it, fall back to its canonical rank instead of panicking.
    let default = roles
        .get(&everyone)
        .map(role_rank)
        .unwrap_or((0, Reverse(everyone.get())));
    match guild_id.member(&ctx.http, bot_id).await {
        Ok(bot) => top_role(&bot.roles, roles, everyone)
            .map(role_rank)
            .unwrap_or(default),
        Err(_) => default,
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
