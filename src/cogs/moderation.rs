use super::Cog;
use crate::db_mongo::{self, ModCase};
use crate::state::AppState;
use async_trait::async_trait;
use chrono::Utc;
use serenity::all::{Context, GuildId, Message, UserId};
use std::sync::Arc;

pub struct ModerationCog {
    state: Arc<AppState>,
}

impl ModerationCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for ModerationCog {
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
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };
        let rest = it.collect::<Vec<_>>().join(" ");

        match cmd {
            "warn" => self.cmd_action(ctx, msg, guild_id, "warn", &rest).await,
            "kick" => self.cmd_action(ctx, msg, guild_id, "kick", &rest).await,
            "ban" => self.cmd_action(ctx, msg, guild_id, "ban", &rest).await,
            "unban" => self.cmd_unban(ctx, msg, guild_id, &rest).await,
            "case" => self.cmd_case(ctx, msg, guild_id, &rest).await,
            "cases" => self.cmd_cases(ctx, msg, guild_id, &rest).await,
            _ => {}
        }
    }
}

impl ModerationCog {
    fn parse_user_id(s: &str) -> Option<u64> {
        let s = s.trim();
        if s.starts_with("<@") && s.ends_with('>') {
            s[2..s.len() - 1].trim_start_matches('!').parse().ok()
        } else {
            s.parse().ok()
        }
    }

    async fn create_case(
        &self,
        guild_id: GuildId,
        action_type: &str,
        target_id: u64,
        moderator_id: u64,
        reason: &str,
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
        };

        if let Err(e) = db_mongo::insert_case(mongo, &case).await {
            tracing::error!(error = ?e, "failed to insert case");
            return None;
        }

        Some(case_number)
    }

    async fn cmd_action(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        action: &str,
        rest: &str,
    ) {
        let mut parts = rest.splitn(2, ' ');
        let target_str = parts.next().unwrap_or("").trim();
        let reason = parts
            .next()
            .unwrap_or("No reason provided.")
            .trim()
            .to_string();

        let target_id = match Self::parse_user_id(target_str) {
            Some(id) => id,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Usage: {action} <@user> [reason]"))
                    .await;
                return;
            }
        };

        let moderator_id = msg.author.id.get();
        let target_user_id = UserId::new(target_id);

        let action_result = match action {
            "kick" => {
                match guild_id
                    .kick_with_reason(&ctx.http, target_user_id, &reason)
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            }
            "ban" => {
                match guild_id
                    .ban_with_reason(&ctx.http, target_user_id, 0, &reason)
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            }
            "warn" => Ok(()),
            _ => Ok(()),
        };

        match action_result {
            Err(e) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Failed to {action}: {e}"))
                    .await;
            }
            Ok(()) => {
                let case_number = self
                    .create_case(guild_id, action, target_id, moderator_id, &reason)
                    .await;
                let case_str = case_number
                    .map(|n| format!(" (Case #{n})"))
                    .unwrap_or_default();
                let action_past = match action {
                    "kick" => "kicked",
                    "ban" => "banned",
                    "warn" => "warned",
                    _ => action,
                };
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "✅ <@{target_id}> has been {action_past}.{case_str}\n**Reason:** {reason}"
                        ),
                    )
                    .await;
            }
        }
    }

    async fn cmd_unban(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        rest: &str,
    ) {
        let target_str = rest.trim();
        let target_id = match Self::parse_user_id(target_str) {
            Some(id) => id,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Usage: unban <user_id>")
                    .await;
                return;
            }
        };

        match guild_id.unban(&ctx.http, UserId::new(target_id)).await {
            Ok(_) => {
                let case_number = self
                    .create_case(
                        guild_id,
                        "unban",
                        target_id,
                        msg.author.id.get(),
                        "Unbanned",
                    )
                    .await;
                let case_str = case_number
                    .map(|n| format!(" (Case #{n})"))
                    .unwrap_or_default();
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!("✅ <@{target_id}> has been unbanned.{case_str}"),
                    )
                    .await;
            }
            Err(e) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Failed to unban: {e}"))
                    .await;
            }
        }
    }

    async fn cmd_case(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        rest: &str,
    ) {
        let case_number: i64 = match rest.trim().parse() {
            Ok(n) => n,
            Err(_) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Usage: case <number>")
                    .await;
                return;
            }
        };

        let mongo = match &self.state.mongo {
            Some(m) => m,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Moderation database unavailable.")
                    .await;
                return;
            }
        };

        match db_mongo::get_case(mongo, guild_id.get() as i64, case_number).await {
            Ok(Some(case)) => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "**Case #{}** — {}\n**Target:** <@{}>\n**Moderator:** <@{}>\n**Reason:** {}\n**Time:** {}",
                            case.case_number,
                            case.action_type.to_uppercase(),
                            case.target_id,
                            case.moderator_id,
                            case.reason,
                            case.timestamp
                        ),
                    )
                    .await;
            }
            Ok(None) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Case #{case_number} not found."))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to get case");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to retrieve case.")
                    .await;
            }
        }
    }

    async fn cmd_cases(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: GuildId,
        rest: &str,
    ) {
        let target_str = rest.trim();
        let target_id = match Self::parse_user_id(target_str) {
            Some(id) => id,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Usage: cases <@user>")
                    .await;
                return;
            }
        };

        let mongo = match &self.state.mongo {
            Some(m) => m,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Moderation database unavailable.")
                    .await;
                return;
            }
        };

        match db_mongo::get_cases_for_user(mongo, guild_id.get() as i64, target_id as i64).await {
            Ok(cases) if cases.is_empty() => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!("No cases found for <@{target_id}>."),
                    )
                    .await;
            }
            Ok(cases) => {
                let mut lines =
                    vec![format!("**Cases for <@{target_id}> ({}):**", cases.len())];
                for c in cases.iter().take(10) {
                    lines.push(format!(
                        "**#{}** {} — {}",
                        c.case_number, c.action_type, c.reason
                    ));
                }
                if cases.len() > 10 {
                    lines.push(format!("... and {} more", cases.len() - 10));
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, lines.join("\n"))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to get cases");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to retrieve cases.")
                    .await;
            }
        }
    }
}
