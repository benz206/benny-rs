use super::Cog;
use crate::state::{AppState, CommandInvocation};
use crate::utils::format::humanize_duration;
use crate::utils::parse::parse_user_id;
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use chrono::Utc;
use serenity::all::{
    Context, CreateEmbed, CreateEmbedFooter, CreateMessage, EditMessage, Message, Permissions,
    Timestamp, UserId,
};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use std::{fs, path::PathBuf};

const REPO_URL: &str = "https://github.com/benz206/benny-rs";

/// Shared note pinned to the ping embed footer.
const PING_NOTE: &str = "Please note that this will be much slower when you use slash commands";

/// Cached source-tree statistics: computed once by walking `src/` on the first
/// `about`/`files` invocation.
struct FileStats {
    files: u64,
    lines: u64,
    chars: u64,
    /// Sorted `(relative path, line count)` pairs for the `files` listing.
    per_file: Vec<(String, u64)>,
}

pub struct BaseCog {
    state: Arc<AppState>,
    stats: OnceLock<FileStats>,
}

impl BaseCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            stats: OnceLock::new(),
        })
    }

    /// Lazily compute (and cache) the source stats.
    fn stats(&self) -> &FileStats {
        self.stats.get_or_init(compute_file_stats)
    }
}

#[async_trait]
impl Cog for BaseCog {
    async fn on_command(&self, ctx: &Context, msg: &Message, inv: &CommandInvocation<'_>) -> bool {
        let arg = inv.args;
        match inv.command {
            "ping" | "pong" => self.cmd_ping(ctx, msg).await,
            "about" => self.cmd_about(ctx, msg).await,
            "version" => self.cmd_version(ctx, msg).await,
            "uptime" => self.cmd_uptime(ctx, msg).await,
            "files" => self.cmd_files(ctx, msg).await,
            "invite" => self.cmd_invite(ctx, msg).await,
            "charinfo" | "ci" | "char" => self.cmd_charinfo(ctx, msg, arg).await,
            "permissions" | "perms" => self.cmd_permissions(ctx, msg, arg).await,
            _ => return false,
        }
        true
    }
}

impl BaseCog {
    async fn reply_embed(&self, ctx: &Context, msg: &Message, embed: CreateEmbed) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    // ---- ping -------------------------------------------------------------

    async fn cmd_ping(&self, ctx: &Context, msg: &Message) {
        let initial = CreateEmbed::new()
            .title("Pinging...")
            .description("Checking Ping")
            .color(colors::GRAY)
            .footer(CreateEmbedFooter::new(PING_NOTE))
            .timestamp(Timestamp::now());

        let start = Instant::now();
        let sent = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(initial))
            .await;
        let Ok(mut sent) = sent else { return };
        let rest = start.elapsed().as_secs_f64();

        // Color tiers (seconds): >=3 red, >=2 orange, >=1 yellow.
        let color = if rest >= 3.0 {
            colors::RED
        } else if rest >= 2.0 {
            colors::ORANGE
        } else if rest >= 1.0 {
            colors::YELLOW
        } else {
            colors::GREEN
        };

