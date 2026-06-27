use super::Cog;
use crate::state::{AppState, Tag};
use crate::tagscript::{self, TagContext, TagOutput};
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use dashmap::DashMap;
use serenity::all::{
    ChannelId, Colour, Context, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateMessage,
    GuildId, Message, Permissions, ReactionType, RoleId, Timestamp,
};
use serenity::prelude::Mentionable;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Names that may not be used as tags because they collide with the tag
/// subsystem's own commands.
const RESERVED_NAMES: &[&str] = &["tag", "tagtest", "tt", "playground", "testtag"];

pub struct TagsCog {
    state: Arc<AppState>,
    /// Per-`{cd}` cooldown tracker, keyed by `"{guild}:{tag}:{bucket}"`.
    cooldowns: DashMap<String, Instant>,
}

impl TagsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            cooldowns: DashMap::new(),
        })
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

        // Tag management group.
        if first_word == "tag" {
            self.handle_tag_command(ctx, msg, guild_id, rest).await;
            return;
        }

        // Playground / test command — runs TagScript without saving.
        if matches!(first_word, "tagtest" | "tt" | "playground" | "testtag") {
            self.cmd_test(ctx, msg, rest).await;
            return;
        }

        // Otherwise treat an unmatched first word as a possible tag invocation.
        let exists = self
            .state
            .tag_cache
            .get(&guild_id)
            .map(|gt| gt.contains_key(first_word))
            .unwrap_or(false);
        if exists {
            self.invoke_tag(ctx, msg, guild_id, first_word, rest).await;
        }
    }
}

impl TagsCog {
    /// Build a fully-populated TagContext for an invocation. Synchronous so the
    /// cache guard is dropped before any `.await`.
    fn build_tag_context(&self, ctx: &Context, msg: &Message, args: &str, uses: i64) -> TagContext {
        let author = &msg.author;
        let target = msg.mentions.first().unwrap_or(author);

        let mut server_name = String::new();
        let mut server_member_count = String::new();
        let mut server_icon = String::new();
        let mut channel_name = String::new();
        if let Some(guild) = msg.guild(&ctx.cache) {
            server_name = guild.name.clone();
            server_member_count = guild.member_count.to_string();
            server_icon = guild.icon_url().unwrap_or_default();
            if let Some(ch) = guild.channels.get(&msg.channel_id) {
                channel_name = ch.name.clone();
            }
        }

        TagContext {
            user_name: author.name.clone(),
            user_mention: author.mention().to_string(),
            user_id: author.id.get().to_string(),
            user_avatar: author.avatar_url().unwrap_or_default(),
            user_discriminator: author
                .discriminator
                .map(|d| d.to_string())
                .unwrap_or_default(),
            target_name: target.name.clone(),
            target_mention: target.mention().to_string(),
            target_id: target.id.get().to_string(),
            target_avatar: target.avatar_url().unwrap_or_default(),
            target_discriminator: target
                .discriminator
                .map(|d| d.to_string())
                .unwrap_or_default(),
            channel_name,
            channel_id: msg.channel_id.get().to_string(),
            channel_mention: format!("<#{}>", msg.channel_id.get()),
            server_name,
            server_id: msg
                .guild_id
                .map(|g| g.get().to_string())
                .unwrap_or_default(),
            server_member_count,
            server_icon,
            args: args.to_string(),
            uses: uses.to_string(),
            ..Default::default()
        }
    }

