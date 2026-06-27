use super::Cog;
use crate::entities::{goodbye_config, logging, prefixes, sentinel_config, settings_users, welcome_config};
use crate::state::AppState;
use crate::utils::{colors, embeds, parse};
use async_trait::async_trait;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serenity::all::{
    ButtonStyle, ComponentInteraction, Context, CreateActionRow, CreateButton, CreateEmbed,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, Message, Timestamp,
};
use std::sync::Arc;

/// custom_id namespace for this cog's interactive components. Every component
/// handled here is prefixed with this; `on_component` early-returns otherwise.
const CID_PREFIX: &str = "set:";

pub struct SettingsCog {
    state: Arc<AppState>,
}

impl SettingsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    /// Structural validation of an IANA-style timezone string.
    ///
    /// A full IANA database check would require the `chrono-tz` crate (not a
    /// dependency, and Cargo.toml is out of scope for this task), so this does a
    /// format check: accepts the common single-word zones plus `Area/Location`
    /// forms with sane characters. Rejects obvious garbage and whitespace.
    fn is_valid_timezone(tz: &str) -> bool {
        let tz = tz.trim();
        if tz.is_empty() || tz.len() > 64 {
            return false;
        }
        if !tz
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '+' | '-'))
        {
            return false;
        }
        let upper = tz.to_ascii_uppercase();
        if matches!(
            upper.as_str(),
            "UTC" | "GMT" | "LOCAL" | "ZULU" | "UNIVERSAL"
        ) {
            return true;
        }
        // Otherwise require an Area/Location form: at least two non-empty
        // segments, each beginning with a letter (e.g. America/New_York).
        let segs: Vec<&str> = tz.split('/').collect();
        segs.len() >= 2
            && segs
                .iter()
                .all(|s| s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
    }

    async fn guild_prefixes(&self, guild_id: u64) -> Vec<String> {
        if let Some(entry) = self.state.prefix_cache.get(&guild_id) {
            return entry.clone();
        }
        let rows = prefixes::Entity::find()
            .filter(prefixes::Column::GuildId.eq(guild_id as i64))
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();
        rows.into_iter().map(|m| m.prefix).collect()
    }
}

#[async_trait]
impl Cog for SettingsCog {
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
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };

        match cmd {
            "settings" => {
                let subcmd = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                match subcmd {
                    "show" | "view" | "list" => self.cmd_show(ctx, msg, guild_id).await,
                    "reset" => self.cmd_reset(ctx, msg, guild_id).await,
                    "timezone" | "tz" => self.cmd_timezone(ctx, msg, arg).await,
                    _ => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Usage: `settings show` | `settings reset` | `settings timezone <IANA tz>`",
                            )
                            .await;
                    }
                }
            }
            "blacklist" => {
                let subcmd = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                self.cmd_blacklist(ctx, msg, subcmd, arg).await;
            }
            _ => {}
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let custom_id = interaction.data.custom_id.as_str();
        if !custom_id.starts_with(CID_PREFIX) {
            return;
        }

        // Expected: set:reset:<confirm|cancel>:<guild_id>:<author_id>
        let parts: Vec<&str> = custom_id.split(':').collect();
        if parts.len() != 5 || parts[1] != "reset" {
            return;
        }
        let action = parts[2];
        let guild_id: u64 = match parts[3].parse() {
            Ok(g) => g,
            Err(_) => return,
        };
        let author_id: u64 = match parts[4].parse() {
            Ok(a) => a,
            Err(_) => return,
        };

        // Only the user who invoked the command may resolve the confirmation.
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

        let response = match action {
            "confirm" => {
                self.do_reset(guild_id).await;
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(embeds::success_embed(
                            "Settings Reset",
                            "All server settings have been reset to defaults.",
                        ))
                        .components(vec![]),
                )
            }
            "cancel" => CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(
                        CreateEmbed::new()
                            .title("Reset Cancelled")
                            .description("No settings were changed.")
                            .color(colors::GRAY)
                            .timestamp(Timestamp::now()),
                    )
                    .components(vec![]),
            ),
            _ => return,
        };
        let _ = interaction.create_response(&ctx.http, response).await;
    }
}

