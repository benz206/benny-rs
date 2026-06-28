use super::Cog;
use crate::entities::{mod_cases, mod_config, mod_timed};
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::roles::{role_rank, top_role};
use crate::utils::time::parse_when;
use crate::utils::{colors, format};
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use serenity::all::{
    Colour, CreateEmbed, CreateEmbedFooter, EditRole, GuildId, Http, PermissionOverwrite,
    PermissionOverwriteType, Permissions, RoleId, Timestamp, User, UserId,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, sleep};

/// How often the background task scans for expired timed infractions.
const EXPIRY_INTERVAL_SECS: u64 = 30;

pub struct ModerationCog {
    state: Arc<AppState>,
    /// Guards the self-spawned expiry task so gateway reconnects (which re-fire
    /// `on_ready`) do not stack duplicate loops.
    expiry_spawned: AtomicBool,
}

impl ModerationCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            expiry_spawned: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl Cog for ModerationCog {
    async fn on_ready(&self, ctx: &serenity::all::Context) {
        // Spawn the expiry sweeper exactly once for the process lifetime.
        if self.expiry_spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        spawn_expiry_task(self.state.clone(), ctx.http.clone());
        tracing::info!("Moderation expiry task started");
    }
}

/// The moderation command surface (prefix + slash).
pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        warn(),
        kick(),
        ban(),
        unban(),
        mute(),
        unmute(),
        case(),
        cases(),
        modlog(),
    ]
}

// ---- commands --------------------------------------------------------------

/// Warn a member and record a moderation case.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MODERATE_MEMBERS"
)]
async fn warn(
    ctx: Context<'_>,
    #[description = "Member to warn"] member: User,
    #[description = "Reason"]
    #[rest]
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let author = ctx.author();
    let target_id = member.id.get();
    let reason = default_reason(reason.unwrap_or_default());

    if let Some(err) = self_guard(sctx, author.id.get(), target_id, "warn") {
        return send_error(ctx, &err).await;
    }
    if let Some(err) = can_act_on(sctx, guild_id, author.id.get(), target_id, "warn").await {
        return send_error(ctx, &err).await;
    }

    let case = create_case(state, guild_id, "warn", target_id, author.id.get(), &reason, None).await;
    let name = fetch_name(sctx, target_id).await;
    let embed = action_embed("Warned", &name, &reason, colors::YELLOW, author, case);
    send_embed(ctx, embed).await
}

/// Kick a member from the server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "KICK_MEMBERS"
)]
async fn kick(
    ctx: Context<'_>,
    #[description = "Member to kick"] member: User,
    #[description = "Reason"]
    #[rest]
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let author = ctx.author();
    let target_id = member.id.get();
    let reason = default_reason(reason.unwrap_or_default());

    if let Some(err) = self_guard(sctx, author.id.get(), target_id, "kick") {
        return send_error(ctx, &err).await;
    }
    if let Some(err) = can_act_on(sctx, guild_id, author.id.get(), target_id, "kick").await {
        return send_error(ctx, &err).await;
    }

    if let Err(e) = guild_id
        .kick_with_reason(&sctx.http, UserId::new(target_id), &reason)
        .await
    {
        return send_error(ctx, &format!("Failed to kick: {e}")).await;
    }

    let case = create_case(state, guild_id, "kick", target_id, author.id.get(), &reason, None).await;
    let name = fetch_name(sctx, target_id).await;
    let embed = action_embed("Kicked", &name, &reason, colors::RED, author, case);
    send_embed(ctx, embed).await
}

/// Ban a user, optionally purging recent messages.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "BAN_MEMBERS"
)]
async fn ban(
    ctx: Context<'_>,
    #[description = "User to ban (mention or id)"] user: User,
    #[description = "Days of messages to delete (0-7)"]
    #[min = 0]
    #[max = 7]
    delete_days: Option<u8>,
    #[description = "Reason"]
    #[rest]
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let author = ctx.author();
    let target_id = user.id.get();
    let reason = default_reason(reason.unwrap_or_default());
    let delete_days = delete_days.unwrap_or(0).min(7);

    if let Some(err) = self_guard(sctx, author.id.get(), target_id, "ban") {
        return send_error(ctx, &err).await;
    }
    if let Some(err) = can_act_on(sctx, guild_id, author.id.get(), target_id, "ban").await {
        return send_error(ctx, &err).await;
    }

    if let Err(e) = guild_id
        .ban_with_reason(&sctx.http, UserId::new(target_id), delete_days, &reason)
        .await
    {
        return send_error(ctx, &format!("Failed to ban: {e}")).await;
    }

    let case = create_case(state, guild_id, "ban", target_id, author.id.get(), &reason, None).await;
    let name = fetch_name(sctx, target_id).await;
    let mut description = reason.clone();
    if delete_days > 0 {
        description =
            format!("{description}\n\nDeleted the last **{delete_days}** day(s) of messages.");
    }
    let embed = action_embed("Banned", &name, &description, colors::RED, author, case);
    send_embed(ctx, embed).await
}