    /// Whether the invoking author may edit/delete tags: bot owner, guild owner,
    /// or holder of the Manage Server (Administrator implies it) permission.
    fn member_can_manage(&self, ctx: &Context, msg: &Message, guild_id: u64) -> bool {
        let user_id = msg.author.id.get();
        if self.state.is_owner(user_id) {
            return true;
        }
        let Some(guild) = ctx.cache.guild(GuildId::new(guild_id)) else {
            return false;
        };
        if guild.owner_id.get() == user_id {
            return true;
        }
        // Prefer the roles supplied with the gateway message, fall back to cache.
        let roles: Vec<RoleId> = if let Some(pm) = &msg.member {
            pm.roles.clone()
        } else if let Some(member) = guild.members.get(&msg.author.id) {
            member.roles.clone()
        } else {
            return false;
        };

        let mut perms = Permissions::empty();
        if let Some(everyone) = guild.roles.get(&RoleId::new(guild_id)) {
            perms |= everyone.permissions;
        }
        for rid in &roles {
            if let Some(role) = guild.roles.get(rid) {
                perms |= role.permissions;
            }
        }
        perms.administrator() || perms.manage_guild()
    }

    /// Send a tag's rendered output: text and/or embed to the redirect channel
    /// (or invoking channel), then apply reactions and deletion side effects.
    async fn send_output_message(&self, ctx: &Context, msg: &Message, output: &TagOutput) {
        let has_content = !output.content.trim().is_empty();
        let has_embed = output.embed.is_some();
        if has_content || has_embed {
            let mut create = CreateMessage::new();
            if has_content {
                create = create.content(output.content.clone());
            }
            if let Some(ref embed_json) = output.embed {
                create = create.embed(json_to_embed(embed_json));
            }
            let dest = output
                .redirect_channel
                .map(ChannelId::new)
                .unwrap_or(msg.channel_id);
            let _ = dest.send_message(&ctx.http, create).await;
        }

        for emoji in &output.react_emojis {
            let _ = msg
                .react(&ctx.http, ReactionType::Unicode(emoji.clone()))
                .await;
        }

        if output.delete_invoke {
            let _ = msg.delete(&ctx.http).await;
        }
    }

    /// Run a stored tag and apply all of its output side effects.
    async fn invoke_tag(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        name: &str,
        args: &str,
    ) {
        let Some((content, uses)) = self
            .state
            .tag_cache
            .get(&guild_id)
            .and_then(|gt| gt.get(name).map(|t| (t.content.clone(), t.uses)))
        else {
            return;
        };

        let mut tag_ctx = self.build_tag_context(ctx, msg, args, uses);
        let output = tagscript::run(&content, &mut tag_ctx);

        // {stop:...} — send the stop message as-is and bail.
        if output.stopped {
            if !output.content.trim().is_empty() {
                let _ = msg.channel_id.say(&ctx.http, &output.content).await;
            }
            return;
        }

        // {cd(secs):bucket} — enforce a per-(guild, tag, bucket) cooldown.
        if let Some((bucket, secs)) = &output.cooldown {
            let key = format!("{guild_id}:{name}:{bucket}");
            let cd = Duration::from_secs(*secs);
            if let Some(last) = self.cooldowns.get(&key) {
                let elapsed = last.elapsed();
                if elapsed < cd {
                    let remaining = (cd - elapsed).as_secs().max(1);
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            format!("This tag is on cooldown. Try again in {remaining}s."),
                        )
                        .await;
                    return;
                }
            }
            self.cooldowns.insert(key, Instant::now());
        }

        self.send_output_message(ctx, msg, &output).await;

