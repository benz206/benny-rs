use super::Cog;
use crate::state::{AppState, GoodbyeConfig, WelcomeConfig};
use crate::tagscript::{self, TagContext};
use async_trait::async_trait;
use serenity::all::{Context, GuildId, Member, User};
use serenity::prelude::Mentionable;
use std::sync::Arc;

pub struct WelcomeCog {
    state: Arc<AppState>,
}

impl WelcomeCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for WelcomeCog {
    async fn on_ready(&self, _ctx: &Context) {
        // Load welcome configs - enabled is INTEGER in SQLite, query as i64
        let rows: Vec<(i64, Option<i64>, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT guild_id, channel_id, message, embed_json, enabled FROM welcome_config",
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        for (guild_id, channel_id, message, embed_json, enabled) in rows {
            self.state.welcome_cache.insert(
                guild_id as u64,
                WelcomeConfig {
                    channel_id,
                    message,
                    embed_json,
                    enabled: enabled != 0,
                },
            );
        }

        // Load goodbye configs
        let rows: Vec<(i64, Option<i64>, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT guild_id, channel_id, message, embed_json, enabled FROM goodbye_config",
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        for (guild_id, channel_id, message, embed_json, enabled) in rows {
            self.state.goodbye_cache.insert(
                guild_id as u64,
                GoodbyeConfig {
                    channel_id,
                    message,
                    embed_json,
                    enabled: enabled != 0,
                },
            );
        }

        tracing::info!("Welcome/Goodbye configs loaded");
    }

    async fn on_message(&self, ctx: &Context, msg: &serenity::all::Message) {
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
            "welcome" => {
                let subcmd = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                self.handle_welcome_cmd(ctx, msg, guild_id, subcmd, arg)
                    .await;
            }
            "goodbye" => {
                let subcmd = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                self.handle_goodbye_cmd(ctx, msg, guild_id, subcmd, arg)
                    .await;
            }
            _ => {}
        }
    }

    async fn on_member_join(&self, ctx: &Context, member: &Member) {
        let guild_id = member.guild_id.get();
        let config = match self.state.welcome_cache.get(&guild_id) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let channel_id = match config.channel_id {
            Some(id) => id as u64,
            None => return,
        };

        let tag_ctx = TagContext {
            user_name: member.user.name.clone(),
            user_mention: member.mention().to_string(),
            user_id: member.user.id.get().to_string(),
            user_avatar: member.user.avatar_url().unwrap_or_default(),
            server_id: guild_id.to_string(),
            channel_id: channel_id.to_string(),
            ..Default::default()
        };
        let mut tag_ctx = tag_ctx;
        let output = tagscript::run(&config.message, &mut tag_ctx);

        use serenity::all::ChannelId;
        let _ = ChannelId::new(channel_id)
            .say(&ctx.http, output.content)
            .await;
    }

    async fn on_member_leave(&self, ctx: &Context, guild_id: GuildId, user: &User) {
        let gid = guild_id.get();
        let config = match self.state.goodbye_cache.get(&gid) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let channel_id = match config.channel_id {
            Some(id) => id as u64,
            None => return,
        };

        let tag_ctx = TagContext {
            user_name: user.name.clone(),
            user_mention: user.mention().to_string(),
            user_id: user.id.get().to_string(),
            user_avatar: user.avatar_url().unwrap_or_default(),
            server_id: gid.to_string(),
            channel_id: channel_id.to_string(),
            ..Default::default()
        };
        let mut tag_ctx = tag_ctx;
        let output = tagscript::run(&config.message, &mut tag_ctx);

        use serenity::all::ChannelId;
        let _ = ChannelId::new(channel_id)
            .say(&ctx.http, output.content)
            .await;
    }
}

impl WelcomeCog {
    async fn handle_welcome_cmd(
        &self,
        ctx: &Context,
        msg: &serenity::all::Message,
        guild_id: u64,
        subcmd: &str,
        arg: &str,
    ) {
        match subcmd {
            "channel" => {
                let channel_id = parse_channel_id(arg);
                match channel_id {
                    None => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Please mention a channel or provide a channel ID.",
                            )
                            .await;
                    }
                    Some(id) => {
                        let _ = sqlx::query(
                            "INSERT INTO welcome_config (guild_id, channel_id) VALUES (?, ?) \
                             ON CONFLICT(guild_id) DO UPDATE SET channel_id = excluded.channel_id",
                        )
                        .bind(guild_id as i64)
                        .bind(id as i64)
                        .execute(self.state.servers_db())
                        .await;

                        let mut entry =
                            self.state.welcome_cache.entry(guild_id).or_insert_with(|| {
                                WelcomeConfig {
                                    channel_id: None,
                                    message: "Welcome {user.mention} to {server}!".to_string(),
                                    embed_json: None,
                                    enabled: false,
                                }
                            });
                        entry.channel_id = Some(id as i64);
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, format!("Welcome channel set to <#{id}>."))
                            .await;
                    }
                }
            }
            "message" => {
                if arg.is_empty() {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Please provide a message template.")
                        .await;
                    return;
                }
                let _ = sqlx::query(
                    "INSERT INTO welcome_config (guild_id, message) VALUES (?, ?) \
                     ON CONFLICT(guild_id) DO UPDATE SET message = excluded.message",
                )
                .bind(guild_id as i64)
                .bind(arg)
                .execute(self.state.servers_db())
                .await;

                let mut entry =
                    self.state.welcome_cache.entry(guild_id).or_insert_with(|| WelcomeConfig {
                        channel_id: None,
                        message: arg.to_string(),
                        embed_json: None,
                        enabled: false,
                    });
                entry.message = arg.to_string();
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Welcome message updated.")
                    .await;
            }
            "enable" => {
                let _ = sqlx::query(
                    "INSERT INTO welcome_config (guild_id, enabled) VALUES (?, 1) \
                     ON CONFLICT(guild_id) DO UPDATE SET enabled = 1",
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                if let Some(mut e) = self.state.welcome_cache.get_mut(&guild_id) {
                    e.enabled = true;
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Welcome messages enabled.")
                    .await;
            }
            "disable" => {
                let _ = sqlx::query(
                    "INSERT INTO welcome_config (guild_id, enabled) VALUES (?, 0) \
                     ON CONFLICT(guild_id) DO UPDATE SET enabled = 0",
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                if let Some(mut e) = self.state.welcome_cache.get_mut(&guild_id) {
                    e.enabled = false;
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Welcome messages disabled.")
                    .await;
            }
            _ => {
                let _ = msg.channel_id.say(
                    &ctx.http,
                    "Usage: `welcome channel <#ch>` | `welcome message <template>` | `welcome enable` | `welcome disable`",
                ).await;
            }
        }
    }

    async fn handle_goodbye_cmd(
        &self,
        ctx: &Context,
        msg: &serenity::all::Message,
        guild_id: u64,
        subcmd: &str,
        arg: &str,
    ) {
        match subcmd {
            "channel" => {
                let channel_id = parse_channel_id(arg);
                match channel_id {
                    None => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Please mention a channel or provide a channel ID.",
                            )
                            .await;
                    }
                    Some(id) => {
                        let _ = sqlx::query(
                            "INSERT INTO goodbye_config (guild_id, channel_id) VALUES (?, ?) \
                             ON CONFLICT(guild_id) DO UPDATE SET channel_id = excluded.channel_id",
                        )
                        .bind(guild_id as i64)
                        .bind(id as i64)
                        .execute(self.state.servers_db())
                        .await;
                        let mut entry =
                            self.state.goodbye_cache.entry(guild_id).or_insert_with(|| {
                                GoodbyeConfig {
                                    channel_id: None,
                                    message: "Goodbye {user.name}!".to_string(),
                                    embed_json: None,
                                    enabled: false,
                                }
                            });
                        entry.channel_id = Some(id as i64);
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, format!("Goodbye channel set to <#{id}>."))
                            .await;
                    }
                }
            }
            "message" => {
                if arg.is_empty() {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Please provide a message template.")
                        .await;
                    return;
                }
                let _ = sqlx::query(
                    "INSERT INTO goodbye_config (guild_id, message) VALUES (?, ?) \
                     ON CONFLICT(guild_id) DO UPDATE SET message = excluded.message",
                )
                .bind(guild_id as i64)
                .bind(arg)
                .execute(self.state.servers_db())
                .await;
                let mut entry =
                    self.state.goodbye_cache.entry(guild_id).or_insert_with(|| GoodbyeConfig {
                        channel_id: None,
                        message: arg.to_string(),
                        embed_json: None,
                        enabled: false,
                    });
                entry.message = arg.to_string();
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Goodbye message updated.")
                    .await;
            }
            "enable" => {
                let _ = sqlx::query(
                    "INSERT INTO goodbye_config (guild_id, enabled) VALUES (?, 1) \
                     ON CONFLICT(guild_id) DO UPDATE SET enabled = 1",
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                if let Some(mut e) = self.state.goodbye_cache.get_mut(&guild_id) {
                    e.enabled = true;
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Goodbye messages enabled.")
                    .await;
            }
            "disable" => {
                let _ = sqlx::query(
                    "INSERT INTO goodbye_config (guild_id, enabled) VALUES (?, 0) \
                     ON CONFLICT(guild_id) DO UPDATE SET enabled = 0",
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                if let Some(mut e) = self.state.goodbye_cache.get_mut(&guild_id) {
                    e.enabled = false;
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Goodbye messages disabled.")
                    .await;
            }
            _ => {
                let _ = msg.channel_id.say(
                    &ctx.http,
                    "Usage: `goodbye channel <#ch>` | `goodbye message <template>` | `goodbye enable` | `goodbye disable`",
                ).await;
            }
        }
    }
}

fn parse_channel_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with("<#") && s.ends_with('>') {
        s[2..s.len() - 1].parse().ok()
    } else {
        s.parse().ok()
    }
}