/// Lift a ban for the given user.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "BAN_MEMBERS"
)]
async fn unban(
    ctx: Context<'_>,
    #[description = "User to unban (mention or id)"] user: User,
    #[description = "Reason"]
    #[rest]
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let author = ctx.author();
    let target_id = user.id.get();
    let reason = default_reason(reason.unwrap_or_default());

    if let Err(e) = guild_id.unban(&sctx.http, UserId::new(target_id)).await {
        return send_error(ctx, &format!("Failed to unban: {e}")).await;
    }

    // Drop any scheduled temp-ban expiry for this user.
    let _ = mod_timed::Entity::delete_many()
        .filter(mod_timed::Column::GuildId.eq(guild_id.get() as i64))
        .filter(mod_timed::Column::UserId.eq(target_id as i64))
        .filter(mod_timed::Column::Action.eq("ban"))
        .exec(state.servers_orm())
        .await;

    let case = create_case(state, guild_id, "unban", target_id, author.id.get(), &reason, None).await;
    let name = fetch_name(sctx, target_id).await;
    let embed = action_embed("Unbanned", &name, &reason, colors::GREEN, author, case);
    send_embed(ctx, embed).await
}

/// Temporarily mute a member for the given duration (e.g. `1h`, `30m`, `2d`).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_ROLES"
)]
async fn mute(
    ctx: Context<'_>,
    #[description = "Member to mute"] member: User,
    #[description = "Duration, e.g. 1h, 30m, 2d"] duration: String,
    #[description = "Reason"]
    #[rest]
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let author = ctx.author();
    let target_id = member.id.get();

    if let Some(err) = self_guard(sctx, author.id.get(), target_id, "mute") {
        return send_error(ctx, &err).await;
    }
    if let Some(err) = can_act_on(sctx, guild_id, author.id.get(), target_id, "mute").await {
        return send_error(ctx, &err).await;
    }

    let now = Utc::now();
    let Some(expiry) = parse_when(&duration, now).filter(|dt| *dt > now) else {
        return send_error(
            ctx,
            "Could not parse a duration. Example: `mute @user 1h spamming`",
        )
        .await;
    };
    let reason = default_reason(reason.unwrap_or_default());
    let expires_ts = expiry.timestamp();

    let Some(role_id) = ensure_mute_role(sctx, state, guild_id).await else {
        return send_error(ctx, "Failed to resolve or create the **Muted** role.").await;
    };

    if let Err(e) = sctx
        .http
        .add_member_role(guild_id, UserId::new(target_id), role_id, Some(&reason))
        .await
    {
        return send_error(ctx, &format!("Failed to apply the Muted role: {e}")).await;
    }

    let case = create_case(
        state,
        guild_id,
        "mute",
        target_id,
        author.id.get(),
        &reason,
        Some(expires_ts),
    )
    .await;

    // Record the active timed infraction for the expiry sweeper. If the case
    // insert failed we still need a primary key, so fall back to a local
    // monotonic counter scoped to mod_timed.
    let case_number = match case {
        Some(c) => c,
        None => fallback_case_number(state, guild_id).await,
    };
    let _ = mod_timed::Entity::insert(mod_timed::ActiveModel {
        guild_id: Set(guild_id.get() as i64),
        case_number: Set(case_number),
        user_id: Set(target_id as i64),
        action: Set("mute".to_string()),
        expires_at: Set(expires_ts),
    })
    .on_conflict(
        OnConflict::columns([mod_timed::Column::GuildId, mod_timed::Column::CaseNumber])
            .update_columns([
                mod_timed::Column::UserId,
                mod_timed::Column::Action,
                mod_timed::Column::ExpiresAt,
            ])
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;

    let name = fetch_name(sctx, target_id).await;
    let description = format!("{reason}\n\nExpires <t:{expires_ts}:R>");
    let embed = action_embed("Muted", &name, &description, colors::RED, author, case);
    send_embed(ctx, embed).await
}

/// Remove the Muted role from a member.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    required_permissions = "MANAGE_ROLES"
)]
async fn unmute(
    ctx: Context<'_>,
    #[description = "Member to unmute"] member: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let author = ctx.author();
    let target_id = member.id.get();

    let stored: Option<i64> = mod_config::Entity::find_by_id(guild_id.get() as i64)
        .one(state.servers_orm())
        .await
        .ok()
        .flatten()
        .and_then(|m| m.mute_role_id);
    let Some(role_id) = stored.map(|r| RoleId::new(r as u64)) else {
        return send_error(ctx, "No **Muted** role is configured for this server.").await;
    };

    if let Err(e) = sctx
        .http
        .remove_member_role(guild_id, UserId::new(target_id), role_id, Some("Unmuted"))
        .await
    {
        return send_error(ctx, &format!("Failed to remove the Muted role: {e}")).await;
    }

    // Clear scheduled expiry rows for this user's mute.
    let _ = mod_timed::Entity::delete_many()
        .filter(mod_timed::Column::GuildId.eq(guild_id.get() as i64))
        .filter(mod_timed::Column::UserId.eq(target_id as i64))
        .filter(mod_timed::Column::Action.eq("mute"))
        .exec(state.servers_orm())
        .await;

    let case = create_case(
        state,
        guild_id,
        "unmute",
        target_id,
        author.id.get(),
        "Unmuted",
        None,
    )
    .await;
    let name = fetch_name(sctx, target_id).await;
    let embed = action_embed("Unmuted", &name, "Unmuted", colors::GREEN, author, case);
    send_embed(ctx, embed).await
}

