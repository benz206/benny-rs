use super::Cog;
use crate::state::{AppState, Tag};
use crate::tagscript::{self, TagContext};
use async_trait::async_trait;
use serenity::all::{Context, Message};
use serenity::prelude::Mentionable;
use std::collections::HashMap;
use std::sync::Arc;

pub struct TagsCog {
    state: Arc<AppState>,
}

impl TagsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for TagsCog {
    async fn on_ready(&self, _ctx: &Context) {
        // Load all tags from DB into tag_cache
        let rows: Vec<(i64, String, String, i64, i64, i64)> = sqlx::query_as(
            "SELECT guild_id, name, content, owner_id, uses, created_at FROM tags_tags",
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        let mut count = 0usize;
        for (guild_id, name, content, owner_id, uses, created_at) in rows {
            let mut guild_tags = self
                .state
                .tag_cache
                .entry(guild_id as u64)
                .or_insert_with(HashMap::new);
            guild_tags.insert(
                name.clone(),
                Tag {
                    name,
                    content,
                    owner_id,
                    uses,
                    created_at,
                },
            );
            count += 1;
        }
        tracing::info!("Tag cache loaded ({count} tags)");
    }

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
        let mut it = body.splitn(2, ' ');
        let Some(first_word) = it.next() else {
            return;
        };
        let rest = it.next().unwrap_or("").trim();

        // Handle tag management commands
        if first_word == "tag" {
            self.handle_tag_command(ctx, msg, guild_id, rest).await;
            return;
        }

        // Try to invoke a tag by name (shortcut: >tagname [args])
        let tag_content = {
            let guild_tags = self.state.tag_cache.get(&guild_id);
            guild_tags.and_then(|gt| gt.get(first_word).map(|t| t.content.clone()))
        };

        if let Some(tag_content) = tag_content {
            let mut tag_ctx = self.build_tag_context(msg, rest);
            let output = tagscript::run(&tag_content, &mut tag_ctx);

            // Increment uses
            let _ = sqlx::query(
                "UPDATE tags_tags SET uses = uses + 1 WHERE guild_id = ? AND name = ?",
            )
            .bind(guild_id as i64)
            .bind(first_word)
            .execute(self.state.servers_db())
            .await;

            if !output.content.is_empty() || output.react_emojis.is_empty() {
                let send_content = if output.content.is_empty() {
                    "\u{200B}".to_string() // zero-width space to avoid empty message error
                } else {
                    output.content
                };
                let _ = msg.channel_id.say(&ctx.http, send_content).await;
            }

            // Handle react side-effects
            for emoji_str in &output.react_emojis {
                use serenity::all::ReactionType;
                let reaction = ReactionType::Unicode(emoji_str.to_string());
                let _ = msg.react(&ctx.http, reaction).await;
            }

            if output.delete_invoke {
                let _ = msg.delete(&ctx.http).await;
            }
        }
    }
}

impl TagsCog {
    fn build_tag_context(&self, msg: &Message, args: &str) -> TagContext {
        TagContext {
            user_name: msg.author.name.clone(),
            user_mention: msg.author.mention().to_string(),
            user_id: msg.author.id.get().to_string(),
            user_avatar: msg.author.avatar_url().unwrap_or_default(),
            server_name: String::new(), // Would need guild from cache
            server_id: msg
                .guild_id
                .map(|g| g.get().to_string())
                .unwrap_or_default(),
            server_member_count: String::new(),
            channel_name: String::new(),
            channel_id: msg.channel_id.get().to_string(),
            args: args.to_string(),
            vars: HashMap::new(),
            ..Default::default()
        }
    }