        // Increment the uses counter (DB + cache).
        let _ = sqlx::query("UPDATE tags_tags SET uses = uses + 1 WHERE guild_id = ? AND name = ?")
            .bind(guild_id as i64)
            .bind(name)
            .execute(self.state.servers_db())
            .await;
        if let Some(mut gt) = self.state.tag_cache.get_mut(&guild_id) {
            if let Some(t) = gt.get_mut(name) {
                t.uses += 1;
            }
        }
    }

    /// `tagtest`/`tt`/`playground`/`testtag` — render TagScript without saving.
    async fn cmd_test(&self, ctx: &Context, msg: &Message, args: &str) {
        if args.trim().is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: tagtest <tagscript>")
                .await;
            return;
        }
        // Mirror tags.py: the whole input is both the script and the {args} seed.
        let mut tag_ctx = self.build_tag_context(ctx, msg, args, 0);
        let output = tagscript::run(args, &mut tag_ctx);

        if output.stopped {
            if !output.content.trim().is_empty() {
                let _ = msg.channel_id.say(&ctx.http, &output.content).await;
            }
            return;
        }
        self.send_output_message(ctx, msg, &output).await;
    }

    async fn handle_tag_command(&self, ctx: &Context, msg: &Message, guild_id: u64, rest: &str) {
        let mut it = rest.splitn(2, ' ');
        let subcommand = it.next().unwrap_or("");
        let args = it.next().unwrap_or("").trim();

        match subcommand {
            "create" | "add" | "+" => self.cmd_create(ctx, msg, guild_id, args).await,
            "edit" => self.cmd_edit(ctx, msg, guild_id, args).await,
            "delete" | "del" | "remove" | "-" => self.cmd_delete(ctx, msg, guild_id, args).await,
            "list" => self.cmd_list(ctx, msg, guild_id).await,
            "info" => self.cmd_info(ctx, msg, guild_id, args).await,
            "raw" => self.cmd_raw(ctx, msg, guild_id, args).await,
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `tag create <name> <content>` | `tag edit <name> <content>` | `tag delete <name>` | `tag list` | `tag info <name>` | `tag raw <name>`\nTest without saving: `tagtest <tagscript>`",
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
        if RESERVED_NAMES.contains(&name.as_str()) {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    format!("`{name}` is reserved and can't be used as a tag name."),
                )
                .await;
            return;
        }
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
                let len = content.chars().count();
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
                let embed = embeds::success_embed(
                    "Success",
                    &format!("Created tag `{name}`, length `{len}`"),
                );
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }
            Ok(_) => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!("Tag `{name}` already exists. Use `tag edit {name} <content>` to edit it."),
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

        let owner_id = self
            .state
            .tag_cache
            .get(&guild_id)
            .and_then(|gt| gt.get(&name).map(|t| t.owner_id));

        let owner_id = match owner_id {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
                return;
            }
            Some(id) => id,
        };

        if owner_id != msg.author.id.get() as i64 && !self.member_can_manage(ctx, msg, guild_id) {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "You can only edit tags you own (or need the Manage Server permission).",
                )
                .await;
            return;
        }

        let len = content.chars().count();
        let _ = sqlx::query("UPDATE tags_tags SET content = ? WHERE guild_id = ? AND name = ?")
            .bind(&content)
            .bind(guild_id as i64)
            .bind(&name)
            .execute(self.state.servers_db())
            .await;
        if let Some(mut gt) = self.state.tag_cache.get_mut(&guild_id) {
            if let Some(t) = gt.get_mut(&name) {
                t.content = content;
            }
        }

        let embed = embeds::success_embed(
            "Success",
            &format!("Edited tag `{name}`, new length `{len}`"),
        );
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
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

        let owner_id = self
            .state
            .tag_cache
            .get(&guild_id)
            .and_then(|gt| gt.get(&name).map(|t| t.owner_id));

        let owner_id = match owner_id {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
                return;
            }
            Some(id) => id,
        };

        if owner_id != msg.author.id.get() as i64 && !self.member_can_manage(ctx, msg, guild_id) {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "You can only delete tags you own (or need the Manage Server permission).",
                )
                .await;
            return;
        }

        let _ = sqlx::query("DELETE FROM tags_tags WHERE guild_id = ? AND name = ?")
            .bind(guild_id as i64)
            .bind(&name)
            .execute(self.state.servers_db())
            .await;
        if let Some(mut gt) = self.state.tag_cache.get_mut(&guild_id) {
            gt.remove(&name);
        }

        let embed = CreateEmbed::new()
            .title("Success")
            .description(format!("Removed tag `{name}`"))
            .color(colors::RED)
            .timestamp(Timestamp::now());
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    async fn cmd_list(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let mut lines: Vec<String> = Vec::new();
        {
            if let Some(gt) = self.state.tag_cache.get(&guild_id) {
                let mut names: Vec<String> = gt.keys().cloned().collect();
                names.sort();
                for n in names {
                    if let Some(t) = gt.get(&n) {
                        lines.push(format!(
                            "{} - Uses: {} Length: {}",
                            t.name,
                            t.uses,
                            t.content.chars().count()
                        ));
                    }
                }
            }
        }

        if lines.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "No tags in this server.")
                .await;
            return;
        }

        let server_name = ctx
            .cache
            .guild(GuildId::new(guild_id))
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "Server".to_string());
        let vis = lines.join("\n");
        let embed = CreateEmbed::new()
            .title(format!("{server_name} Tags"))
            .description(format!("```yaml\n{vis}\n```"))
            .color(colors::PINK)
            .timestamp(Timestamp::now());
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
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

        let tag = self
            .state
            .tag_cache
            .get(&guild_id)
            .and_then(|gt| gt.get(&name).cloned());

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
                let embed = CreateEmbed::new()
                    .title(format!("Tag: {}", t.name))
                    .color(colors::ORANGE)
                    .field("Owner", format!("<@{}>", t.owner_id), true)
                    .field("Uses", t.uses.to_string(), true)
                    .field("Length", t.content.chars().count().to_string(), true)
                    .field("Created", created, true)
                    .timestamp(Timestamp::now());
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }
        }
    }

    async fn cmd_raw(&self, ctx: &Context, msg: &Message, guild_id: u64, name: &str) {
        let name = name.trim().to_lowercase();
        if name.is_empty() {
            let _ = msg.channel_id.say(&ctx.http, "Usage: tag raw <name>").await;
            return;
        }

        let content = self
            .state
            .tag_cache
            .get(&guild_id)
            .and_then(|gt| gt.get(&name).map(|t| t.content.clone()));

        match content {
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("Tag `{name}` not found."))
                    .await;
            }
            Some(c) => {
                let escaped = c.replace('\\', "\\\\").replace('`', "\\`");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("```\n{escaped}\n```"))
                    .await;
            }
        }
    }
}