/// View a single moderation case by number.
#[poise::command(slash_command, prefix_command, guild_only, category = "Moderation")]
async fn case(
    ctx: Context<'_>,
    #[description = "Case number"] number: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;

    match mod_cases::Entity::find_by_id((guild_id.get() as i64, number))
        .one(state.servers_orm())
        .await
    {
        Ok(Some(case)) => send_embed(ctx, case_embed(&case)).await,
        Ok(None) => send_error(ctx, &format!("Case #{number} not found.")).await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to get case");
            send_error(ctx, "Failed to retrieve case.").await
        }
    }
}

/// List all moderation cases recorded for a member.
#[poise::command(slash_command, prefix_command, guild_only, category = "Moderation")]
async fn cases(
    ctx: Context<'_>,
    #[description = "Member"] member: User,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let target_id = member.id.get();

    let cases = match mod_cases::Entity::find()
        .filter(mod_cases::Column::GuildId.eq(guild_id.get() as i64))
        .filter(mod_cases::Column::TargetId.eq(target_id as i64))
        .order_by_asc(mod_cases::Column::CaseNumber)
        .all(state.servers_orm())
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "failed to get cases");
            return send_error(ctx, "Failed to retrieve cases.").await;
        }
    };

    if cases.is_empty() {
        return send_error(ctx, &format!("No cases found for <@{target_id}>.")).await;
    }

    let mut embed = CreateEmbed::new()
        .title(format!("Cases for {}", fetch_name(sctx, target_id).await))
        .description(format!("<@{target_id}> has **{}** case(s).", cases.len()))
        .color(colors::BLURPLE)
        .timestamp(Timestamp::now());
    for c in cases.iter().take(15) {
        embed = embed.field(
            format!("#{} \u{2022} {}", c.case_number, c.action_type.to_uppercase()),
            format!(
                "**Reason:** {}\n**Moderator:** <@{}>",
                format::truncate(&c.reason, 200),
                c.moderator_id
            ),
            false,
        );
    }
    if cases.len() > 15 {
        embed = embed.footer(CreateEmbedFooter::new(format!(
            "...and {} more",
            cases.len() - 15
        )));
    }
    send_embed(ctx, embed).await
}

