use super::Cog;
use crate::entities::tags;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::{AppState, Tag};
use crate::tagscript::{self, TagContext, TagOutput};
use crate::utils::ratelimit::RateLimiter;
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use dashmap::DashMap;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, Set};
use serenity::all::{
    ChannelId, Colour, CreateAllowedMentions, CreateEmbed, CreateEmbedAuthor,
    CreateEmbedFooter, CreateMessage, GuildId, Message, Permissions, ReactionType, RoleId,
    Timestamp, UserId,
};
use serenity::prelude::Mentionable;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Names that may not be used as tags because they collide with the tag
/// subsystem's own commands.
const RESERVED_NAMES: &[&str] = &["tag", "tagtest", "tt", "playground", "testtag"];
const MAX_TAG_NAME_LEN: usize = 32;
const MAX_TAG_CONTENT_LEN: usize = 2000;
/// Cap on the per-`{cd}` cooldown tracker so it can't grow without bound.
const MAX_COOLDOWN_KEYS: usize = 4096;
/// Light per-user throttle on dynamic tag invocation, independent of the
/// author opt-in `{cd}` block.
const INVOKE_INTERVAL: Duration = Duration::from_millis(1500);

pub struct TagsCog {
    state: Arc<AppState>,
    /// Per-`{cd}` cooldown tracker, keyed by `"{guild}:{tag}:{bucket}"`.
    cooldowns: DashMap<String, Instant>,
    /// Per-`(guild, user)` throttle on tag invocation.
    invoke_limiter: RateLimiter<(u64, u64)>,
}

impl TagsCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            cooldowns: DashMap::new(),
            invoke_limiter: RateLimiter::new(8192),
        })
    }
}

#[async_trait]
impl Cog for TagsCog {
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        // Load all tags from DB into tag_cache
        let rows = tags::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        let mut count = 0usize;
        for m in rows {
            let mut guild_tags = self
                .state
                .tag_cache
                .entry(m.guild_id as u64)
                .or_insert_with(HashMap::new);
            guild_tags.insert(
                m.name.clone(),
                Tag {
                    name: m.name,
                    content: m.content,
                    owner_id: m.owner_id,
                    uses: m.uses,
                    created_at: m.created_at,
                },
            );
            count += 1;
        }
        tracing::info!("Tag cache loaded ({count} tags)");
    }

    async fn on_message(&self, ctx: &serenity::all::Context, msg: &Message) {
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

        // Treat an unmatched first word as a possible tag invocation.
        // Names are stored lower-cased, so match case-insensitively (otherwise
        // `Hello` could never invoke the stored `hello`).
        let tag_name = first_word.to_lowercase();
        let exists = self
            .state
            .tag_cache
            .get(&guild_id)
            .map(|gt| gt.contains_key(&tag_name))
            .unwrap_or(false);
        if exists {
            // Light per-user throttle so a stored tag can't be spam-invoked
            // (separate from the author opt-in `{cd}` block).
            if self
                .invoke_limiter
                .check((guild_id, msg.author.id.get()), INVOKE_INTERVAL)
                .is_some()
            {
                return;
            }
            self.invoke_tag(ctx, msg, guild_id, &tag_name, rest).await;
        }
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![tag(), tagtest()]
}

// ---- poise commands --------------------------------------------------------

/// Tag management commands.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Tags",
    subcommand_required,
    subcommands("tag_create", "tag_edit", "tag_delete", "tag_list", "tag_info", "tag_raw"),
)]
async fn tag(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Create a new tag.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "create",
    aliases("add"),
    category = "Tags",
)]
async fn tag_create(
    ctx: Context<'_>,
    #[description = "Name"] name: String,
    #[description = "Content"]
    #[rest]
    content: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let name = name.to_lowercase();

    if RESERVED_NAMES.contains(&name.as_str()) {
        return send_error(
            ctx,
            &format!("`{name}` is reserved and can't be used as a tag name."),
        )
        .await;
    }
    if name.chars().count() > MAX_TAG_NAME_LEN
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return send_error(
            ctx,
            &format!(
                "Tag names must be ≤{MAX_TAG_NAME_LEN} characters and contain only letters, numbers, `_` or `-`."
            ),
        )
        .await;
    }

    let content = content.trim().to_string();
    if content.is_empty() {
        return send_error(ctx, "Please provide tag content.").await;
    }
    if content.chars().count() > MAX_TAG_CONTENT_LEN {
        return send_error(
            ctx,
            &format!("Tag content must be ≤{MAX_TAG_CONTENT_LEN} characters."),
        )
        .await;
    }

    let owner_id = ctx.author().id.get() as i64;
    let created_at = chrono::Utc::now().timestamp();

    let result = tags::Entity::insert(tags::ActiveModel {
        guild_id: Set(guild_id as i64),
        name: Set(name.clone()),
        content: Set(content.clone()),
        owner_id: Set(owner_id),
        uses: Set(0),
        created_at: Set(created_at),
    })
    .on_conflict(
        OnConflict::columns([tags::Column::GuildId, tags::Column::Name])
            .do_nothing()
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;

    match result {
        Ok(_) => {
            let len = content.chars().count();
            state
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
            send_embed(ctx, embed).await
        }
        Err(DbErr::RecordNotInserted) => {
            send_error(
                ctx,
                &format!(
                    "Tag `{name}` already exists. Use `tag edit {name} <content>` to edit it."
                ),
            )
            .await
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to create tag");
            send_error(ctx, "Database error.").await
        }
    }
}

