use super::Cog;
use crate::db_mongo::{self, ModCase};
use crate::entities::{mod_config, mod_timed};
use crate::state::AppState;
use crate::utils::embeds::error_embed;
use crate::utils::parse::parse_user_id;
use crate::utils::roles::{role_rank, top_role};
use crate::utils::time::parse_when;
use crate::utils::{colors, format};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serenity::all::{
    Context, CreateEmbed, CreateEmbedFooter, CreateMessage, EditRole, GuildId, Http, Message,
    PermissionOverwrite, PermissionOverwriteType, Permissions, RoleId, Timestamp, UserId,
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
    async fn on_ready(&self, ctx: &Context) {
        // Spawn the expiry sweeper exactly once for the process lifetime.
        if self.expiry_spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        spawn_expiry_task(self.state.clone(), ctx.http.clone());
        tracing::info!("Moderation expiry task started");
    }

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
            "warn" => self.cmd_warn(ctx, msg, guild_id, rest).await,
            "kick" => self.cmd_kick(ctx, msg, guild_id, rest).await,
            "ban" => self.cmd_ban(ctx, msg, guild_id, rest).await,
            "unban" => self.cmd_unban(ctx, msg, guild_id, rest).await,
            "mute" => self.cmd_mute(ctx, msg, guild_id, rest).await,
            "unmute" => self.cmd_unmute(ctx, msg, guild_id, rest).await,
            "case" => self.cmd_case(ctx, msg, guild_id, rest).await,
            "cases" => self.cmd_cases(ctx, msg, guild_id, rest).await,
            "modlog" | "modlogs" => self.cmd_modlog(ctx, msg, guild_id).await,
            _ => {}
        }
    }
}

impl ModerationCog {
    // ---- shared helpers ---------------------------------------------------

    /// Send a plain error embed to the command channel.
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

    /// Compute the invoking member's effective guild permissions. Prefers the
    /// cache (populated by the GUILDS intent), falling back to a fresh HTTP
    /// fetch of the partial guild so the check still works on a cold cache.
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

    /// Enforce that the invoker holds `perm` (administrator / owner always
    /// pass). Sends an error embed and returns false when the check fails.
    async fn require_perm(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        perm: Permissions,
        label: &str,
    ) -> bool {
        let allowed = self
            .invoker_perms(ctx, guild_id, msg.author.id.get())
            .await
            .map(|p| p.contains(Permissions::ADMINISTRATOR) || p.contains(perm))
            .unwrap_or(false);
        if !allowed {
            self.reply_error(
                ctx,
                msg,
                &format!("You need the **{label}** permission to use this command."),
            )
            .await;
        }
        allowed
    }