impl SettingsCog {
    async fn cmd_show(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        // --- Server settings ---
        let prefixes = self.guild_prefixes(guild_id).await;
        let prefix_str = if prefixes.is_empty() {
            format!("`{}`", self.state.prefix())
        } else {
            prefixes
                .iter()
                .map(|p| format!("`{p}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let welcome_str = match self.state.welcome_cache.get(&guild_id) {
            Some(c) if c.enabled => c
                .channel_id
                .map(|id| format!("Enabled in <#{id}>"))
                .unwrap_or_else(|| "Enabled (no channel set)".to_string()),
            Some(_) => "Disabled".to_string(),
            None => "Not configured".to_string(),
        };

        let logging_str = match self.state.logging_cache.get(&guild_id) {
            Some(c) if c.enabled => "Enabled".to_string(),
            Some(_) => "Disabled".to_string(),
            None => "Not configured".to_string(),
        };

        let sentinel_str = match self.state.sentinel_cache.get(&guild_id) {
            Some(c) if c.enabled => "Enabled".to_string(),
            Some(_) => "Disabled".to_string(),
            None => "Not configured".to_string(),
        };

        // --- User settings (settings_users) ---
        let user_id = msg.author.id.get() as i64;
        let user_row = settings_users::Entity::find_by_id(user_id)
            .one(self.state.users_orm())
            .await
            .ok()
            .flatten();

        let (timezone, patron_level, is_blacklisted) = match user_row {
            Some(m) => (m.timezone, m.patron_level, m.is_blacklisted),
            None => (None, 0, false),
        };
        let timezone_str = timezone.unwrap_or_else(|| "Not set".to_string());

        let embed = CreateEmbed::new()
            .title("Settings")
            .color(colors::BLURPLE)
            .timestamp(Timestamp::now())
            .field("Prefixes", prefix_str, false)
            .field("Welcome", welcome_str, true)
            .field("Logging", logging_str, true)
            .field("Sentinel", sentinel_str, true)
            .field(
                format!("{}'s Timezone", msg.author.name),
                timezone_str,
                false,
            )
            .field("Patron Level", patron_level.to_string(), true)
            .field(
                "Blacklisted",
                if is_blacklisted { "Yes" } else { "No" }.to_string(),
                true,
            );

        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    /// Send a confirmation prompt; the destructive work happens in
    /// `on_component` -> `do_reset` once the invoker clicks Confirm.
    async fn cmd_reset(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let author_id = msg.author.id.get();
        let confirm = CreateButton::new(format!("set:reset:confirm:{guild_id}:{author_id}"))
            .label("Confirm")
            .style(ButtonStyle::Danger)
            .emoji('✅');
        let cancel = CreateButton::new(format!("set:reset:cancel:{guild_id}:{author_id}"))
            .label("Cancel")
            .style(ButtonStyle::Secondary)
            .emoji('❌');

        let embed = CreateEmbed::new()
            .title("Reset Server Settings?")
            .description(
                "This will reset all server settings (prefixes, welcome, goodbye, logging, \
                 sentinel) to their defaults. This cannot be undone.",
            )
            .color(colors::YELLOW)
            .timestamp(Timestamp::now());

        let builder = CreateMessage::new()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(vec![confirm, cancel])]);
        let _ = msg.channel_id.send_message(&ctx.http, builder).await;
    }

    /// Delete every per-guild settings row and drop the cached copies.
    async fn do_reset(&self, guild_id: u64) {
        let gid = guild_id as i64;
        let _ = prefixes::Entity::delete_many()
            .filter(prefixes::Column::GuildId.eq(gid))
            .exec(self.state.servers_orm())
            .await;
        let _ = welcome_config::Entity::delete_many()
            .filter(welcome_config::Column::GuildId.eq(gid))
            .exec(self.state.servers_orm())
            .await;
        let _ = goodbye_config::Entity::delete_many()
            .filter(goodbye_config::Column::GuildId.eq(gid))
            .exec(self.state.servers_orm())
            .await;
        let _ = logging::Entity::delete_many()
            .filter(logging::Column::GuildId.eq(gid))
            .exec(self.state.servers_orm())
            .await;
        let _ = sentinel_config::Entity::delete_many()
            .filter(sentinel_config::Column::GuildId.eq(gid))
            .exec(self.state.servers_orm())
            .await;

        self.state.prefix_cache.remove(&guild_id);
        self.state.welcome_cache.remove(&guild_id);
        self.state.goodbye_cache.remove(&guild_id);
        self.state.logging_cache.remove(&guild_id);
        self.state.sentinel_cache.remove(&guild_id);
    }

    async fn cmd_timezone(&self, ctx: &Context, msg: &Message, arg: &str) {
        if arg.is_empty() {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Usage: `settings timezone <IANA tz>` (e.g. `America/New_York`)",
                )
                .await;
            return;
        }

        if !Self::is_valid_timezone(arg) {
            let _ = msg
                .channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new().embed(embeds::error_embed(&format!(
                        "`{arg}` is not a valid timezone. Use an IANA name like `America/New_York` or `UTC`."
                    ))),
                )
                .await;
            return;
        }