/// Edit an existing tag's content.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "edit",
    category = "Tags",
)]
async fn tag_edit(
    ctx: Context<'_>,
    #[description = "Name"] name: String,
    #[description = "Content"]
    #[rest]
    content: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let name = name.to_lowercase();
    let content = content.trim().to_string();

    if content.is_empty() {
        return send_error(ctx, "Please provide new content.").await;
    }
    if content.chars().count() > MAX_TAG_CONTENT_LEN {
        return send_error(
            ctx,
            &format!("Tag content must be ≤{MAX_TAG_CONTENT_LEN} characters."),
        )
        .await;
    }

    let owner_id = state
        .tag_cache
        .get(&guild_id)
        .and_then(|gt| gt.get(&name).map(|t| t.owner_id));

    let owner_id = match owner_id {
        None => return send_error(ctx, &format!("Tag `{name}` not found.")).await,
        Some(id) => id,
    };

    if owner_id != ctx.author().id.get() as i64
        && !member_can_manage(sctx, state, guild_id, ctx.author().id.get()).await
    {
        return send_error(
            ctx,
            "You can only edit tags you own (or need the Manage Server permission).",
        )
        .await;
    }

    let len = content.chars().count();
    let _ = tags::Entity::update_many()
        .col_expr(tags::Column::Content, Expr::value(content.clone()))
        .filter(tags::Column::GuildId.eq(guild_id as i64))
        .filter(tags::Column::Name.eq(name.as_str()))
        .exec(state.servers_orm())
        .await;
    if let Some(mut gt) = state.tag_cache.get_mut(&guild_id)
        && let Some(t) = gt.get_mut(&name) {
            t.content = content;
        }

    let embed = embeds::success_embed(
        "Success",
        &format!("Edited tag `{name}`, new length `{len}`"),
    );
    send_embed(ctx, embed).await
}

/// Delete a tag.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "delete",
    aliases("del", "remove"),
    category = "Tags",
)]
async fn tag_delete(
    ctx: Context<'_>,
    #[description = "Name"] name: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;
    let name = name.trim().to_lowercase();

    let owner_id = state
        .tag_cache
        .get(&guild_id)
        .and_then(|gt| gt.get(&name).map(|t| t.owner_id));

    let owner_id = match owner_id {
        None => return send_error(ctx, &format!("Tag `{name}` not found.")).await,
        Some(id) => id,
    };

    if owner_id != ctx.author().id.get() as i64
        && !member_can_manage(sctx, state, guild_id, ctx.author().id.get()).await
    {
        return send_error(
            ctx,
            "You can only delete tags you own (or need the Manage Server permission).",
        )
        .await;
    }

    let _ = tags::Entity::delete_many()
        .filter(tags::Column::GuildId.eq(guild_id as i64))
        .filter(tags::Column::Name.eq(name.as_str()))
        .exec(state.servers_orm())
        .await;
    if let Some(mut gt) = state.tag_cache.get_mut(&guild_id) {
        gt.remove(&name);
    }

    let embed = CreateEmbed::new()
        .title("Success")
        .description(format!("Removed tag `{name}`"))
        .color(colors::RED)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// List all tags in this server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "list",
    category = "Tags",
)]
async fn tag_list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    let mut lines: Vec<String> = Vec::new();
    if let Some(gt) = state.tag_cache.get(&guild_id) {
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

    if lines.is_empty() {
        return send_error(ctx, "No tags in this server.").await;
    }

    let server_name = sctx
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
    send_embed(ctx, embed).await
}

