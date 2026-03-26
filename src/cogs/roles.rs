use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, EditMessage, Message, RoleId};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct RolesCog {
    state: Arc<AppState>,
}

impl RolesCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    fn parse_role_id(s: &str) -> Option<u64> {
        let s = s.trim();
        if s.starts_with("<@&") && s.ends_with('>') {
            s[3..s.len() - 1].parse().ok()
        } else {
            s.parse().ok()
        }
    }
}

#[async_trait]
impl Cog for RolesCog {
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
        if cmd != "roleall" {
            return;
        }

        let sub = it.next().unwrap_or("").trim();
        let role_arg = it.next().unwrap_or("").trim();

        let (action, role_str) = if sub == "remove" {
            ("remove", role_arg)
        } else {
            ("add", sub)
        };

        let role_id = match Self::parse_role_id(role_str) {
            Some(id) => RoleId::new(id),
            None => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: roleall <@role> | roleall remove <@role>",
                    )
                    .await;
                return;
            }
        };

        let status_msg = msg
            .channel_id
            .say(
                &ctx.http,
                format!(
                    "Fetching members to {} role <@&{}>...",
                    action,
                    role_id.get()
                ),
            )
            .await;

        let members = match guild_id.members(&ctx.http, None, None).await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = ?e, "failed to fetch members");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to fetch members.")
                    .await;
                return;
            }
        };

        let total = members.len();
        let mut processed = 0usize;
        let mut errors = 0usize;

        for member in &members {
            if member.user.bot {
                continue;
            }

            let result = if action == "add" {
                ctx.http
                    .add_member_role(guild_id, member.user.id, role_id, None)
                    .await
            } else {
                ctx.http
                    .remove_member_role(guild_id, member.user.id, role_id, None)
                    .await
            };

            match result {
                Ok(_) => processed += 1,
                Err(_) => errors += 1,
            }

            // Rate limit: sleep 500ms between assignments
            sleep(Duration::from_millis(500)).await;

            // Update progress every 10 members
            if (processed + errors) % 10 == 0 {
                if let Ok(ref status) = status_msg {
                    let _ = status
                        .channel_id
                        .edit_message(
                            &ctx.http,
                            status.id,
                            EditMessage::new().content(format!(
                                "Processing... {}/{total} done ({errors} errors)",
                                processed + errors
                            )),
                        )
                        .await;
                }
            }
        }

        let action_past = if action == "add" {
            "added to"
        } else {
            "removed from"
        };
        let summary = format!(
            "Role <@&{}> {action_past} **{processed}** members. ({errors} errors, {total} total)",
            role_id.get()
        );
        if let Ok(ref status) = status_msg {
            let _ = status
                .channel_id
                .edit_message(
                    &ctx.http,
                    status.id,
                    EditMessage::new().content(summary),
                )
                .await;
        } else {
            let _ = msg.channel_id.say(&ctx.http, summary).await;
        }
    }
}