/// Show the most recent moderation actions in this server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Moderation",
    aliases("modlogs")
)]
async fn modlog(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;

    let cases = match mod_cases::Entity::find()
        .filter(mod_cases::Column::GuildId.eq(guild_id.get() as i64))
        .order_by_desc(mod_cases::Column::CaseNumber)
        .limit(15)
        .all(state.servers_orm())
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = ?e, "failed to get modlog");
            return send_error(ctx, "Failed to retrieve the modlog.").await;
        }
    };

    if cases.is_empty() {
        return send_error(ctx, "No moderation actions recorded yet.").await;
    }

    let mut embed = CreateEmbed::new()
        .title("Recent Moderation Actions")
        .color(colors::BLURPLE)
        .timestamp(Timestamp::now());
    for c in &cases {
        embed = embed.field(
            format!("#{} \u{2022} {}", c.case_number, c.action_type.to_uppercase()),
            format!(
                "**Target:** <@{}>\n**Moderator:** <@{}>\n**Reason:** {}",
                c.target_id,
                c.moderator_id,
                format::truncate(&c.reason, 150)
            ),
            false,
        );
    }
    send_embed(ctx, embed).await
}

// ---- shared helpers --------------------------------------------------------

/// Reject self-targeting and bot-targeting. Returns an error string when blocked.
fn self_guard(
    ctx: &serenity::all::Context,
    author_id: u64,
    target_id: u64,
    action: &str,
) -> Option<String> {
    let bot_id = ctx.cache.current_user().id.get();
    if target_id == author_id {
        return Some(if action == "ban" {
            "You can't ban yourself idiot".to_string()
        } else {
            format!("You can't {action} yourself.")
        });
    }
    if target_id == bot_id {
        return Some(if action == "ban" {
            "After all I've done for you, you try to ban me?".to_string()
        } else {
            format!("I'm not going to {action} myself.")
        });
    }
    None
}

/// Enforce role hierarchy on the *invoker*: Discord only stops the bot from
/// acting above its own top role, so without this a junior mod with
/// ban/kick/timeout perms could action senior staff. The server owner may act on
/// anyone; nobody may act on the owner or on a member whose highest role is at or
/// above the invoker's. Returns an error message when blocked. Targets that
/// aren't current members (e.g. ban by id) have nothing to compare and pass.
async fn can_act_on(
    ctx: &serenity::all::Context,
    guild_id: GuildId,
    invoker_id: u64,
    target_id: u64,
    action: &str,
) -> Option<String> {
    let (roles, owner_id) = match ctx
        .cache
        .guild(guild_id)
        .map(|g| (g.roles.clone(), g.owner_id))
    {
        Some(t) => t,
        None => {
            let partial = guild_id.to_partial_guild(&ctx.http).await.ok()?;
            (partial.roles, partial.owner_id)
        }
    };
    let everyone = RoleId::new(guild_id.get());

    if target_id == owner_id.get() {
        return Some(format!("You can't {action} the server owner."));
    }
    if invoker_id == owner_id.get() {
        return None;
    }

    let Ok(target) = guild_id.member(&ctx.http, UserId::new(target_id)).await else {
        return None; // not a member — bot/Discord hierarchy still applies
    };
    let Ok(invoker) = guild_id.member(&ctx.http, UserId::new(invoker_id)).await else {
        return None;
    };
    let invoker_top = top_role(&invoker.roles, &roles, everyone).map(role_rank);
    let target_top = top_role(&target.roles, &roles, everyone).map(role_rank);
    if let (Some(it), Some(tt)) = (invoker_top, target_top)
        && tt >= it
    {
        return Some(format!(
            "You can't {action} someone whose highest role is above or equal to yours."
        ));
    }
    None
}

/// Fetch a display name for an arbitrary user id, falling back to a mention.
async fn fetch_name(ctx: &serenity::all::Context, user_id: u64) -> String {
    match UserId::new(user_id).to_user(&ctx.http).await {
        Ok(u) => u.name,
        Err(_) => format!("<@{user_id}>"),
    }
}

