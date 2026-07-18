use super::Cog;
use crate::entities::{premium_tokens, settings_users};
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::format;
use async_trait::async_trait;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect, Set};
use serenity::all::{Colour, CreateEmbed, CreateMessage, Timestamp};
use std::sync::Arc;
use uuid::Uuid;

/// Crown icon used across premium embeds.
const ICON: &str = "\u{1F451}";

/// Aqua accent (0x7FDBFF).
const AQUA: Colour = Colour::from_rgb(127, 219, 255);

/// Premium tiers. Backed by `settings_users.patron_level` (0-3) and
/// `premium_tokens.level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PremiumLevel {
    None = 0,
    Basic = 1,
    Pro = 2,
    Max = 3,
}

impl PremiumLevel {
    /// Map a stored integer level to a tier; unknown values clamp to `None`.
    pub fn from_level(level: i64) -> Self {
        match level {
            1 => Self::Basic,
            2 => Self::Pro,
            3 => Self::Max,
            _ => Self::None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Basic => "Basic",
            Self::Pro => "Pro",
            Self::Max => "Max",
        }
    }

    /// Human-readable perks unlocked at this tier (cumulative).
    pub fn perks(self) -> &'static str {
        match self {
            Self::None => {
                "No premium perks unlocked.\nRedeem a token with `premium activate <token>`."
            }
            Self::Basic => "\u{2022} Reduced command cooldowns (0.5\u{00D7})",
            Self::Pro => {
                "\u{2022} Reduced command cooldowns (0.5\u{00D7})\n\
                 \u{2022} Logging customization"
            }
            Self::Max => {
                "\u{2022} Reduced command cooldowns (0.5\u{00D7})\n\
                 \u{2022} Logging customization\n\
                 \u{2022} Custom Sentinel thresholds\n\
                 \u{2022} All current & future premium features"
            }
        }
    }
}

pub struct PremiumCog;

impl PremiumCog {
    pub fn new(_state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl Cog for PremiumCog {}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![premium()]
}

// ---- commands --------------------------------------------------------------

/// Show your premium tier and perks.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Premium",
    subcommands("premium_info", "premium_activate", "premium_generate", "premium_tokens_list")
)]
async fn premium(ctx: Context<'_>) -> Result<(), Error> {
    send_premium_info(ctx).await
}

/// Show your premium tier and perks.
#[poise::command(slash_command, prefix_command, rename = "info")]
async fn premium_info(ctx: Context<'_>) -> Result<(), Error> {
    send_premium_info(ctx).await
}

/// Activate a premium key.
#[poise::command(slash_command, prefix_command, rename = "activate")]
async fn premium_activate(
    ctx: Context<'_>,
    #[description = "Premium key"] key: String,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let token = key.trim();
    if token.is_empty() {
        return send_error(ctx, "Usage: `premium activate <token>`").await;
    }

    let row = premium_tokens::Entity::find_by_id(token)
        .one(state.users_orm())
        .await
        .ok()
        .flatten();

    let Some(m) = row else {
        return send_error(ctx, "That token is invalid.").await;
    };
    let level = m.level;
    if m.redeemed {
        return send_error(ctx, "That token has already been redeemed.").await;
    }

    let user_id = ctx.author().id.get() as i64;

    // Burn the token atomically: the UPDATE only matches while redeemed is
    // still false, so of two concurrent redemptions exactly one affects a
    // row — a token can never be redeemed twice via a check-then-act race.
    let burn = premium_tokens::Entity::update_many()
        .col_expr(premium_tokens::Column::Redeemed, Expr::value(true))
        .col_expr(premium_tokens::Column::OwnerId, Expr::value(user_id))
        .filter(premium_tokens::Column::Token.eq(token))
        .filter(premium_tokens::Column::Redeemed.eq(false))
        .exec(state.users_orm())
        .await;
    match burn {
        Ok(res) if res.rows_affected == 1 => {}
        Ok(_) => {
            return send_error(ctx, "That token has already been redeemed.").await;
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to redeem premium token");
            return send_error(ctx, "Failed to redeem that token. Try again later.").await;
        }
    }

    // Apply the token's tier, but never downgrade a user who already holds a
    // higher tier (e.g. redeeming a Basic token after a Max one).
    let existing = settings_users::Entity::find_by_id(user_id)
        .one(state.users_orm())
        .await
        .ok()
        .flatten()
        .map(|s| s.patron_level)
        .unwrap_or(0);
    let new_level = level.max(existing);
    let _ = settings_users::Entity::insert(settings_users::ActiveModel {
        user_id: Set(user_id),
        patron_level: Set(new_level),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::column(settings_users::Column::UserId)
            .update_columns([settings_users::Column::PatronLevel])
            .to_owned(),
    )
    .exec(state.users_orm())
    .await;

    let tier = PremiumLevel::from_level(level);
    let embed = CreateEmbed::new()
        .title(format!("{ICON} Premium Activated"))
        .description(format!(
            "Your token was redeemed. You are now **{}** tier!\n\n**Perks:**\n{}",
            tier.name(),
            tier.perks()
        ))
        .color(AQUA)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Generate a new premium token (owner only).
#[poise::command(
    slash_command,
    prefix_command,
    rename = "generate",
    owners_only,
    hide_in_help
)]
async fn premium_generate(
    ctx: Context<'_>,
    #[description = "Tier level (1 = Basic, 2 = Pro, 3 = Max)"]
    #[min = 1]
    #[max = 3]
    level: Option<i64>,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let level = level.unwrap_or(1);
    let token = Uuid::new_v4().to_string();

    if let Err(e) = premium_tokens::Entity::insert(premium_tokens::ActiveModel {
        token: Set(token.clone()),
        level: Set(level),
        redeemed: Set(false),
        owner_id: Set(None),
    })
    .exec(state.users_orm())
    .await
    {
        tracing::error!(error = ?e, "failed to insert premium token");
        return send_error(ctx, "Failed to generate token.").await;
    }

    let tier = PremiumLevel::from_level(level);
    let dm_embed = CreateEmbed::new()
        .title(format!("{ICON} Premium Token Generated"))
        .description(format!(
            "**Tier:** {}\n**Token:**\n```\n{}\n```\nRedeem with `premium activate {}`",
            tier.name(),
            token,
            token
        ))
        .color(AQUA)
        .timestamp(Timestamp::now());

    // Prefer DM to keep the token private; fall back to channel reply
    // (owner-only context) if DMs are closed.
    let sctx = ctx.serenity_context();
    let dm_ok = match ctx.author().id.create_dm_channel(&sctx.http).await {
        Ok(dm) => dm
            .send_message(&sctx.http, CreateMessage::new().embed(dm_embed.clone()))
            .await
            .is_ok(),
        Err(_) => false,
    };

    if dm_ok {
        let ack = CreateEmbed::new()
            .title(format!("{ICON} Premium Token Generated"))
            .description(format!(
                "A **{}** tier token was sent to your DMs.",
                tier.name()
            ))
            .color(AQUA)
            .timestamp(Timestamp::now());
        send_embed(ctx, ack).await
    } else {
        send_embed(ctx, dm_embed).await
    }
}

