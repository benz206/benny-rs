use super::Cog;
use crate::state::AppState;
use crate::utils::embeds::error_embed;
use crate::utils::format;
use async_trait::async_trait;
use serenity::all::{Colour, Context, CreateEmbed, CreateMessage, Message, Timestamp};
use std::sync::Arc;
use uuid::Uuid;

/// Crown icon used across premium embeds (mirrors premium.py `ICON`).
const ICON: &str = "\u{1F451}";

/// Aqua accent, mirrors premium.py's `style.Color.AQUA` (0x7FDBFF).
const AQUA: Colour = Colour::from_rgb(127, 219, 255);

/// Premium tiers. Backed by `settings_users.patron_level` (0-3) and
/// `premium_tokens.level`. Mirrors the design's
/// `PremiumLevel { None = 0, Basic = 1, Pro = 2, Max = 3 }`.
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

    pub fn as_u8(self) -> u8 {
        self as u8
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

pub struct PremiumCog {
    state: Arc<AppState>,
}

impl PremiumCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    // ---- shared helpers --------------------------------------------------

    async fn reply_embed(&self, ctx: &Context, msg: &Message, embed: CreateEmbed) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    async fn reply_error(&self, ctx: &Context, msg: &Message, text: &str) {
        self.reply_embed(ctx, msg, error_embed(text)).await;
    }

    /// Resolve a user's current premium tier as a raw level (0-3).
    async fn user_premium_level(&self, user_id: u64) -> u8 {
        let level: Option<(i64,)> =
            sqlx::query_as("SELECT patron_level FROM settings_users WHERE user_id = ?")
                .bind(user_id as i64)
                .fetch_optional(self.state.users_db())
                .await
                .ok()
                .flatten();
        PremiumLevel::from_level(level.map(|(l,)| l).unwrap_or(0)).as_u8()
    }

    // ---- commands --------------------------------------------------------

    /// `premium` / `premium info` — show the invoker's tier and perks.
    async fn cmd_info(&self, ctx: &Context, msg: &Message) {
        let level = PremiumLevel::from_level(self.user_premium_level(msg.author.id.get()).await as i64);
        let embed = CreateEmbed::new()
            .title(format!("{ICON} Premium Status"))
            .description(format!(
                "**Tier:** {}\n\n**Perks:**\n{}",
                level.name(),
                level.perks()
            ))
            .color(AQUA)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    /// `premium activate <token>` / `premium redeem <token>` — redeem a token.
    async fn cmd_activate(&self, ctx: &Context, msg: &Message, token: &str) {
        let token = token.trim();
        if token.is_empty() {
            self.reply_error(ctx, msg, "Usage: `premium activate <token>`").await;
            return;
        }

        let row: Option<(i64, i64)> =
            sqlx::query_as("SELECT level, redeemed FROM premium_tokens WHERE token = ?")
                .bind(token)
                .fetch_optional(self.state.users_db())
                .await
                .ok()
                .flatten();

        let Some((level, redeemed)) = row else {
            self.reply_error(ctx, msg, "That token is invalid.").await;
            return;
        };
        if redeemed != 0 {
            self.reply_error(ctx, msg, "That token has already been redeemed.").await;
            return;
        }

        let user_id = msg.author.id.get() as i64;

        // Burn the token, recording the redeemer as its owner.
        if let Err(e) = sqlx::query(
            "UPDATE premium_tokens SET redeemed = 1, owner_id = ? WHERE token = ?",
        )
        .bind(user_id)
        .bind(token)
        .execute(self.state.users_db())
        .await
        {
            tracing::error!(error = ?e, "failed to redeem premium token");
            self.reply_error(ctx, msg, "Failed to redeem that token. Try again later.").await;
            return;
        }

        // Apply the token's tier to the user.
        let _ = sqlx::query(
            "INSERT INTO settings_users (user_id, patron_level) VALUES (?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET patron_level = excluded.patron_level",
        )
        .bind(user_id)
        .bind(level)
        .execute(self.state.users_db())
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
        self.reply_embed(ctx, msg, embed).await;
    }

    /// `premium generate [level]` — OWNER ONLY. Mint a token and DM it.
    async fn cmd_generate(&self, ctx: &Context, msg: &Message, arg: &str) {
        if !self.state.is_owner(msg.author.id.get()) {
            self.reply_error(ctx, msg, "This command is owner-only.").await;
            return;
        }

        // Default to Basic (1); accept 1-3.
        let arg = arg.trim();
        let level: i64 = if arg.is_empty() {
            1
        } else {
            match arg.parse::<i64>() {
                Ok(n) if (1..=3).contains(&n) => n,
                _ => {
                    self.reply_error(
                        ctx,
                        msg,
                        "Level must be 1 (Basic), 2 (Pro), or 3 (Max).",
                    )
                    .await;
                    return;
                }
            }
        };

        let token = Uuid::new_v4().to_string();
        if let Err(e) = sqlx::query(
            "INSERT INTO premium_tokens (token, level, redeemed, owner_id) VALUES (?, ?, 0, NULL)",
        )
        .bind(&token)
        .bind(level)
        .execute(self.state.users_db())
        .await
        {
            tracing::error!(error = ?e, "failed to insert premium token");
            self.reply_error(ctx, msg, "Failed to generate token.").await;
            return;
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

        // Prefer DM to keep the token private; fall back to the channel
        // (owner-only context) if DMs are closed.
        let dm_ok = match msg.author.id.create_dm_channel(&ctx.http).await {
            Ok(dm) => dm
                .send_message(&ctx.http, CreateMessage::new().embed(dm_embed.clone()))
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
            self.reply_embed(ctx, msg, ack).await;
        } else {
            self.reply_embed(ctx, msg, dm_embed).await;
        }
    }

    /// `premium tokens` — OWNER ONLY. List outstanding & redeemed tokens.
    async fn cmd_tokens(&self, ctx: &Context, msg: &Message) {
        if !self.state.is_owner(msg.author.id.get()) {
            self.reply_error(ctx, msg, "This command is owner-only.").await;
            return;
        }

        let rows: Vec<(String, i64, i64, Option<i64>)> = sqlx::query_as(
            "SELECT token, level, redeemed, owner_id FROM premium_tokens ORDER BY redeemed ASC, token ASC",
        )
        .fetch_all(self.state.users_db())
        .await
        .unwrap_or_default();

        if rows.is_empty() {
            self.reply_error(ctx, msg, "No premium tokens have been generated yet.").await;
            return;
        }

        let mut outstanding: Vec<String> = Vec::new();
        let mut redeemed: Vec<String> = Vec::new();
        for (token, level, is_redeemed, owner) in &rows {
            let tier = PremiumLevel::from_level(*level);
            if *is_redeemed == 0 {
                outstanding.push(format!("`{}` \u{2014} **{}**", token, tier.name()));
            } else {
                redeemed.push(format!(
                    "`{}` \u{2014} **{}** \u{2192} <@{}>",
                    token,
                    tier.name(),
                    owner.unwrap_or(0)
                ));
            }
        }

        let embed = CreateEmbed::new()
            .title(format!("{ICON} Premium Tokens"))
            .color(AQUA)
            .field(
                format!("Outstanding ({})", outstanding.len()),
                Self::token_field(&outstanding),
                false,
            )
            .field(
                format!("Redeemed ({})", redeemed.len()),
                Self::token_field(&redeemed),
                false,
            )
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    /// Render up to 15 token lines into one embed field value (1024 char cap),
    /// noting any overflow.
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
}

#[async_trait]
impl Cog for PremiumCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        let mut it = body.splitn(2, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "premium" {
            return;
        }
        let rest = it.next().unwrap_or("").trim();

        // Split sub-command from its remaining argument.
        let mut parts = rest.splitn(2, ' ');
        let action = parts.next().unwrap_or("").trim();
        let arg = parts.next().unwrap_or("").trim();

        match action {
            "" | "info" => self.cmd_info(ctx, msg).await,
            "activate" | "redeem" => self.cmd_activate(ctx, msg, arg).await,
            "generate" | "gen" => self.cmd_generate(ctx, msg, arg).await,
            "tokens" | "list" => self.cmd_tokens(ctx, msg).await,
            _ => {
                self.reply_error(
                    ctx,
                    msg,
                    "Unknown subcommand. Try `premium info` or `premium activate <token>`.",
                )
                .await;
            }
        }
    }
}
