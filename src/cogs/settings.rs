use super::Cog;
use crate::entities::{
    goodbye_config, logging, prefixes, sentinel_config, settings_users, welcome_config,
};
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::{colors, embeds, perms};
use async_trait::async_trait;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serenity::all::{
    ButtonStyle, ComponentInteraction, CreateActionRow, CreateButton, CreateEmbed,
    CreateInteractionResponse, CreateInteractionResponseMessage, GuildId, Permissions, Timestamp,
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
}

#[async_trait]
impl Cog for SettingsCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
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
                // Re-check at confirm time: the author may have lost Manage
                // Server between invoking and confirming this destructive wipe.
                if !perms::has_perm(
                    ctx,
                    GuildId::new(guild_id),
                    interaction.user.id.get(),
                    Permissions::MANAGE_GUILD,
                )
                .await
                {
                    let _ = interaction
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new().ephemeral(true).content(
                                    "You need the **Manage Server** permission to reset settings.",
                                ),
                            ),
                        )
                        .await;
                    return;
                }
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

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![settings(), blacklist()]
}

// ---- helpers ---------------------------------------------------------------

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

// ---- settings commands -----------------------------------------------------

/// Server and user settings.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Settings",
    subcommand_required,
    subcommands("settings_show", "settings_reset", "settings_timezone")
)]
async fn settings(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Show the current server and your personal settings.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Settings",
    rename = "show",
    required_permissions = "MANAGE_GUILD"
)]
async fn settings_show(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let author = ctx.author();

    let prefixes = state.custom_prefixes(guild_id).await;
    let prefix_str = if prefixes.is_empty() {
        format!("`{}`", state.prefix())
    } else {
        prefixes
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let welcome_str = match state.welcome_cache.get(&guild_id) {
        Some(c) if c.enabled => c
            .channel_id
            .map(|id| format!("Enabled in <#{id}>"))
            .unwrap_or_else(|| "Enabled (no channel set)".to_string()),
        Some(_) => "Disabled".to_string(),
        None => "Not configured".to_string(),
    };

    let logging_str = match state.logging_cache.get(&guild_id) {
        Some(c) if c.enabled => "Enabled".to_string(),
        Some(_) => "Disabled".to_string(),
        None => "Not configured".to_string(),
    };

    let sentinel_str = match state.sentinel_cache.get(&guild_id) {
        Some(c) if c.enabled => "Enabled".to_string(),
        Some(_) => "Disabled".to_string(),
        None => "Not configured".to_string(),
    };

    let user_id = author.id.get() as i64;
    let user_row = settings_users::Entity::find_by_id(user_id)
        .one(state.users_orm())
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
        .field(format!("{}'s Timezone", author.name), timezone_str, false)
        .field("Patron Level", patron_level.to_string(), true)
        .field(
            "Blacklisted",
            if is_blacklisted { "Yes" } else { "No" }.to_string(),
            true,
        );

    send_embed(ctx, embed).await
}

/// Reset all server settings to defaults (requires confirmation).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Settings",
    rename = "reset",
    required_permissions = "MANAGE_GUILD"
)]
async fn settings_reset(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let author_id = ctx.author().id.get();

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

    ctx.send(
        poise::CreateReply::default()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(vec![confirm, cancel])]),
    )
    .await?;
    Ok(())
}

/// Set your personal timezone (IANA name, e.g. America/New_York).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Settings",
    rename = "timezone",
    required_permissions = "MANAGE_GUILD"
)]
async fn settings_timezone(
    ctx: Context<'_>,
    #[description = "IANA timezone"] timezone: String,
) -> Result<(), Error> {
    if !is_valid_timezone(&timezone) {
        return send_error(
            ctx,
            &format!(
                "`{timezone}` is not a valid timezone. Use an IANA name like `America/New_York` or `UTC`."
            ),
        )
        .await;
    }

    let state = &ctx.data().state;
    let user_id = ctx.author().id.get() as i64;

    let result = settings_users::Entity::insert(settings_users::ActiveModel {
        user_id: Set(user_id),
        timezone: Set(Some(timezone.clone())),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::column(settings_users::Column::UserId)
            .update_columns([settings_users::Column::Timezone])
            .to_owned(),
    )
    .exec(state.users_orm())
    .await;

    match result {
        Ok(_) => {
            send_embed(
                ctx,
                embeds::success_embed(
                    "Timezone Set",
                    &format!("Your timezone is now set to `{timezone}`."),
                ),
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to set timezone");
            send_error(ctx, "Database error.").await
        }
    }
}

// ---- blacklist commands ----------------------------------------------------

/// Bot owner blacklist management.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Settings",
    subcommand_required,
    subcommands("blacklist_add", "blacklist_remove")
)]
async fn blacklist(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Add a user to the bot blacklist (owner-only).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Settings",
    rename = "add",
    required_permissions = "MANAGE_GUILD"
)]
async fn blacklist_add(
    ctx: Context<'_>,
    user: serenity::all::User,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    if !state.is_owner(ctx.author().id.get()) {
        return send_error(ctx, "This command is owner-only.").await;
    }
    let user_id = user.id.get() as i64;
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
    .exec(state.users_orm())
    .await;
    ctx.say(format!("<@{user_id}> added to blacklist.")).await?;
    Ok(())
}

/// Remove a user from the bot blacklist (owner-only).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Settings",
    rename = "remove",
    required_permissions = "MANAGE_GUILD"
)]
async fn blacklist_remove(
    ctx: Context<'_>,
    user: serenity::all::User,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    if !state.is_owner(ctx.author().id.get()) {
        return send_error(ctx, "This command is owner-only.").await;
    }
    let user_id = user.id.get() as i64;
    let _ = settings_users::Entity::update_many()
        .col_expr(settings_users::Column::IsBlacklisted, Expr::value(false))
        .filter(settings_users::Column::UserId.eq(user_id))
        .exec(state.users_orm())
        .await;
    ctx.say(format!("<@{user_id}> removed from blacklist.")).await?;
    Ok(())
}
