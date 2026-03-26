use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub enum PremiumLevel {
    None,
    Basic,
    Pro,
    Ultimate,
}

impl PremiumLevel {
    pub fn from_level(level: i64) -> Self {
        match level {
            1 => Self::Basic,
            2 => Self::Pro,
            3 => Self::Ultimate,
            _ => Self::None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Basic => "Basic",
            Self::Pro => "Pro",
            Self::Ultimate => "Ultimate",
        }
    }
}

pub async fn check_premium(state: &AppState, user_id: u64) -> PremiumLevel {
    let level: Option<(i64,)> = sqlx::query_as(
        "SELECT patron_level FROM settings_users WHERE user_id = ?"
    )
    .bind(user_id as i64)
    .fetch_optional(state.users_db())
    .await
    .ok()
    .flatten();

    PremiumLevel::from_level(level.map(|(l,)| l).unwrap_or(0))
}

pub struct PremiumCog {
    state: Arc<AppState>,
}

impl PremiumCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for PremiumCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) { return; }
        let body = content[prefix.len()..].trim();
        let mut it = body.splitn(2, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "premium" { return; }
        let subcmd = it.next().unwrap_or("").trim();

        let mut parts = subcmd.splitn(2, ' ');
        let action = parts.next().unwrap_or("").trim();
        let _key = parts.next().unwrap_or("").trim();

        match action {
            "info" | "" => {
                let level = check_premium(&self.state, msg.author.id.get()).await;
                let _ = msg.channel_id.say(
                    &ctx.http,
                    format!(
                        "**Premium Status**\nTier: **{}**\n\n{}",
                        level.name(),
                        if level == PremiumLevel::None {
                            "You do not have premium. Premium features are not yet available for purchase."
                        } else {
                            "Thank you for your support!"
                        }
                    )
                ).await;
            }
            "activate" => {
                let _ = msg.channel_id.say(
                    &ctx.http,
                    "Premium activation is not yet available. Stay tuned!"
                ).await;
            }
            _ => {
                let _ = msg.channel_id.say(&ctx.http, "Usage: `premium info` | `premium activate <key>`").await;
            }
        }
    }
}