    /// Reject self-targeting and bot-targeting. Reserves mod.py's exact ban
    /// quips for the ban action. Returns an error string when blocked.
    fn self_guard(
        &self,
        ctx: &Context,
        msg: &Message,
        target_id: u64,
        action: &str,
    ) -> Option<String> {
        let bot_id = ctx.cache.current_user().id.get();
        if target_id == msg.author.id.get() {
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
    /// ban/kick/timeout perms could action senior staff. The server owner may
    /// act on anyone; nobody may act on the owner or on a member whose highest
    /// role is at or above the invoker's. Returns an error message when blocked.
    /// Targets that aren't current members (e.g. ban by id) have nothing to
    /// compare and pass.
    async fn can_act_on(
        &self,
        ctx: &Context,
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
    async fn fetch_name(&self, ctx: &Context, user_id: u64) -> String {
        match UserId::new(user_id).to_user(&ctx.http).await {
            Ok(u) => u.name,
            Err(_) => format!("<@{user_id}>"),
        }
    }

    /// Insert a moderation case into MongoDB, returning the assigned case
    /// number. Degrades to `None` (no case number) when Mongo is unavailable.
    async fn create_case(
        &self,
        guild_id: GuildId,
        action_type: &str,
        target_id: u64,
        moderator_id: u64,
        reason: &str,
        expires_at: Option<i64>,
    ) -> Option<i64> {
        let mongo = match &self.state.mongo {
            Some(m) => m,
            None => {
                tracing::warn!("MongoDB not available, cannot create moderation case");
                return None;
            }
        };

        let guild_id_i64 = guild_id.get() as i64;
        let case_number = match db_mongo::next_case_number(mongo, guild_id_i64).await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(error = ?e, "failed to get next case number");
                return None;
            }
        };

        let case = ModCase {
            guild_id: guild_id_i64,
            case_number,
            action_type: action_type.to_string(),
            target_id: target_id as i64,
            moderator_id: moderator_id as i64,
            reason: reason.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            active: true,
            expires_at,
        };

        if let Err(e) = db_mongo::insert_case(mongo, &case).await {
            tracing::error!(error = ?e, "failed to insert case");
            return None;
        }

        Some(case_number)
    }

    /// Build the success embed shared by warn/mute/kick/ban/etc.
    fn action_embed(
        verb_past: &str,
        target_name: &str,
        reason: &str,
        color: serenity::all::Colour,
        msg: &Message,
        case: Option<i64>,
    ) -> CreateEmbed {
        let mut footer_text = format!("{verb_past} by {}", msg.author.name);
        if let Some(n) = case {
            footer_text = format!("{footer_text} \u{2022} Case #{n}");
        }
        let mut footer = CreateEmbedFooter::new(footer_text);
        if let Some(avatar) = msg.author.avatar_url() {
            footer = footer.icon_url(avatar);
        }
        CreateEmbed::new()
            .title(format!("{verb_past} {target_name}"))
            .description(reason)
            .color(color)
            .footer(footer)
            .timestamp(Timestamp::now())
    }

    // ---- commands ---------------------------------------------------------

    async fn cmd_warn(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perm(
                ctx,
                msg,
                guild_id,
                Permissions::MODERATE_MEMBERS,
                "Moderate Members",
            )
            .await
        {
            return;
        }
        let (target_str, reason) = split_first(rest);
        let reason = default_reason(reason);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `warn <@member> [reason]`")
                .await;
            return;
        };
        if let Some(err) = self.self_guard(ctx, msg, target_id, "warn") {
            self.reply_error(ctx, msg, &err).await;
            return;
        }
        if let Some(err) = self
            .can_act_on(ctx, guild_id, msg.author.id.get(), target_id, "warn")
            .await
        {
            self.reply_error(ctx, msg, &err).await;
            return;
        }

        let case = self
            .create_case(
                guild_id,
                "warn",
                target_id,
                msg.author.id.get(),
                &reason,
                None,
            )
            .await;
        let name = self.fetch_name(ctx, target_id).await;
        let embed = Self::action_embed("Warned", &name, &reason, colors::YELLOW, msg, case);
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_kick(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perm(
                ctx,
                msg,
                guild_id,
                Permissions::KICK_MEMBERS,
                "Kick Members",
            )
            .await
        {
            return;
        }
        let (target_str, reason) = split_first(rest);
        let reason = default_reason(reason);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `kick <@member> [reason]`")
                .await;
            return;
        };
        if let Some(err) = self.self_guard(ctx, msg, target_id, "kick") {
            self.reply_error(ctx, msg, &err).await;
            return;
        }
        if let Some(err) = self
            .can_act_on(ctx, guild_id, msg.author.id.get(), target_id, "kick")
            .await
        {
            self.reply_error(ctx, msg, &err).await;
            return;
        }

        if let Err(e) = guild_id
            .kick_with_reason(&ctx.http, UserId::new(target_id), &reason)
            .await
        {
            self.reply_error(ctx, msg, &format!("Failed to kick: {e}"))
                .await;
            return;
        }

        let case = self
            .create_case(
                guild_id,
                "kick",
                target_id,
                msg.author.id.get(),
                &reason,
                None,
            )
            .await;
        let name = self.fetch_name(ctx, target_id).await;
        let embed = Self::action_embed("Kicked", &name, &reason, colors::RED, msg, case);
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_ban(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perm(ctx, msg, guild_id, Permissions::BAN_MEMBERS, "Ban Members")
            .await
        {
            return;
        }
        let (target_str, after) = split_first(rest);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `ban <@member> [reason] [delete_days]`")
                .await;
            return;
        };
        if let Some(err) = self.self_guard(ctx, msg, target_id, "ban") {
            self.reply_error(ctx, msg, &err).await;
            return;
        }
        if let Some(err) = self
            .can_act_on(ctx, guild_id, msg.author.id.get(), target_id, "ban")
            .await
        {
            self.reply_error(ctx, msg, &err).await;
            return;
        }

        // Trailing 0-7 integer (if present) is the message-delete-days window.
        let (reason, delete_days) = extract_delete_days(&after);
        let reason = default_reason(reason);

        if let Err(e) = guild_id
            .ban_with_reason(&ctx.http, UserId::new(target_id), delete_days, &reason)
            .await
        {
            self.reply_error(ctx, msg, &format!("Failed to ban: {e}"))
                .await;
            return;
        }

        let case = self
            .create_case(
                guild_id,
                "ban",
                target_id,
                msg.author.id.get(),
                &reason,
                None,
            )
            .await;
        let name = self.fetch_name(ctx, target_id).await;
        let mut description = reason.clone();
        if delete_days > 0 {
            description =
                format!("{description}\n\nDeleted the last **{delete_days}** day(s) of messages.");
        }
        let embed = Self::action_embed("Banned", &name, &description, colors::RED, msg, case);
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_unban(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perm(ctx, msg, guild_id, Permissions::BAN_MEMBERS, "Ban Members")
            .await
        {
            return;
        }
        let (target_str, reason) = split_first(rest);
        let reason = default_reason(reason);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `unban <user_id> [reason]`")
                .await;
            return;
        };

        if let Err(e) = guild_id.unban(&ctx.http, UserId::new(target_id)).await {
            self.reply_error(ctx, msg, &format!("Failed to unban: {e}"))
                .await;
            return;
        }

        // Drop any scheduled temp-ban expiry for this user.
        let _ = mod_timed::Entity::delete_many()
            .filter(mod_timed::Column::GuildId.eq(guild_id.get() as i64))
            .filter(mod_timed::Column::UserId.eq(target_id as i64))
            .filter(mod_timed::Column::Action.eq("ban"))
            .exec(self.state.servers_orm())
            .await;

        let case = self
            .create_case(
                guild_id,
                "unban",
                target_id,
                msg.author.id.get(),
                &reason,
                None,
            )
            .await;
        let name = self.fetch_name(ctx, target_id).await;
        let embed = Self::action_embed("Unbanned", &name, &reason, colors::GREEN, msg, case);
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_mute(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perm(
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
        let (target_str, after) = split_first(rest);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `mute <@member> <duration> [reason]`")
                .await;
            return;
        };
        if let Some(err) = self.self_guard(ctx, msg, target_id, "mute") {
            self.reply_error(ctx, msg, &err).await;
            return;
        }
        if let Some(err) = self
            .can_act_on(ctx, guild_id, msg.author.id.get(), target_id, "mute")
            .await
        {
            self.reply_error(ctx, msg, &err).await;
            return;
        }

        let Some((expiry, reason)) = extract_duration(&after) else {
            self.reply_error(
                ctx,
                msg,
                "Could not parse a duration. Example: `mute @user 1h spamming`",
            )
            .await;
            return;
        };
        let reason = default_reason(reason);
        let expires_ts = expiry.timestamp();

        let Some(role_id) = self.ensure_mute_role(ctx, guild_id).await else {
            self.reply_error(ctx, msg, "Failed to resolve or create the **Muted** role.")
                .await;
            return;
        };

        if let Err(e) = ctx
            .http
            .add_member_role(guild_id, UserId::new(target_id), role_id, Some(&reason))
            .await
        {
            self.reply_error(ctx, msg, &format!("Failed to apply the Muted role: {e}"))
                .await;
            return;
        }

        let case = self
            .create_case(
                guild_id,
                "mute",
                target_id,
                msg.author.id.get(),
                &reason,
                Some(expires_ts),
            )
            .await;

        // Record the active timed infraction for the expiry sweeper. When Mongo
        // is offline we still need a primary key, so fall back to a local
        // monotonic counter scoped to mod_timed.
        let case_number = match case {
            Some(c) => c,
            None => self.fallback_case_number(guild_id).await,
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
        .exec(self.state.servers_orm())
        .await;

        let name = self.fetch_name(ctx, target_id).await;
        let description = format!("{reason}\n\nExpires <t:{expires_ts}:R>");
        let embed = Self::action_embed("Muted", &name, &description, colors::RED, msg, case);
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_unmute(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        if !self
            .require_perm(
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
        let (target_str, _) = split_first(rest);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `unmute <@member>`")
                .await;
            return;
        };

        let stored: Option<i64> = mod_config::Entity::find_by_id(guild_id.get() as i64)
            .one(self.state.servers_orm())
            .await
            .ok()
            .flatten()
            .and_then(|m| m.mute_role_id);
        let Some(role_id) = stored.map(|r| RoleId::new(r as u64)) else {
            self.reply_error(ctx, msg, "No **Muted** role is configured for this server.")
                .await;
            return;
        };

        if let Err(e) = ctx
            .http
            .remove_member_role(guild_id, UserId::new(target_id), role_id, Some("Unmuted"))
            .await
        {
            self.reply_error(ctx, msg, &format!("Failed to remove the Muted role: {e}"))
                .await;
            return;
        }

        // Clear scheduled expiry rows for this user's mute.
        let _ = mod_timed::Entity::delete_many()
            .filter(mod_timed::Column::GuildId.eq(guild_id.get() as i64))
            .filter(mod_timed::Column::UserId.eq(target_id as i64))
            .filter(mod_timed::Column::Action.eq("mute"))
            .exec(self.state.servers_orm())
            .await;

        let case = self
            .create_case(
                guild_id,
                "unmute",
                target_id,
                msg.author.id.get(),
                "Unmuted",
                None,
            )
            .await;
        let name = self.fetch_name(ctx, target_id).await;
        let embed = Self::action_embed("Unmuted", &name, "Unmuted", colors::GREEN, msg, case);
        self.reply_embed(ctx, msg, embed).await;
    }

    async fn cmd_case(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        let case_number: i64 = match rest.trim().parse() {
            Ok(n) => n,
            Err(_) => {
                self.reply_error(ctx, msg, "Usage: `case <number>`").await;
                return;
            }
        };

        let mongo = match &self.state.mongo {
            Some(m) => m,
            None => {
                self.reply_error(ctx, msg, "Moderation database unavailable.")
                    .await;
                return;
            }
        };

        match db_mongo::get_case(mongo, guild_id.get() as i64, case_number).await {
            Ok(Some(case)) => {
                self.reply_embed(ctx, msg, case_embed(&case)).await;
            }
            Ok(None) => {
                self.reply_error(ctx, msg, &format!("Case #{case_number} not found."))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to get case");
                self.reply_error(ctx, msg, "Failed to retrieve case.").await;
            }
        }
    }

    async fn cmd_cases(&self, ctx: &Context, msg: &Message, guild_id: GuildId, rest: &str) {
        let (target_str, _) = split_first(rest);
        let Some(target_id) = parse_user_id(target_str) else {
            self.reply_error(ctx, msg, "Usage: `cases <@member>`").await;
            return;
        };

        let mongo = match &self.state.mongo {
            Some(m) => m,
            None => {
                self.reply_error(ctx, msg, "Moderation database unavailable.")
                    .await;
                return;
            }
        };

        match db_mongo::get_cases_for_user(mongo, guild_id.get() as i64, target_id as i64).await {
            Ok(cases) if cases.is_empty() => {
                self.reply_error(ctx, msg, &format!("No cases found for <@{target_id}>."))
                    .await;
            }
            Ok(mut cases) => {
                cases.sort_by_key(|c| c.case_number);
                let mut embed = CreateEmbed::new()
                    .title(format!(
                        "Cases for {}",
                        self.fetch_name(ctx, target_id).await
                    ))
                    .description(format!("<@{target_id}> has **{}** case(s).", cases.len()))
                    .color(colors::BLURPLE)
                    .timestamp(Timestamp::now());
                for c in cases.iter().take(15) {
                    embed = embed.field(
                        format!(
                            "#{} \u{2022} {}",
                            c.case_number,
                            c.action_type.to_uppercase()
                        ),
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
                self.reply_embed(ctx, msg, embed).await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to get cases");
                self.reply_error(ctx, msg, "Failed to retrieve cases.")
                    .await;
            }
        }
    }

    async fn cmd_modlog(&self, ctx: &Context, msg: &Message, guild_id: GuildId) {
        let mongo = match &self.state.mongo {
            Some(m) => m,
            None => {
                self.reply_error(ctx, msg, "Moderation database unavailable.")
                    .await;
                return;
            }
        };

        match db_mongo::recent_cases(mongo, guild_id.get() as i64, 15).await {
            Ok(cases) if cases.is_empty() => {
                self.reply_error(ctx, msg, "No moderation actions recorded yet.")
                    .await;
            }
            Ok(cases) => {
                let mut embed = CreateEmbed::new()
                    .title("Recent Moderation Actions")
                    .color(colors::BLURPLE)
                    .timestamp(Timestamp::now());
                for c in &cases {
                    embed = embed.field(
                        format!(
                            "#{} \u{2022} {}",
                            c.case_number,
                            c.action_type.to_uppercase()
                        ),
                        format!(
                            "**Target:** <@{}>\n**Moderator:** <@{}>\n**Reason:** {}",
                            c.target_id,
                            c.moderator_id,
                            format::truncate(&c.reason, 150)
                        ),
                        false,
                    );
                }
                self.reply_embed(ctx, msg, embed).await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to get modlog");
                self.reply_error(ctx, msg, "Failed to retrieve the modlog.")
                    .await;
            }
        }
    }

    // ---- mute-role plumbing ----------------------------------------------

    /// Resolve the guild's Muted role, creating it (and denying send/speak
    /// permissions across every channel) when it is missing. Stores the id in
    /// `mod_config`.
    async fn ensure_mute_role(&self, ctx: &Context, guild_id: GuildId) -> Option<RoleId> {
        let gid = guild_id.get() as i64;

        let stored: Option<i64> = mod_config::Entity::find_by_id(gid)
            .one(self.state.servers_orm())
            .await
            .ok()
            .flatten()
            .and_then(|m| m.mute_role_id);

        // Reuse the stored role only if it still exists in the guild.
        if let Some(rid) = stored {
            if let Ok(roles) = guild_id.roles(&ctx.http).await {
                if roles.contains_key(&RoleId::new(rid as u64)) {
                    return Some(RoleId::new(rid as u64));
                }
            }
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
        .exec(self.state.servers_orm())
        .await;

        Some(role.id)
    }

    /// Next mod_timed primary key when no Mongo case number is available.
    async fn fallback_case_number(&self, guild_id: GuildId) -> i64 {
        // Highest existing case number for the guild (NULL/no rows -> 0), +1.
        let max: Option<i64> = mod_timed::Entity::find()
            .filter(mod_timed::Column::GuildId.eq(guild_id.get() as i64))
            .order_by_desc(mod_timed::Column::CaseNumber)
            .one(self.state.servers_orm())
            .await
            .ok()
            .flatten()
            .map(|m| m.case_number);
        max.unwrap_or(0) + 1
    }
}

// ---- free helpers ---------------------------------------------------------

/// Split off the first whitespace-delimited token, returning it plus the
/// trimmed remainder.
fn split_first(s: &str) -> (&str, String) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim().to_string()),
        None => (s, String::new()),
    }
}

/// Apply the default reason when none was supplied.
fn default_reason(reason: String) -> String {
    if reason.trim().is_empty() {
        "No reason provided.".to_string()
    } else {
        reason
    }
}

/// Pull a trailing 0-7 integer off the end of `s` as message-delete days,
/// returning the remaining reason text and the day count (default 0).
fn extract_delete_days(s: &str) -> (String, u8) {
    let s = s.trim();
    if s.is_empty() {
        return (String::new(), 0);
    }
    match s.rsplit_once(char::is_whitespace) {
        Some((rest, last)) => match last.parse::<u8>() {
            Ok(d) if d <= 7 => (rest.trim().to_string(), d),
            _ => (s.to_string(), 0),
        },
        // Whole string is a single token: a bare number means "no reason, N days".
        None => match s.parse::<u8>() {
            Ok(d) if d <= 7 => (String::new(), d),
            _ => (s.to_string(), 0),
        },
    }
}

/// Greedily peel a leading duration/time expression (up to 3 tokens) off
/// `after`, returning the resolved future instant and the remaining reason.
fn extract_duration(after: &str) -> Option<(DateTime<Utc>, String)> {
    let now = Utc::now();
    let tokens: Vec<&str> = after.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let max = tokens.len().min(3);
    // Longest prefix first so multi-word durations ("2 days") beat "2".
    for k in (1..=max).rev() {
        let candidate = tokens[..k].join(" ");
        if let Some(dt) = parse_when(&candidate, now) {
            if dt > now {
                return Some((dt, tokens[k..].join(" ")));
            }
        }
    }
    None
}

/// Detailed embed for a single case lookup.
fn case_embed(case: &ModCase) -> CreateEmbed {
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
        .field("When", &case.timestamp, false)
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
/// unbans for temp-bans), marks the Mongo case inactive, and clears the row.
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

                if let Some(mongo) = &state.mongo {
                    let _ = db_mongo::set_case_active(mongo, gid, case_number, false).await;
                }

                if !lifted {
                    tracing::warn!(guild = gid, case = case_number, action = %action, "failed to lift infraction");
                }
            }
        }
    });
}
