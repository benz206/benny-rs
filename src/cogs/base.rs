use super::Cog;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::format::{humanize_duration, truncate};
use crate::utils::colors;
use chrono::Utc;
use serenity::all::{CreateEmbed, CreateEmbedFooter, Member, Permissions, Timestamp};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use std::{fs, path::PathBuf};

const REPO_URL: &str = "https://github.com/benz206/benny-rs";

/// Shared note pinned to the ping embed footer.
const PING_NOTE: &str = "Please note that this will be much slower when you use slash commands";

/// Cached source-tree statistics: computed once at process start.
struct FileStats {
    files: u64,
    lines: u64,
    chars: u64,
    /// Sorted `(relative path, line count)` pairs for the `files` listing.
    per_file: Vec<(String, u64)>,
}

static FILE_STATS: LazyLock<FileStats> = LazyLock::new(compute_file_stats);

pub struct BaseCog;

impl BaseCog {
    pub fn new(_state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self)
    }
}

impl Cog for BaseCog {}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        ping(),
        about(),
        version(),
        uptime(),
        files(),
        invite(),
        charinfo(),
        perms(),
    ]
}

// ---- commands ---------------------------------------------------------------

/// Check the bot's latency.
#[poise::command(slash_command, prefix_command, category = "Info & Utility", aliases("pong"))]
async fn ping(ctx: Context<'_>) -> Result<(), Error> {
    let initial = CreateEmbed::new()
        .title("Pinging...")
        .description("Checking Ping")
        .color(colors::GRAY)
        .footer(CreateEmbedFooter::new(PING_NOTE))
        .timestamp(Timestamp::now());

    let start = Instant::now();
    let handle = ctx
        .send(poise::CreateReply::default().embed(initial))
        .await?;
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
    handle
        .edit(ctx, poise::CreateReply::default().embed(result))
        .await?;
    Ok(())
}

/// Show information about the bot.
#[poise::command(slash_command, prefix_command, category = "Info & Utility")]
async fn about(ctx: Context<'_>) -> Result<(), Error> {
    let sctx = ctx.serenity_context();
    let (guild_count, user_count, bot_name, bot_avatar) = {
        let guilds = sctx.cache.guilds();
        let gc = guilds.len();
        let mut uc: u64 = 0;
        for gid in &guilds {
            if let Some(g) = sctx.cache.guild(*gid) {
                uc += g.member_count;
            }
        }
        let cu = sctx.cache.current_user();
        (gc, uc, cu.name.clone(), cu.avatar_url())
    };

    let s = &*FILE_STATS;
    let uptime = humanize_duration(Duration::from_secs(ctx.data().state.uptime_secs()));

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
    send_embed(ctx, embed).await
}

/// Show the current git version and recent commits.
#[poise::command(slash_command, prefix_command, category = "Info & Utility")]
async fn version(ctx: Context<'_>) -> Result<(), Error> {
    let Some((head_short, commits)) = latest_commits(5) else {
        return send_error(ctx, "Could not read the git history.").await;
    };
    let embed = CreateEmbed::new()
        .title(format!("Current Version: {head_short}"))
        .description(commits)
        .color(colors::TEAL)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Show how long the bot has been running.
#[poise::command(slash_command, prefix_command, category = "Info & Utility")]
async fn uptime(ctx: Context<'_>) -> Result<(), Error> {
    let sctx = ctx.serenity_context();
    let bot_name = sctx.cache.current_user().name.clone();
    let secs = ctx.data().state.uptime_secs();
    let started = Utc::now().timestamp() - secs as i64;
    let humanized = humanize_duration(Duration::from_secs(secs));

    let embed = CreateEmbed::new()
        .title(format!("{bot_name} Uptime"))
        .description(format!(
            "Started at <t:{started}:F>\nTotal Uptime: {humanized} (<t:{started}:R>)"
        ))
        .color(colors::random())
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// List source file statistics.
#[poise::command(slash_command, prefix_command, category = "Info & Utility")]
async fn files(
    ctx: Context<'_>,
    #[description = "Show full per-file listing"] full: Option<bool>,
) -> Result<(), Error> {
    let s = &*FILE_STATS;

    let description = if full == Some(true) {
        let mut body = String::from("{\n");
        for (i, (path, lines)) in s.per_file.iter().enumerate() {
            let comma = if i + 1 < s.per_file.len() { "," } else { "" };
            body.push_str(&format!("    \"{path}\": {lines}{comma}\n"));
        }
        body.push('}');
        let full_desc = format!("```json\n{body}\n```");
        if full_desc.len() > 4000 {
            format!(
                "```json\n{{\n    \"files\": {},\n    \"lines\": {},\n    \"chars\": {}\n}}\n```",
                s.files, s.lines, s.chars
            )
        } else {
            full_desc
        }
    } else {
        format!(
            "```json\n{{\n    \"files\": {},\n    \"lines\": {},\n    \"chars\": {}\n}}\n```",
            s.files, s.lines, s.chars
        )
    };

    let embed = CreateEmbed::new()
        .title("File Lines")
        .description(description)
        .color(colors::TEAL)
        .footer(CreateEmbedFooter::new(format!("{} files listed.", s.files)))
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Get the bot's invite link.
#[poise::command(slash_command, prefix_command, category = "Info & Utility")]
async fn invite(ctx: Context<'_>) -> Result<(), Error> {
    let bot_id = ctx.serenity_context().cache.current_user().id.get();
    let url = format!(
        "https://discord.com/api/oauth2/authorize?client_id={bot_id}\
        &permissions=1636352650487&scope=applications.commands%20bot"
    );
    let embed = CreateEmbed::new()
        .title("Invite Me")
        .description(format!("[Invite]({url}) me to your server!"))
        .color(colors::TEAL)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Look up Unicode information for one or more characters.
#[poise::command(slash_command, prefix_command, category = "Info & Utility", aliases("ci", "char"))]
async fn charinfo(
    ctx: Context<'_>,
    #[description = "Characters"]
    #[rest]
    characters: String,
) -> Result<(), Error> {
    let lines: Vec<String> = characters
        .chars()
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

    let joined = lines.join("\n");
    let embed = CreateEmbed::new()
        .title("Charinfo")
        .description(truncate(&joined, 1900).to_string())
        .color(colors::YELLOW)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Show permissions for a member (defaults to yourself).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Info & Utility",
    aliases("permissions")
)]
async fn perms(
    ctx: Context<'_>,
    #[description = "Member"] member: Option<Member>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let member = match member {
        Some(m) => m,
        None => match guild_id.member(&sctx.http, ctx.author().id).await {
            Ok(m) => m,
            Err(_) => return send_error(ctx, "Could not resolve your member data.").await,
        },
    };

    let cached_perms: Option<Permissions> = sctx
        .cache
        .guild(guild_id)
        .map(|g| g.member_permissions(&member));
    let perms = match cached_perms {
        Some(p) => p,
        None => match guild_id.to_partial_guild(&sctx.http).await {
            Ok(pg) => pg.member_permissions(&member),
            Err(_) => return send_error(ctx, "Could not resolve permissions.").await,
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
    send_embed(ctx, embed).await
}

// ---- free helpers ---------------------------------------------------------

/// Walk `src/`, counting `.rs` files, total lines, total chars, and per-file
/// line counts. Runs once; the result is cached in `FILE_STATS`.
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
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && let Ok(text) = fs::read_to_string(&path) {
                    let l = text.lines().count() as u64;
                    let c = text.chars().count() as u64;
                    files += 1;
                    lines += l;
                    chars += c;
                    per_file.push((path.to_string_lossy().replace('\\', "/"), l));
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
