use super::Cog;
use crate::entities::logging;
use crate::state::{AppState, CommandInvocation, LoggingConfig};
use crate::utils::perms;
use async_trait::async_trait;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serenity::all::{
    ChannelId, Context, GuildChannel, GuildId, Member, Message, MessageId, Permissions, Role,
    RoleId, User,
};
use serenity::model::event::MessageUpdateEvent;
use std::sync::Arc;
use tracing::error;

// Webhook embed colors (u32 hex, posted as raw JSON to the webhook).
const C_GREEN: u32 = 0x57f287; // create / join / unban
const C_RED: u32 = 0xed4245; // delete / ban
const C_YELLOW: u32 = 0xfee75c; // edit
const C_ORANGE: u32 = 0xe67e22; // leave

pub struct LoggingCog {
    state: Arc<AppState>,
}

impl LoggingCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    /// POST a raw embed payload to the guild's configured webhook. No-op when
    /// the guild has no webhook configured or logging is disabled.
    async fn send_log(&self, guild_id: u64, payload: serde_json::Value) {
        let config = match self.state.logging_cache.get(&guild_id) {
            Some(c) if c.enabled && !c.webhook_url.is_empty() => c.clone(),
            _ => return,
        };

        if let Err(e) = self
            .state
            .http
            .post(&config.webhook_url)
            .json(&payload)
            .send()
            .await
        {
            error!(error = ?e, guild_id, "failed to send log webhook");
        }
    }

    /// Build a single color-coded, timestamped embed from `fields` and dispatch
    /// it to the guild webhook. Every logged event funnels through here so they
    /// all carry a timestamp.
    async fn log_event(
        &self,
        guild_id: u64,
        title: &str,
        color: u32,
        fields: Vec<(&str, String, bool)>,
    ) {
        let json_fields: Vec<serde_json::Value> = fields
            .into_iter()
            .map(|(name, value, inline)| {
                serde_json::json!({ "name": name, "value": value, "inline": inline })
            })
            .collect();

        let payload = serde_json::json!({
            "embeds": [{
                "title": title,
                "color": color,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "fields": json_fields,
            }]
        });
        self.send_log(guild_id, payload).await;
    }
}

/// Discord rejects embed field values that are empty or over 1024 chars. Normalize
/// arbitrary user content into a safe, non-empty, truncated value.
fn field_value(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        "*(empty)*".to_string()
    } else {
        crate::utils::format::truncate(t, 1000).to_string()
    }
}

/// `name (<@id>)` display for a user that survives the user having left.
fn user_display(u: &User) -> String {
    format!("{} (<@{}>)", u.name, u.id.get())
}

#[async_trait]
impl Cog for LoggingCog {
    async fn on_ready(&self, _ctx: &Context) {
        let rows = logging::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        for row in rows {
            self.state.logging_cache.insert(
                row.guild_id as u64,
                LoggingConfig {
                    webhook_url: row.webhook_url,
                    enabled: row.enabled,
                },
            );
        }
        tracing::info!("Logging configs loaded");
    }

    async fn on_command(&self, ctx: &Context, msg: &Message, inv: &CommandInvocation<'_>) -> bool {
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return false,
        };
        if inv.command != "logging" {
            return false;
        }

        let subcmd = inv.args;
        let mut parts = subcmd.splitn(2, ' ');
        let action = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();

        // Logging pipes guild-wide events — including message content — to a
        // stored webhook, so every subcommand requires Manage Server.
        if !perms::require_perm(
            ctx,
            msg,
            GuildId::new(guild_id),
            Permissions::MANAGE_GUILD,
            "Manage Server",
        )
        .await
        {
            return true;
        }