/// Show info about a tag.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "info",
    category = "Tags",
)]
async fn tag_info(
    ctx: Context<'_>,
    #[description = "Name"] name: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let name = name.trim().to_lowercase();

    let tag = state
        .tag_cache
        .get(&guild_id)
        .and_then(|gt| gt.get(&name).cloned());

    match tag {
        None => send_error(ctx, &format!("Tag `{name}` not found.")).await,
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
            send_embed(ctx, embed).await
        }
    }
}

/// Show the raw (unrendered) content of a tag.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "raw",
    category = "Tags",
)]
async fn tag_raw(
    ctx: Context<'_>,
    #[description = "Name"] name: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap().get();
    let state = &ctx.data().state;
    let name = name.trim().to_lowercase();

    let content = state
        .tag_cache
        .get(&guild_id)
        .and_then(|gt| gt.get(&name).map(|t| t.content.clone()));

    match content {
        None => send_error(ctx, &format!("Tag `{name}` not found.")).await,
        Some(c) => {
            let escaped = c.replace('\\', "\\\\").replace('`', "\\`");
            ctx.say(format!("```\n{escaped}\n```")).await?;
            Ok(())
        }
    }
}

/// Evaluate a TagScript snippet without saving it.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    aliases("tt", "playground", "testtag"),
    category = "Tags",
)]
async fn tagtest(
    ctx: Context<'_>,
    #[description = "TagScript"]
    #[rest]
    script: String,
) -> Result<(), Error> {
    let script = script.trim();
    if script.is_empty() {
        return send_error(ctx, "Usage: tagtest <tagscript>").await;
    }

    let sctx = ctx.serenity_context();
    let author = ctx.author();
    let guild_id = ctx.guild_id().unwrap();
    let channel_id = ctx.channel_id();

    let mut server_name = String::new();
    let mut server_member_count = String::new();
    let mut server_icon = String::new();
    let mut channel_name = String::new();
    if let Some(guild) = sctx.cache.guild(guild_id) {
        server_name = guild.name.clone();
        server_member_count = guild.member_count.to_string();
        server_icon = guild.icon_url().unwrap_or_default();
        if let Some(ch) = guild.channels.get(&channel_id) {
            channel_name = ch.name.clone();
        }
    }

    let mut tag_ctx = TagContext {
        user_name: author.name.clone(),
        user_mention: author.mention().to_string(),
        user_id: author.id.get().to_string(),
        user_avatar: author.avatar_url().unwrap_or_default(),
        user_discriminator: author
            .discriminator
            .map(|d| d.to_string())
            .unwrap_or_default(),
        target_name: author.name.clone(),
        target_mention: author.mention().to_string(),
        target_id: author.id.get().to_string(),
        target_avatar: author.avatar_url().unwrap_or_default(),
        target_discriminator: author
            .discriminator
            .map(|d| d.to_string())
            .unwrap_or_default(),
        channel_name,
        channel_id: channel_id.get().to_string(),
        channel_mention: format!("<#{}>", channel_id.get()),
        server_name,
        server_id: guild_id.get().to_string(),
        server_member_count,
        server_icon,
        args: script.to_string(),
        uses: "0".to_string(),
        ..Default::default()
    };

    let output = tagscript::run(script, &mut tag_ctx);

    if output.stopped {
        if !output.content.trim().is_empty() {
            ctx.say(output.content).await?;
        }
        return Ok(());
    }

    let has_content = !output.content.trim().is_empty();
    let has_embed = output.embed.is_some();
    if has_content || has_embed {
        let mut create = CreateMessage::new().allowed_mentions(CreateAllowedMentions::new());
        if has_content {
            let content: String = output.content.chars().take(2000).collect();
            create = create.content(content);
        }
        if let Some(ref embed_json) = output.embed {
            create = create.embed(json_to_embed(embed_json));
        }
        let _ = channel_id.send_message(&sctx.http, create).await;
    }

    Ok(())
}

// ---- TagsCog helpers (on_message path only) --------------------------------