/// Insert a moderation case into SQLite via the ORM, returning the assigned
/// per-guild case number (current highest + 1). The read and the insert run in
/// one transaction; the `(guild_id, case_number)` primary key is the final
/// backstop against a duplicate should two actions ever race. Returns `None`
/// only on a database error.
async fn create_case(
    state: &AppState,
    guild_id: GuildId,
    action_type: &str,
    target_id: u64,
    moderator_id: u64,
    reason: &str,
    expires_at: Option<i64>,
) -> Option<i64> {
    let gid = guild_id.get() as i64;
    let now = Utc::now().timestamp();

    let txn = match state.servers_orm().begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = ?e, "failed to open case transaction");
            return None;
        }
    };

    // Next per-guild case number = current highest + 1, read inside the
    // transaction so it stays consistent with the insert that follows.
    let highest = match mod_cases::Entity::find()
        .filter(mod_cases::Column::GuildId.eq(gid))
        .order_by_desc(mod_cases::Column::CaseNumber)
        .one(&txn)
        .await
    {
        Ok(row) => row.map(|m| m.case_number).unwrap_or(0),
        Err(e) => {
            tracing::error!(error = ?e, "failed to read highest case number");
            return None;
        }
    };
    let case_number = highest + 1;

    if let Err(e) = mod_cases::Entity::insert(mod_cases::ActiveModel {
        guild_id: Set(gid),
        case_number: Set(case_number),
        action_type: Set(action_type.to_owned()),
        target_id: Set(target_id as i64),
        moderator_id: Set(moderator_id as i64),
        reason: Set(reason.to_owned()),
        created_at: Set(now),
        active: Set(true),
        expires_at: Set(expires_at),
    })
    .exec(&txn)
    .await
    {
        tracing::error!(error = ?e, "failed to insert moderation case");
        return None;
    }

    if let Err(e) = txn.commit().await {
        tracing::error!(error = ?e, "failed to commit moderation case");
        return None;
    }
    Some(case_number)
}

/// Build the success embed shared by warn/mute/kick/ban/etc.
fn action_embed(
    verb_past: &str,
    target_name: &str,
    reason: &str,
    color: Colour,
    author: &User,
    case: Option<i64>,
) -> CreateEmbed {
    let mut footer_text = format!("{verb_past} by {}", author.name);
    if let Some(n) = case {
        footer_text = format!("{footer_text} \u{2022} Case #{n}");
    }
    let mut footer = CreateEmbedFooter::new(footer_text);
    if let Some(avatar) = author.avatar_url() {
        footer = footer.icon_url(avatar);
    }
    CreateEmbed::new()
        .title(format!("{verb_past} {target_name}"))
        .description(reason)
        .color(color)
        .footer(footer)
        .timestamp(Timestamp::now())
}

/// Resolve the guild's Muted role, creating it (and denying send/speak
/// permissions across every channel) when it is missing. Stores the id in
/// `mod_config`.
async fn ensure_mute_role(
    ctx: &serenity::all::Context,
    state: &AppState,
    guild_id: GuildId,
) -> Option<RoleId> {
    let gid = guild_id.get() as i64;

    let stored: Option<i64> = mod_config::Entity::find_by_id(gid)
        .one(state.servers_orm())
        .await
        .ok()
        .flatten()
        .and_then(|m| m.mute_role_id);

    // Reuse the stored role only if it still exists in the guild.
    if let Some(rid) = stored
        && let Ok(roles) = guild_id.roles(&ctx.http).await
        && roles.contains_key(&RoleId::new(rid as u64))
    {
        return Some(RoleId::new(rid as u64));
    }

    let role = guild_id
        .create_role(
            ctx,
            EditRole::new()
                .name("Muted")
                .permissions(Permissions::empty())
                .mentionable(false),
        )
        .await
        .ok()?;

    // Deny the role from talking / reacting in every existing channel.
    if let Ok(channels) = guild_id.channels(&ctx.http).await {
        let overwrite = PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::SEND_MESSAGES
                | Permissions::SEND_MESSAGES_IN_THREADS
                | Permissions::CREATE_PUBLIC_THREADS
                | Permissions::CREATE_PRIVATE_THREADS
                | Permissions::ADD_REACTIONS
                | Permissions::SPEAK,
            kind: PermissionOverwriteType::Role(role.id),
        };
        for (_id, channel) in channels {
            let _ = channel
                .create_permission(&ctx.http, overwrite.clone())
                .await;
        }
    }

    let _ = mod_config::Entity::insert(mod_config::ActiveModel {
        guild_id: Set(gid),
        mute_role_id: Set(Some(role.id.get() as i64)),
    })
    .on_conflict(
        OnConflict::column(mod_config::Column::GuildId)
            .update_columns([mod_config::Column::MuteRoleId])
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;

    Some(role.id)
}