/// List all outstanding and redeemed premium tokens (owner only).
#[poise::command(
    slash_command,
    prefix_command,
    rename = "tokens",
    owners_only,
    hide_in_help
)]
async fn premium_tokens_list(ctx: Context<'_>) -> Result<(), Error> {
    let state = &ctx.data().state;

    let rows = premium_tokens::Entity::find()
        .order_by(premium_tokens::Column::Redeemed, Order::Asc)
        .order_by(premium_tokens::Column::Token, Order::Asc)
        .limit(100)
        .all(state.users_orm())
        .await
        .unwrap_or_default();

    if rows.is_empty() {
        return send_error(ctx, "No premium tokens have been generated yet.").await;
    }

    let mut outstanding: Vec<String> = Vec::new();
    let mut redeemed: Vec<String> = Vec::new();
    for row in &rows {
        let tier = PremiumLevel::from_level(row.level);
        if !row.redeemed {
            outstanding.push(format!("`{}` \u{2014} **{}**", row.token, tier.name()));
        } else {
            redeemed.push(format!(
                "`{}` \u{2014} **{}** \u{2192} <@{}>",
                row.token,
                tier.name(),
                row.owner_id.unwrap_or(0)
            ));
        }
    }

    let embed = CreateEmbed::new()
        .title(format!("{ICON} Premium Tokens"))
        .color(AQUA)
        .field(
            format!("Outstanding ({})", outstanding.len()),
            token_field(&outstanding),
            false,
        )
        .field(
            format!("Redeemed ({})", redeemed.len()),
            token_field(&redeemed),
            false,
        )
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

// ---- helpers ---------------------------------------------------------------

async fn send_premium_info(ctx: Context<'_>) -> Result<(), Error> {
    let state = &ctx.data().state;
    let user_id = ctx.author().id.get();
    let level = fetch_user_level(state, user_id).await;
    let embed = CreateEmbed::new()
        .title(format!("{ICON} Premium Status"))
        .description(format!(
            "**Tier:** {}\n\n**Perks:**\n{}",
            level.name(),
            level.perks()
        ))
        .color(AQUA)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

async fn fetch_user_level(state: &AppState, user_id: u64) -> PremiumLevel {
    let level = settings_users::Entity::find_by_id(user_id as i64)
        .one(state.users_orm())
        .await
        .ok()
        .flatten()
        .map(|m| m.patron_level);
    PremiumLevel::from_level(level.unwrap_or(0))
}

/// Render up to 15 token lines into one embed field value (1024-char cap),
/// noting any overflow. Mirrors the old `Self::token_field` method.
fn token_field(lines: &[String]) -> String {
    if lines.is_empty() {
        return "None".to_string();
    }
    let shown = lines.len().min(15);
    let mut body = lines[..shown].join("\n");
    if lines.len() > shown {
        body.push_str(&format!("\n\u{2026}and {} more", lines.len() - shown));
    }
    format::truncate(&body, 1024).to_string()
}
