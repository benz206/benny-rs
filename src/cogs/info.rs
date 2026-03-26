use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message, RoleId, UserId};
use std::sync::Arc;

pub struct InfoCog {
    state: Arc<AppState>,
}

impl InfoCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    fn parse_user_id(s: &str) -> Option<u64> {
        let s = s.trim();
        if s.starts_with("<@") && s.ends_with('>') {
            s[2..s.len() - 1].trim_start_matches('!').parse().ok()
        } else {
            s.parse().ok()
        }
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
impl Cog for InfoCog {
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
        let arg = it.next().unwrap_or("").trim();

        match cmd {
            "userinfo" | "ui" | "whois" => self.cmd_userinfo(ctx, msg, arg).await,
            "serverinfo" | "si" | "guildinfo" => self.cmd_serverinfo(ctx, msg).await,
            "roleinfo" | "ri" => self.cmd_roleinfo(ctx, msg, arg).await,
            "avatar" | "av" => self.cmd_avatar(ctx, msg, arg).await,
            _ => {}
        }
    }
}

impl InfoCog {
    async fn cmd_userinfo(&self, ctx: &Context, msg: &Message, arg: &str) {
        let guild_id = match msg.guild_id {
            Some(g) => g,
            None => return,
        };

        let target_id = if arg.is_empty() {
            msg.author.id.get()
        } else {
            match Self::parse_user_id(arg) {
                Some(id) => id,
                None => {
                    let _ = msg.channel_id.say(&ctx.http, "User not found.").await;
                    return;
                }
            }
        };

        match guild_id.member(&ctx.http, UserId::new(target_id)).await {
            Ok(member) => {
                let user = &member.user;
                let joined_at = member
                    .joined_at
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                let created_at = user.id.created_at().to_string();
                let roles: Vec<String> = member
                    .roles
                    .iter()
                    .map(|r| format!("<@&{}>", r.get()))
                    .collect();
                let roles_str = if roles.is_empty() {
                    "None".to_string()
                } else {
                    roles.join(", ")
                };
                let nickname = member.nick.as_deref().unwrap_or("None");
                let avatar = user
                    .avatar_url()
                    .unwrap_or_else(|| user.default_avatar_url());

                let text = format!(
                    "**User Info: {}**\n\
                    **ID:** {}\n\
                    **Nickname:** {}\n\
                    **Account Created:** {}\n\
                    **Joined Server:** {}\n\
                    **Roles ({}):** {}\n\
                    **Avatar:** {}",
                    user.name,
                    user.id.get(),
                    nickname,
                    created_at,
                    joined_at,
                    member.roles.len(),
                    roles_str,
                    avatar
                );
                let _ = msg.channel_id.say(&ctx.http, text).await;
            }
            Err(_) => {
                match UserId::new(target_id).to_user(&ctx.http).await {
                    Ok(user) => {
                        let created_at = user.id.created_at().to_string();
                        let avatar = user
                            .avatar_url()
                            .unwrap_or_else(|| user.default_avatar_url());
                        let text = format!(
                            "**User Info: {}**\n**ID:** {}\n**Account Created:** {}\n**Avatar:** {}",
                            user.name,
                            user.id.get(),
                            created_at,
                            avatar
                        );
                        let _ = msg.channel_id.say(&ctx.http, text).await;
                    }
                    Err(_) => {
                        let _ = msg.channel_id.say(&ctx.http, "User not found.").await;
                    }
                }
            }
        }
    }

    async fn cmd_serverinfo(&self, ctx: &Context, msg: &Message) {
        let guild_id = match msg.guild_id {
            Some(g) => g,
            None => return,
        };
        match ctx.http.get_guild(guild_id).await {
            Ok(guild) => {
                let created_at = guild.id.created_at().to_string();
                let member_count = guild.approximate_member_count.unwrap_or(0);
                let icon = guild.icon_url().unwrap_or_else(|| "No icon".to_string());
                let text = format!(
                    "**Server Info: {}**\n\
                    **ID:** {}\n\
                    **Owner:** <@{}>\n\
                    **Members:** {}\n\
                    **Roles:** {}\n\
                    **Created:** {}\n\
                    **Icon:** {}",
                    guild.name,
                    guild.id.get(),
                    guild.owner_id.get(),
                    member_count,
                    guild.roles.len(),
                    created_at,
                    icon
                );
                let _ = msg.channel_id.say(&ctx.http, text).await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to get guild info");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to get server info.")
                    .await;
            }
        }
    }

    async fn cmd_roleinfo(&self, ctx: &Context, msg: &Message, arg: &str) {
        if arg.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: roleinfo <@role>")
                .await;
            return;
        }
        let guild_id = match msg.guild_id {
            Some(g) => g,
            None => return,
        };
        let role_id = match Self::parse_role_id(arg) {
            Some(id) => RoleId::new(id),
            None => {
                let _ = msg.channel_id.say(&ctx.http, "Invalid role.").await;
                return;
            }
        };

        match ctx.http.get_guild(guild_id).await {
            Ok(guild) => match guild.roles.get(&role_id) {
                Some(role) => {
                    let created_at = role.id.created_at().to_string();
                    let color = format!("#{:06X}", role.colour.0);
                    let text = format!(
                        "**Role Info: {}**\n\
                            **ID:** {}\n\
                            **Color:** {}\n\
                            **Hoisted:** {}\n\
                            **Mentionable:** {}\n\
                            **Position:** {}\n\
                            **Created:** {}",
                        role.name,
                        role.id.get(),
                        color,
                        role.hoist,
                        role.mentionable,
                        role.position,
                        created_at
                    );
                    let _ = msg.channel_id.say(&ctx.http, text).await;
                }
                None => {
                    let _ = msg.channel_id.say(&ctx.http, "Role not found.").await;
                }
            },
            Err(_) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to get role info.")
                    .await;
            }
        }
    }

    async fn cmd_avatar(&self, ctx: &Context, msg: &Message, arg: &str) {
        let target_id = if arg.is_empty() {
            msg.author.id.get()
        } else {
            match Self::parse_user_id(arg) {
                Some(id) => id,
                None => {
                    let _ = msg.channel_id.say(&ctx.http, "User not found.").await;
                    return;
                }
            }
        };

        match UserId::new(target_id).to_user(&ctx.http).await {
            Ok(user) => {
                let avatar = user
                    .avatar_url()
                    .map(|u| u.replace(".webp", ".png"))
                    .unwrap_or_else(|| user.default_avatar_url());
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!("**{}'s avatar:**\n{}", user.name, avatar),
                    )
                    .await;
            }
            Err(_) => {
                let _ = msg.channel_id.say(&ctx.http, "User not found.").await;
            }
        }
    }
}