/// Next mod_timed primary key when no case number is available.
async fn fallback_case_number(state: &AppState, guild_id: GuildId) -> i64 {
    // Highest existing case number for the guild (NULL/no rows -> 0), +1.
    let max: Option<i64> = mod_timed::Entity::find()
        .filter(mod_timed::Column::GuildId.eq(guild_id.get() as i64))
        .order_by_desc(mod_timed::Column::CaseNumber)
        .one(state.servers_orm())
        .await
        .ok()
        .flatten()
        .map(|m| m.case_number);
    max.unwrap_or(0) + 1
}

// ---- free helpers ----------------------------------------------------------

/// Apply the default reason when none was supplied.
fn default_reason(reason: String) -> String {
    if reason.trim().is_empty() {
        "No reason provided.".to_string()
    } else {
        reason
    }
}

/// Detailed embed for a single case lookup.
fn case_embed(case: &mod_cases::Model) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(format!(
            "Case #{} \u{2022} {}",
            case.case_number,
            case.action_type.to_uppercase()
        ))
        .color(colors::BLURPLE)
        .field("Target", format!("<@{}>", case.target_id), true)
        .field("Moderator", format!("<@{}>", case.moderator_id), true)
        .field("Reason", &case.reason, false)
        .field("When", format!("<t:{}:F>", case.created_at), false)
        .timestamp(Timestamp::now());
    if let Some(exp) = case.expires_at {
        embed = embed.field(
            "Expires",
            format!("<t:{exp}:R>{}", if case.active { "" } else { " (lifted)" }),
            false,
        );
    }
    embed
}

/// Background sweeper: every `EXPIRY_INTERVAL_SECS` it lifts any timed
/// infraction whose `expires_at` has passed (removes the Muted role for mutes,
/// unbans for temp-bans), marks the logged case inactive, and clears the row.
fn spawn_expiry_task(state: Arc<AppState>, http: Arc<Http>) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(EXPIRY_INTERVAL_SECS)).await;
            let now = Utc::now().timestamp();

            let rows = match mod_timed::Entity::find()
                .filter(mod_timed::Column::ExpiresAt.lte(now))
                .all(state.servers_orm())
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = ?e, "mod expiry scan failed");
                    continue;
                }
            };

            for row in rows {
                let mod_timed::Model {
                    guild_id: gid,
                    case_number,
                    user_id: uid,
                    action,
                    ..
                } = row;
                let guild_id = GuildId::new(gid as u64);
                let user_id = UserId::new(uid as u64);

                let lifted = match action.as_str() {
                    "mute" => {
                        let role: Option<i64> = mod_config::Entity::find_by_id(gid)
                            .one(state.servers_orm())
                            .await
                            .ok()
                            .flatten()
                            .and_then(|m| m.mute_role_id);
                        match role {
                            Some(rid) => http
                                .remove_member_role(
                                    guild_id,
                                    user_id,
                                    RoleId::new(rid as u64),
                                    Some("Mute expired"),
                                )
                                .await
                                .is_ok(),
                            // No role on record: nothing to undo.
                            None => true,
                        }
                    }
                    "ban" => http
                        .remove_ban(guild_id, user_id, Some("Temp-ban expired"))
                        .await
                        .is_ok(),
                    _ => true,
                };

                // Always clear the row (a permanently failing target — e.g. a
                // user who already left — must not wedge the sweeper).
                let _ = mod_timed::Entity::delete_many()
                    .filter(mod_timed::Column::GuildId.eq(gid))
                    .filter(mod_timed::Column::CaseNumber.eq(case_number))
                    .exec(state.servers_orm())
                    .await;

                // Mark the logged case inactive (best-effort).
                let _ = mod_cases::Entity::update_many()
                    .col_expr(mod_cases::Column::Active, Expr::value(false))
                    .filter(mod_cases::Column::GuildId.eq(gid))
                    .filter(mod_cases::Column::CaseNumber.eq(case_number))
                    .exec(state.servers_orm())
                    .await;

                if !lifted {
                    tracing::warn!(guild = gid, case = case_number, action = %action, "failed to lift infraction");
                }
            }
        }
    });
}