        match action {
            "setup" => {
                let webhook_url = arg;
                if webhook_url.is_empty() || !is_discord_webhook(webhook_url) {
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            "Usage: logging setup <webhook_url> (must be a Discord webhook URL)",
                        )
                        .await;
                    return true;
                }
                let _ = logging::Entity::insert(logging::ActiveModel {
                    guild_id: Set(guild_id as i64),
                    webhook_url: Set(webhook_url.to_string()),
                    enabled: Set(true),
                })
                .on_conflict(
                    OnConflict::column(logging::Column::GuildId)
                        .update_columns([logging::Column::WebhookUrl, logging::Column::Enabled])
                        .to_owned(),
                )
                .exec(self.state.servers_orm())
                .await;
                self.state.logging_cache.insert(
                    guild_id,
                    LoggingConfig {
                        webhook_url: webhook_url.to_string(),
                        enabled: true,
                    },
                );
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Logging webhook configured and enabled.")
                    .await;
            }
            "disable" => {
                let _ = logging::Entity::update_many()
                    .col_expr(logging::Column::Enabled, Expr::value(false))
                    .filter(logging::Column::GuildId.eq(guild_id as i64))
                    .exec(self.state.servers_orm())
                    .await;
                if let Some(mut e) = self.state.logging_cache.get_mut(&guild_id) {
                    e.enabled = false;
                }
                let _ = msg.channel_id.say(&ctx.http, "Logging disabled.").await;
            }
            "test" => {
                let payload = serde_json::json!({
                    "content": "Logging test — this webhook is working!"
                });
                self.send_log(guild_id, payload).await;
                let _ = msg.channel_id.say(&ctx.http, "Test log sent.").await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `logging setup <webhook_url>` | `logging disable` | `logging test`",
                    )
                    .await;
            }
        }
        true
    }

    async fn on_message_update(
        &self,
        _ctx: &Context,
        old: Option<Message>,
        new: Option<Message>,
        _event: &MessageUpdateEvent,
    ) {
        let new_msg = match &new {
            Some(m) => m,
            None => return,
        };
        if new_msg.author.bot {
            return;
        }
        let guild_id = match new_msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };

        // `old` is only populated when the previous message was in serenity's
        // message cache; otherwise the "before" content is unavailable.
        let old_content = old.as_ref().map(|m| m.content.as_str());
        let new_content = new_msg.content.as_str();

        // Identical content means a non-content edit (embed unfurl, pin, etc.);
        // nothing useful to log.
        if old_content == Some(new_content) {
            return;
        }

        let before = match old_content {
            Some(c) => field_value(c),
            None => "*(unavailable — not cached)*".to_string(),
        };

        self.log_event(
            guild_id,
            "Message Edited",
            C_YELLOW,
            vec![
                ("Author", user_display(&new_msg.author), true),
                ("Channel", format!("<#{}>", new_msg.channel_id.get()), true),
                ("Before", before, false),
                ("After", field_value(new_content), false),
            ],
        )
        .await;
    }

    async fn on_message_delete(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
        msg_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        let guild_id = match guild_id {
            Some(g) => g.get(),
            None => return,
        };

        // The delete event carries no content; recover author + content from the
        // message cache when available. The guard is drained synchronously here
        // so nothing is held across the later await.
        let cached = ctx
            .cache
            .message(channel_id, msg_id)
            .map(|m| (m.author.id.get(), m.author.bot, m.content.clone()));

        let mut fields = vec![("Channel", format!("<#{}>", channel_id.get()), true)];
        match cached {
            Some((author_id, is_bot, content)) => {
                if is_bot {
                    return;
                }
                fields.push(("Author", format!("<@{author_id}>"), true));
                fields.push(("Content", field_value(&content), false));
            }
            None => {
                fields.push(("Message ID", msg_id.get().to_string(), true));
                fields.push((
                    "Content",
                    "*(unavailable — message not cached)*".to_string(),
                    false,
                ));
            }
        }

        self.log_event(guild_id, "Message Deleted", C_RED, fields)
            .await;
    }

    async fn on_member_join(&self, _ctx: &Context, member: &Member) {
        let guild_id = member.guild_id.get();
        let created_timestamp = member.user.id.created_at().unix_timestamp();
        let now = chrono::Utc::now().timestamp();
        let account_age_days = (now - created_timestamp) / 86400;

        self.log_event(
            guild_id,
            "Member Joined",
            C_GREEN,
            vec![
                ("User", user_display(&member.user), true),
                ("Account Age", format!("{account_age_days} days"), true),
            ],
        )
        .await;
    }

    async fn on_member_leave(&self, _ctx: &Context, guild_id: GuildId, user: &User) {
        self.log_event(
            guild_id.get(),
            "Member Left",
            C_ORANGE,
            vec![("User", user_display(user), true)],
        )
        .await;
    }

    async fn on_member_ban(&self, _ctx: &Context, guild_id: GuildId, banned_user: &User) {
        self.log_event(
            guild_id.get(),
            "Member Banned",
            C_RED,
            vec![("User", user_display(banned_user), true)],
        )
        .await;
    }

    async fn on_member_unban(&self, _ctx: &Context, guild_id: GuildId, unbanned_user: &User) {
        self.log_event(
            guild_id.get(),
            "Member Unbanned",
            C_GREEN,
            vec![("User", user_display(unbanned_user), true)],
        )
        .await;
    }

    async fn on_channel_create(&self, _ctx: &Context, channel: &GuildChannel) {
        self.log_event(
            channel.guild_id.get(),
            "Channel Created",
            C_GREEN,
            vec![
                (
                    "Channel",
                    format!("{} (<#{}>)", channel.name, channel.id.get()),
                    true,
                ),
                ("Type", channel.kind.name().to_string(), true),
            ],
        )
        .await;
    }

    async fn on_channel_delete(&self, _ctx: &Context, channel: &GuildChannel) {
        // The channel is gone, so a `<#id>` mention would not resolve — show the
        // raw name and id instead.
        self.log_event(
            channel.guild_id.get(),
            "Channel Deleted",
            C_RED,
            vec![
                ("Name", format!("#{}", channel.name), true),
                ("Type", channel.kind.name().to_string(), true),
                ("ID", channel.id.get().to_string(), true),
            ],
        )
        .await;
    }

    async fn on_role_create(&self, _ctx: &Context, role: &Role) {
        self.log_event(
            role.guild_id.get(),
            "Role Created",
            C_GREEN,
            vec![
                (
                    "Role",
                    format!("{} (<@&{}>)", role.name, role.id.get()),
                    true,
                ),
                ("ID", role.id.get().to_string(), true),
            ],
        )
        .await;
    }

    async fn on_role_delete(
        &self,
        _ctx: &Context,
        guild_id: GuildId,
        role_id: RoleId,
        role: Option<Role>,
    ) {
        // `role` is only present when the deleted role was cached.
        let name = role
            .as_ref()
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "*(uncached role)*".to_string());

        self.log_event(
            guild_id.get(),
            "Role Deleted",
            C_RED,
            vec![
                ("Role", name, true),
                ("ID", role_id.get().to_string(), true),
            ],
        )
        .await;
    }
}

/// Whether `url` is a Discord webhook endpoint. Restricting the stored log
/// target to Discord's own hosts prevents a guild's event log — which carries
/// message edit/delete content — from being pointed at an arbitrary server.
fn is_discord_webhook(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "discord.com" | "discordapp.com" | "ptb.discord.com" | "canary.discord.com"
    ) && path.starts_with("api/webhooks/")
}