    async fn handle_tag_command(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        rest: &str,
    ) {
        let mut it = rest.splitn(2, ' ');
        let subcommand = it.next().unwrap_or("");
        let args = it.next().unwrap_or("").trim();

        match subcommand {
            "create" | "add" => self.cmd_create(ctx, msg, guild_id, args).await,
            "edit" => self.cmd_edit(ctx, msg, guild_id, args).await,
            "delete" | "del" | "remove" => self.cmd_delete(ctx, msg, guild_id, args).await,
            "list" => self.cmd_list(ctx, msg, guild_id).await,
            "info" => self.cmd_info(ctx, msg, guild_id, args).await,
            "raw" => self.cmd_raw(ctx, msg, guild_id, args).await,
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `tag create <name> <content>` | `tag edit <name> <content>` | `tag delete <name>` | `tag list` | `tag info <name>` | `tag raw <name>`",
                    )
                    .await;
            }
        }
    }

    async fn cmd_create(&self, ctx: &Context, msg: &Message, guild_id: u64, args: &str) {
        let mut parts = args.splitn(2, ' ');
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n.to_lowercase(),
            _ => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Usage: tag create <name> <content>")
                    .await;
                return;
            }
        };
        let content = match parts.next() {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Please provide tag content.")
                    .await;
                return;
            }
        };

        let owner_id = msg.author.id.get() as i64;
        let created_at = chrono::Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT OR IGNORE INTO tags_tags (guild_id, name, content, owner_id, uses, created_at) VALUES (?, ?, ?, ?, 0, ?)",
        )
        .bind(guild_id as i64)
        .bind(&name)
        .bind(&content)
        .bind(owner_id)
        .bind(created_at)
        .execute(self.state.servers_db())
        .await;

        match result {
            Ok(r) if r.rows_affected() > 0 => {
                // Update cache
                self.state
                    .tag_cache
                    .entry(guild_id)
                    .or_insert_with(HashMap::new)
                    .insert(
                        name.clone(),
                        Tag {
                            name: name.clone(),
                            content,
                            owner_id,
                            uses: 0,
                            created_at,
                        },
                    );
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("✅ Tag `{name}` created."))
                    .await;
            }
            Ok(_) => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "Tag `{name}` already exists. Use `tag edit {name}` to edit it."
                        ),
                    )
                    .await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to create tag");
                let _ = msg.channel_id.say(&ctx.http, "Database error.").await;
            }
        }
    }

    async fn cmd_edit(&self, ctx: &Context, msg: &Message, guild_id: u64, args: &str) {
        let mut parts = args.splitn(2, ' ');
        let name = match parts.next() {
            Some(n) if !n.is_empty() => n.to_lowercase(),
            _ => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Usage: tag edit <name> <new content>")
                    .await;
                return;
            }
        };
        let content = match parts.next() {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Please provide new content.")
                    .await;
                return;
            }
        };

        let user_id = msg.author.id.get() as i64;

        // Check ownership (or Manage Guild permission — simplified for now)
        let owner: Option<(i64,)> = sqlx::query_as(
            "SELECT owner_id FROM tags_tags WHERE guild_id = ? AND name = ?",
        )
        .bind(guild_id as i64)
        .bind(&name)
        .fetch_optional(self.state.servers_db())
        .await
        .ok()
        .flatten();

        match owner {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
            }
            Some((owner_id,))
                if owner_id != user_id && !self.state.is_owner(msg.author.id.get()) =>
            {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "You can only edit tags you own.")
                    .await;
            }
            _ => {
                let _ = sqlx::query(
                    "UPDATE tags_tags SET content = ? WHERE guild_id = ? AND name = ?",
                )
                .bind(&content)
                .bind(guild_id as i64)
                .bind(&name)
                .execute(self.state.servers_db())
                .await;

                // Update cache
                if let Some(mut guild_tags) = self.state.tag_cache.get_mut(&guild_id) {
                    if let Some(tag) = guild_tags.get_mut(&name) {
                        tag.content = content;
                    }
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("✅ Tag `{name}` updated."))
                    .await;
            }
        }
    }

    async fn cmd_delete(&self, ctx: &Context, msg: &Message, guild_id: u64, name: &str) {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: tag delete <name>")
                .await;
            return;
        }

        let user_id = msg.author.id.get() as i64;
        let owner: Option<(i64,)> = sqlx::query_as(
            "SELECT owner_id FROM tags_tags WHERE guild_id = ? AND name = ?",
        )
        .bind(guild_id as i64)
        .bind(&name)
        .fetch_optional(self.state.servers_db())
        .await
        .ok()
        .flatten();

        match owner {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
            }
            Some((owner_id,))
                if owner_id != user_id && !self.state.is_owner(msg.author.id.get()) =>
            {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "You can only delete tags you own.")
                    .await;
            }
            _ => {
                let _ =
                    sqlx::query("DELETE FROM tags_tags WHERE guild_id = ? AND name = ?")
                        .bind(guild_id as i64)
                        .bind(&name)
                        .execute(self.state.servers_db())
                        .await;

                if let Some(mut guild_tags) = self.state.tag_cache.get_mut(&guild_id) {
                    guild_tags.remove(&name);
                }
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("✅ Tag `{name}` deleted."))
                    .await;
            }
        }
    }

    async fn cmd_list(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let tag_names: Vec<String> =
            if let Some(guild_tags) = self.state.tag_cache.get(&guild_id) {
                let mut names: Vec<String> = guild_tags.keys().cloned().collect();
                names.sort();
                names
            } else {
                vec![]
            };

        if tag_names.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "No tags in this server.")
                .await;
        } else {
            let list = tag_names
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    format!("**Tags ({}):** {list}", tag_names.len()),
                )
                .await;
        }
    }

    async fn cmd_info(&self, ctx: &Context, msg: &Message, guild_id: u64, name: &str) {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: tag info <name>")
                .await;
            return;
        }

        let tag = if let Some(guild_tags) = self.state.tag_cache.get(&guild_id) {
            guild_tags.get(&name).cloned()
        } else {
            None
        };

        match tag {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
            }
            Some(t) => {
                let created = chrono::DateTime::from_timestamp(t.created_at, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "**Tag `{}`**\nOwner: <@{}>\nUses: {}\nCreated: {}",
                            t.name, t.owner_id, t.uses, created
                        ),
                    )
                    .await;
            }
        }
    }

    async fn cmd_raw(&self, ctx: &Context, msg: &Message, guild_id: u64, name: &str) {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: tag raw <name>")
                .await;
            return;
        }

        let content = if let Some(guild_tags) = self.state.tag_cache.get(&guild_id) {
            guild_tags.get(&name).map(|t| t.content.clone())
        } else {
            None
        };

        match content {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
            }
            Some(c) => {
                let escaped = c.replace('`', "\\`");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("```\n{escaped}\n```"))
                    .await;
            }
        }
    }
}