impl TagsCog {
    /// Build a fully-populated TagContext for an invocation. Synchronous so the
    /// cache guard is dropped before any `.await`.
    fn build_tag_context(
        &self,
        ctx: &serenity::all::Context,
        msg: &Message,
        args: &str,
        uses: i64,
    ) -> TagContext {
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

    /// Send a tag's rendered output: text and/or embed to the redirect channel
    /// (or invoking channel), then apply reactions and deletion side effects.
    async fn send_output_message(
        &self,
        ctx: &serenity::all::Context,
        msg: &Message,
        output: &TagOutput,
    ) {
        let has_content = !output.content.trim().is_empty();
        let has_embed = output.embed.is_some();
        if has_content || has_embed {
            // Tag content is member-authored, so never let it ping @everyone,
            // @here, roles, or arbitrary users.
            let mut create = CreateMessage::new().allowed_mentions(CreateAllowedMentions::new());
            if has_content {
                // Cap at Discord's 2000-char message limit; an oversized render
                // would otherwise fail to send and the tag would do nothing.
                let content: String = output.content.chars().take(2000).collect();
                create = create.content(content);
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
        ctx: &serenity::all::Context,
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
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new()
                            .content(output.content.clone())
                            .allowed_mentions(CreateAllowedMentions::new()),
                    )
                    .await;
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
            crate::utils::cache::bounded_insert(
                &self.cooldowns,
                key,
                Instant::now(),
                MAX_COOLDOWN_KEYS,
            );
        }

        self.send_output_message(ctx, msg, &output).await;

        // Increment the uses counter (DB + cache). `ExprTrait` (for `.add`) is
        // scoped to this block so its blanket impl doesn't shadow inherent
        // methods like `u64::max` elsewhere in the function.
        let inc_uses = {
            use sea_orm::sea_query::ExprTrait;
            Expr::col(tags::Column::Uses).add(1)
        };
        let _ = tags::Entity::update_many()
            .col_expr(tags::Column::Uses, inc_uses)
            .filter(tags::Column::GuildId.eq(guild_id as i64))
            .filter(tags::Column::Name.eq(name))
            .exec(self.state.servers_orm())
            .await;
        if let Some(mut gt) = self.state.tag_cache.get_mut(&guild_id)
            && let Some(t) = gt.get_mut(name) {
                t.uses += 1;
            }
    }
}

// ---- free helpers ----------------------------------------------------------

/// Whether `user_id` may edit/delete tags: bot owner, guild owner, or holder
/// of the Manage Server (Administrator implies it) permission. Checks member
/// roles from cache first, falls back to HTTP if not cached.
async fn member_can_manage(
    sctx: &serenity::all::Context,
    state: &AppState,
    guild_id: u64,
    user_id: u64,
) -> bool {
    if state.is_owner(user_id) {
        return true;
    }
    let gid = GuildId::new(guild_id);
    let uid = UserId::new(user_id);

    // Compute what we can from the cache in one guard scope, then drop it
    // before any potential HTTP call.
    let (is_owner, cached_result) = match sctx.cache.guild(gid) {
        None => return false,
        Some(g) => {
            let is_owner = g.owner_id.get() == user_id;
            let result = g.members.get(&uid).map(|m| {
                let mut perms = Permissions::empty();
                if let Some(everyone) = g.roles.get(&RoleId::new(guild_id)) {
                    perms |= everyone.permissions;
                }
                for rid in &m.roles {
                    if let Some(role) = g.roles.get(rid) {
                        perms |= role.permissions;
                    }
                }
                perms.administrator() || perms.manage_guild()
            });
            (is_owner, result)
        }
    };

    if is_owner {
        return true;
    }

    match cached_result {
        Some(can_manage) => can_manage,
        None => {
            // Member not in cache — fall back to HTTP.
            let member = match gid.member(&sctx.http, uid).await {
                Ok(m) => m,
                Err(_) => return false,
            };
            let Some(g) = sctx.cache.guild(gid) else {
                return false;
            };
            let mut perms = Permissions::empty();
            if let Some(everyone) = g.roles.get(&RoleId::new(guild_id)) {
                perms |= everyone.permissions;
            }
            for rid in &member.roles {
                if let Some(role) = g.roles.get(rid) {
                    perms |= role.permissions;
                }
            }
            perms.administrator() || perms.manage_guild()
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
    if let Some(author) = v.get("author")
        && let Some(name) = author.get("name").and_then(|x| x.as_str()) {
            let mut a = CreateEmbedAuthor::new(name);
            if let Some(icon) = author.get("icon_url").and_then(|x| x.as_str()) {
                a = a.icon_url(icon);
            }
            embed = embed.author(a);
        }

    embed
}