/// Deserialize a TagScript embed JSON object into a serenity `CreateEmbed`.
fn json_to_embed(v: &serde_json::Value) -> CreateEmbed {
    let mut embed = CreateEmbed::new();

    if let Some(title) = v.get("title").and_then(|x| x.as_str()) {
        embed = embed.title(title);
    }
    if let Some(desc) = v.get("description").and_then(|x| x.as_str()) {
        embed = embed.description(desc);
    }
    if let Some(color) = v.get("color").and_then(|x| x.as_u64()) {
        embed = embed.color(Colour(color as u32));
    }
    if let Some(url) = v.get("url").and_then(|x| x.as_str()) {
        embed = embed.url(url);
    }
    if let Some(fields) = v.get("fields").and_then(|x| x.as_array()) {
        for f in fields {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("\u{200B}");
            let value = f
                .get("value")
                .and_then(|x| x.as_str())
                .unwrap_or("\u{200B}");
            let inline = f.get("inline").and_then(|x| x.as_bool()).unwrap_or(false);
            embed = embed.field(name, value, inline);
        }
    }
    if let Some(url) = v
        .get("thumbnail")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.thumbnail(url);
    }
    if let Some(url) = v
        .get("image")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.image(url);
    }
    if let Some(text) = v
        .get("footer")
        .and_then(|t| t.get("text"))
        .and_then(|x| x.as_str())
    {
        embed = embed.footer(CreateEmbedFooter::new(text));
    }
    if let Some(author) = v.get("author") {
        if let Some(name) = author.get("name").and_then(|x| x.as_str()) {
            let mut a = CreateEmbedAuthor::new(name);
            if let Some(icon) = author.get("icon_url").and_then(|x| x.as_str()) {
                a = a.icon_url(icon);
            }
            embed = embed.author(a);
        }
    }

    embed
}
