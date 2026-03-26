use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use dashmap::DashMap;
use serenity::all::{
    ChannelId, Context, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateMessage, Message,
    Timestamp,
};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
struct EmbedSession {
    title: Option<String>,
    description: Option<String>,
    color: Option<u32>,
    author: Option<String>,
    footer: Option<String>,
    fields: Vec<(String, String, bool)>,
}

pub struct EmbedCog {
    state: Arc<AppState>,
    sessions: DashMap<u64, EmbedSession>,
}

impl EmbedCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            sessions: DashMap::new(),
        })
    }
}

#[async_trait]
impl Cog for EmbedCog {
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
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "embed" {
            return;
        }
        let subcmd = it.next().unwrap_or("").trim();
        let arg = it.next().unwrap_or("").trim();

        let user_id = msg.author.id.get();

        match subcmd {
            "new" | "create" => {
                self.sessions.insert(user_id, EmbedSession::default());
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "New embed session started. Use:\n\
                        `embed title <text>` | `embed description <text>` | `embed color <hex>` |\n\
                        `embed author <text>` | `embed footer <text>` | `embed field <name> | <value>` |\n\
                        `embed preview` | `embed send <#channel>` | `embed clear`",
                    )
                    .await;
            }
            "title" => {
                if let Some(mut session) = self.sessions.get_mut(&user_id) {
                    session.title = Some(arg.to_string());
                    let _ = msg.channel_id.say(&ctx.http, "Title set.").await;
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "No active session. Use `embed new` first.")
                        .await;
                }
            }
            "description" | "desc" => {
                if let Some(mut session) = self.sessions.get_mut(&user_id) {
                    session.description = Some(arg.to_string());
                    let _ = msg.channel_id.say(&ctx.http, "Description set.").await;
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "No active session. Use `embed new` first.")
                        .await;
                }
            }
            "color" | "colour" => {
                let hex = arg.trim_start_matches('#');
                match u32::from_str_radix(hex, 16) {
                    Ok(color) => {
                        if let Some(mut session) = self.sessions.get_mut(&user_id) {
                            session.color = Some(color);
                            let _ = msg
                                .channel_id
                                .say(&ctx.http, format!("Color set to #{color:06X}."))
                                .await;
                        } else {
                            let _ = msg
                                .channel_id
                                .say(&ctx.http, "No active session. Use `embed new` first.")
                                .await;
                        }
                    }
                    Err(_) => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Invalid hex color. Example: `embed color ff5733`",
                            )
                            .await;
                    }
                }
            }
            "author" => {
                if let Some(mut session) = self.sessions.get_mut(&user_id) {
                    session.author = Some(arg.to_string());
                    let _ = msg.channel_id.say(&ctx.http, "Author set.").await;
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "No active session. Use `embed new` first.")
                        .await;
                }
            }
            "footer" => {
                if let Some(mut session) = self.sessions.get_mut(&user_id) {
                    session.footer = Some(arg.to_string());
                    let _ = msg.channel_id.say(&ctx.http, "Footer set.").await;
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "No active session. Use `embed new` first.")
                        .await;
                }
            }
            "field" => {
                // format: field <name> | <value>
                if let Some((name, value)) = arg.split_once('|') {
                    if let Some(mut session) = self.sessions.get_mut(&user_id) {
                        session
                            .fields
                            .push((name.trim().to_string(), value.trim().to_string(), false));
                        let _ = msg.channel_id.say(&ctx.http, "Field added.").await;
                    } else {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "No active session. Use `embed new` first.")
                            .await;
                    }
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: embed field <name> | <value>")
                        .await;
                }
            }
            "preview" => {
                if let Some(session) = self.sessions.get(&user_id) {
                    let embed = build_embed(&session);
                    let _ = msg
                        .channel_id
                        .send_message(&ctx.http, CreateMessage::new().embed(embed))
                        .await;
                } else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "No active session. Use `embed new` first.")
                        .await;
                }
            }
            "send" => {
                let channel_id = parse_channel_id(arg);
                match channel_id {
                    None => {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "Usage: embed send <#channel>")
                            .await;
                    }
                    Some(id) => {
                        if let Some(session) = self.sessions.get(&user_id) {
                            let embed = build_embed(&session);
                            drop(session);
                            let _ = ChannelId::new(id)
                                .send_message(&ctx.http, CreateMessage::new().embed(embed))
                                .await;
                            self.sessions.remove(&user_id);
                            let _ = msg.channel_id.say(&ctx.http, "Embed sent!").await;
                        } else {
                            let _ = msg
                                .channel_id
                                .say(&ctx.http, "No active session.")
                                .await;
                        }
                    }
                }
            }
            "clear" => {
                self.sessions.remove(&user_id);
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Embed session cleared.")
                    .await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `embed new` | `embed title/description/color/author/footer <text>` | \
                        `embed field <name> | <value>` | `embed preview` | `embed send <#ch>` | `embed clear`",
                    )
                    .await;
            }
        }
    }
}

fn build_embed(session: &EmbedSession) -> CreateEmbed {
    let mut embed = CreateEmbed::new();
    if let Some(ref title) = session.title {
        embed = embed.title(title);
    }
    if let Some(ref desc) = session.description {
        embed = embed.description(desc);
    }
    if let Some(color) = session.color {
        embed = embed.color(serenity::all::Colour(color));
    }
    if let Some(ref author) = session.author {
        embed = embed.author(CreateEmbedAuthor::new(author));
    }
    if let Some(ref footer) = session.footer {
        embed = embed.footer(CreateEmbedFooter::new(footer));
    }
    for (name, value, inline) in &session.fields {
        embed = embed.field(name, value, *inline);
    }
    embed.timestamp(Timestamp::now())
}

fn parse_channel_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with("<#") && s.ends_with('>') {
        s[2..s.len() - 1].parse().ok()
    } else {
        s.parse().ok()
    }
}