        // Second REST sample taken around the edit call.
        let edit_start = Instant::now();
        let result = CreateEmbed::new()
            .title("Pong!")
            .description(format!(
                "**Overall Latency:** `{:.0} ms`\n**REST Latency:** `{:.0} ms`",
                rest * 1000.0,
                edit_start.elapsed().as_secs_f64() * 1000.0
            ))
            .color(color)
            .footer(CreateEmbedFooter::new(PING_NOTE))
            .timestamp(Timestamp::now());
        let _ = sent.edit(&ctx.http, EditMessage::new().embed(result)).await;
    }

    // ---- about ------------------------------------------------------------

    async fn cmd_about(&self, ctx: &Context, msg: &Message) {
        // Pull cache-backed counts/identity synchronously so no `!Send` cache
        // ref is held across an await point.
        let (guild_count, user_count, bot_name, bot_avatar) = {
            let guilds = ctx.cache.guilds();
            let gc = guilds.len();
            let mut uc: u64 = 0;
            for gid in &guilds {
                if let Some(g) = ctx.cache.guild(*gid) {
                    uc += g.member_count;
                }
            }
            let cu = ctx.cache.current_user();
            (gc, uc, cu.name.clone(), cu.avatar_url())
        };

        let s = self.stats();
        let uptime = humanize_duration(Duration::from_secs(self.state.uptime_secs()));

        let mut footer = CreateEmbedFooter::new(bot_name);
        if let Some(av) = bot_avatar {
            footer = footer.icon_url(av);
        }

        let embed = CreateEmbed::new()
            .title("About the Bot")
            .description(
                "A Bot I've made for fun, friends and learning Rust.\n\
                The bot also does a lot of odd things I feel I may need such as reading text off \
                images, playing music, and stealing sheetmusic, lol.\n\
                Hope you enjoy",
            )
            .color(colors::TEAL)
            .field("Version", format!("v{}", env!("CARGO_PKG_VERSION")), true)
            .field(
                "Library",
                "[serenity](https://github.com/serenity-rs/serenity)",
                true,
            )
            .field("Uptime", uptime, true)
            .field("Guilds", guild_count.to_string(), true)
            .field("Users", user_count.to_string(), true)
            .field(
                "Source",
                format!(
                    "**{}** files\n**{}** lines\n**{}** chars",
                    s.files, s.lines, s.chars
                ),
                true,
            )
            .footer(footer)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- version ----------------------------------------------------------

    async fn cmd_version(&self, ctx: &Context, msg: &Message) {
        let Some((head_short, commits)) = latest_commits(5) else {
            self.reply_embed(
                ctx,
                msg,
                embeds::error_embed("Could not read the git history."),
            )
            .await;
            return;
        };
        let embed = CreateEmbed::new()
            .title(format!("Current Version: {head_short}"))
            .description(commits)
            .color(colors::TEAL)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- uptime -----------------------------------------------------------

    async fn cmd_uptime(&self, ctx: &Context, msg: &Message) {
        let bot_name = ctx.cache.current_user().name.clone();
        let secs = self.state.uptime_secs();
        let started = Utc::now().timestamp() - secs as i64;
        let humanized = humanize_duration(Duration::from_secs(secs));

        let embed = CreateEmbed::new()
            .title(format!("{bot_name} Uptime"))
            .description(format!(
                "Started at <t:{started}:F>\nTotal Uptime: {humanized} (<t:{started}:R>)"
            ))
            .color(colors::random())
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- files ------------------------------------------------------------

    async fn cmd_files(&self, ctx: &Context, msg: &Message) {
        let s = self.stats();

        // Build the per-file JSON dump (path -> line count).
        let mut body = String::from("{\n");
        for (i, (path, lines)) in s.per_file.iter().enumerate() {
            let comma = if i + 1 < s.per_file.len() { "," } else { "" };
            body.push_str(&format!("    \"{path}\": {lines}{comma}\n"));
        }
        body.push('}');

        // Embed descriptions cap at 4096 chars; fall back to a totals summary
        // for very large trees.
        let mut description = format!("```json\n{body}\n```");
        if description.len() > 4000 {
            description = format!(
                "```json\n{{\n    \"files\": {},\n    \"lines\": {},\n    \"chars\": {}\n}}\n```",
                s.files, s.lines, s.chars
            );
        }

        let embed = CreateEmbed::new()
            .title("File Lines")
            .description(description)
            .color(colors::TEAL)
            .footer(CreateEmbedFooter::new(format!("{} files listed.", s.files)))
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- invite -----------------------------------------------------------

    async fn cmd_invite(&self, ctx: &Context, msg: &Message) {
        let bot_id = ctx.cache.current_user().id.get();
        let url = format!(
            "https://discord.com/api/oauth2/authorize?client_id={bot_id}\
            &permissions=1636352650487&scope=applications.commands%20bot"
        );
        let embed = CreateEmbed::new()
            .title("Invite Me")
            .description(format!("[Invite]({url}) me to your server!"))
            .color(colors::TEAL)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- charinfo ---------------------------------------------------------

    async fn cmd_charinfo(&self, ctx: &Context, msg: &Message, arg: &str) {
        if arg.is_empty() {
            self.reply_embed(
                ctx,
                msg,
                embeds::error_embed("Give me some characters to look up."),
            )
            .await;
            return;
        }

        // Cap the number of characters so the joined description stays under the
        // 4096-char embed limit.
        let lines: Vec<String> = arg
            .chars()
            .take(25)
            .map(|ch| {
                let cp = ch as u32;
                let name = unicode_names2::name(ch)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "Name not found.".to_string());
                format!(
                    "`\\U{cp:08x} - {ch}` [{name}](http://www.fileformat.info/info/unicode/char/{cp:x})"
                )
            })
            .collect();

        let embed = CreateEmbed::new()
            .title("Charinfo")
            .description(lines.join("\n"))
            .color(colors::YELLOW)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- permissions ------------------------------------------------------

    async fn cmd_permissions(&self, ctx: &Context, msg: &Message, arg: &str) {
        let Some(guild_id) = msg.guild_id else {
            self.reply_embed(
                ctx,
                msg,
                embeds::error_embed("This command can only be used in a server."),
            )
            .await;
            return;
        };

        let target_id = if arg.is_empty() {
            msg.author.id.get()
        } else {
            match parse_user_id(arg) {
                Some(id) => id,
                None => {
                    self.reply_embed(ctx, msg, embeds::error_embed("Member not found."))
                        .await;
                    return;
                }
            }
        };

        let member = match guild_id.member(&ctx.http, UserId::new(target_id)).await {
            Ok(m) => m,
            Err(_) => {
                self.reply_embed(ctx, msg, embeds::error_embed("Member not found."))
                    .await;
                return;
            }
        };

        // Prefer the cache (GUILDS intent); fall back to a partial-guild fetch
        // so the lookup still resolves on a cold cache.
        let cached_perms: Option<Permissions> = ctx
            .cache
            .guild(guild_id)
            .map(|g| g.member_permissions(&member));
        let perms = match cached_perms {
            Some(p) => p,
            None => match guild_id.to_partial_guild(&ctx.http).await {
                Ok(pg) => pg.member_permissions(&member),
                Err(_) => {
                    self.reply_embed(
                        ctx,
                        msg,
                        embeds::error_embed("Could not resolve permissions."),
                    )
                    .await;
                    return;
                }
            },
        };

        let names = perms.get_permission_names();
        let description = if names.is_empty() {
            "No permissions.".to_string()
        } else {
            names
                .iter()
                .map(|n| format!("\u{2022} {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let embed = CreateEmbed::new()
            .title(format!("Permissions for {}", member.display_name()))
            .description(description)
            .color(colors::BLURPLE)
            .footer(CreateEmbedFooter::new(format!(
                "{} permission(s)",
                names.len()
            )))
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }
}

// ---- free helpers ---------------------------------------------------------

/// Walk `src/`, counting `.rs` files, total lines, total chars, and per-file
/// line counts. Runs once; the result is cached in `BaseCog::stats`.
fn compute_file_stats() -> FileStats {
    let mut files = 0u64;
    let mut lines = 0u64;
    let mut chars = 0u64;
    let mut per_file: Vec<(String, u64)> = Vec::new();

    let mut stack = vec![PathBuf::from("src")];
    while let Some(p) = stack.pop() {
        let Ok(rd) = fs::read_dir(&p) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(text) = fs::read_to_string(&path) {
                    let l = text.lines().count() as u64;
                    let c = text.chars().count() as u64;
                    files += 1;
                    lines += l;
                    chars += c;
                    per_file.push((path.to_string_lossy().replace('\\', "/"), l));
                }
            }
        }
    }

    per_file.sort();
    FileStats {
        files,
        lines,
        chars,
        per_file,
    }
}

/// Format the latest `count` commits via `git log`. Returns the HEAD short hash
/// plus a newline-joined, markdown-linked commit list. `None` when git is
/// unavailable or the directory is not a repository.
fn latest_commits(count: usize) -> Option<(String, String)> {
    let output = std::process::Command::new("git")
        .args(["log", "-n", &count.to_string(), "--format=%H\x1f%s\x1f%ct"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut head_short = String::new();
    let mut entries: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\u{1f}');
        let full = parts.next().unwrap_or("");
        let subject = parts.next().unwrap_or("");
        let ts = parts.next().unwrap_or("");
        if full.is_empty() {
            continue;
        }
        let short: String = full.chars().take(8).collect();
        if head_short.is_empty() {
            head_short = short.clone();
        }
        entries.push(format!(
            "[`{short}`]({REPO_URL}/commit/{full}) {subject} (<t:{ts}:R>)"
        ));
    }

    if entries.is_empty() {
        None
    } else {
        Some((head_short, entries.join("\n")))
    }
}