        let user_id = msg.author.id.get() as i64;
        let result = settings_users::Entity::insert(settings_users::ActiveModel {
            user_id: Set(user_id),
            timezone: Set(Some(arg.to_string())),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(settings_users::Column::UserId)
                .update_columns([settings_users::Column::Timezone])
                .to_owned(),
        )
        .exec(self.state.users_orm())
        .await;

        match result {
            Ok(_) => {
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Timezone Set",
                            &format!("Your timezone is now set to `{arg}`."),
                        )),
                    )
                    .await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to set timezone");
                let _ = msg.channel_id.say(&ctx.http, "Database error.").await;
            }
        }
    }

    async fn cmd_blacklist(&self, ctx: &Context, msg: &Message, subcmd: &str, arg: &str) {
        // The blacklist is a global (cross-guild) gate on bot usage, so only the
        // bot owner may edit it — never a regular member (who could otherwise
        // un-blacklist themselves or blacklist others).
        if !self.state.is_owner(msg.author.id.get()) {
            let _ = msg
                .channel_id
                .say(&ctx.http, "This command is owner-only.")
                .await;
            return;
        }
        let user_id = match parse::parse_user_id(arg) {
            Some(id) => id as i64,
            None => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `blacklist add <@user>` | `blacklist remove <@user>`",
                    )
                    .await;
                return;
            }
        };

        match subcmd {
            "add" => {
                let _ = settings_users::Entity::insert(settings_users::ActiveModel {
                    user_id: Set(user_id),
                    is_blacklisted: Set(true),
                    ..Default::default()
                })
                .on_conflict(
                    OnConflict::column(settings_users::Column::UserId)
                        .update_columns([settings_users::Column::IsBlacklisted])
                        .to_owned(),
                )
                .exec(self.state.users_orm())
                .await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("<@{user_id}> added to blacklist."))
                    .await;
            }
            "remove" => {
                let _ = settings_users::Entity::update_many()
                    .col_expr(
                        settings_users::Column::IsBlacklisted,
                        Expr::value(false),
                    )
                    .filter(settings_users::Column::UserId.eq(user_id))
                    .exec(self.state.users_orm())
                    .await;
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("<@{user_id}> removed from blacklist."))
                    .await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `blacklist add <@user>` | `blacklist remove <@user>`",
                    )
                    .await;
            }
        }
    }
}
