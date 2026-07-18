use super::Cog;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::{colors, interactions};
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, ComponentInteraction, CreateActionRow, CreateButton, CreateEmbed,
    CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage, GuildId,
    Timestamp,
};
use std::sync::Arc;
use sysinfo::{Disks, Networks, System};

/// All of this cog's component ids share the `dev:` prefix so `on_component`
/// can early-return on ids it does not own (component events are fanned out to
/// every cog).
const SYS_INFO_ID: &str = "dev:sys:info";
const SYS_CPU_ID: &str = "dev:sys:cpu";
const SYS_RAM_ID: &str = "dev:sys:ram";

/// Discord truncation budget for output dumped into a code block.
const OUTPUT_LIMIT: usize = 1800;

/// Where to look for file logs. The bot logs to stdout by default; this file
/// only exists if `tracing-appender` is wired up in `main.rs`.
const LOG_FILE: &str = "logs/benny.log";

pub struct DevCog {
    state: Arc<AppState>,
}

impl DevCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for DevCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let id = interaction.data.custom_id.as_str();
        // Early-return on ids this cog does not own.
        if !id.starts_with("dev:") {
            return;
        }
        // Owner-only enforcement on the SystemView controls too.
        if !self.state.is_owner(interaction.user.id.get()) {
            interactions::respond_ephemeral_text(
                ctx,
                interaction,
                "These controls are owner-only.",
            )
            .await;
            return;
        }

        let embed = match id {
            SYS_INFO_ID => build_info_embed(),
            SYS_CPU_ID => build_cpu_embed().await,
            SYS_RAM_ID => build_ram_embed(),
            _ => return,
        };
        let _ = interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(embed)
                        .components(vec![system_view()]),
                ),
            )
            .await;
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![dev()]
}

// ---- commands ---------------------------------------------------------------

/// Developer command group.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    category = "Dev",
    subcommands(
        "dev_system",
        "dev_gitpull",
        "dev_servers",
        "dev_suspicious",
        "dev_leave",
        "dev_close",
        "dev_redis",
        "dev_logs",
        "dev_uptime",
        "dev_ping",
        "dev_sync",
        "dev_clear"
    )
)]
async fn dev(ctx: Context<'_>) -> Result<(), Error> {
    let embed = CreateEmbed::new()
        .title("Developer Commands")
        .description(
            "**System:** `dev system` / `dev sys`\n\
             **Git:** `dev gitpull` / `dev pull`\n\
             **Servers:** `dev servers`, `dev suspicious [ratio]`, `dev leave <guild_id>`\n\
             **Redis:** `dev redis <get|set|search|info|cinfo|showall>`\n\
             **Logs:** `dev logs [n]`\n\
             **Process:** `dev uptime`, `dev ping`, `dev close`\n\
             **Sync:** `dev sync`, `dev clear`\n\
             **Disabled in Rust build:** `dev eval`, `dev load`/`unload`/`reload`, `openfile`",
        )
        .color(colors::BLACK)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Show a system overview embed with Info / CPU / RAM controls.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "system",
    aliases("sys")
)]
async fn dev_system(ctx: Context<'_>) -> Result<(), Error> {
    let embed = base_system_embed().await;
    ctx.send(
        poise::CreateReply::default()
            .embed(embed)
            .components(vec![system_view()]),
    )
    .await?;
    Ok(())
}

/// Run `git pull` and report the output.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "gitpull",
    aliases("pull")
)]
async fn dev_gitpull(ctx: Context<'_>) -> Result<(), Error> {
    let raw = run_git_pull();
    send_embed(ctx, git_embed(&raw)).await
}

/// List every guild the bot is in.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "servers",
    aliases("guilds")
)]
async fn dev_servers(ctx: Context<'_>) -> Result<(), Error> {
    let sctx = ctx.serenity_context();
    let guild_ids = sctx.cache.guilds();
    let bot_name = sctx.cache.current_user().name.clone();
    let count = guild_ids.len();

    let mut lines = String::new();
    for gid in &guild_ids {
        if let Some(g) = sctx.cache.guild(*gid) {
            lines.push_str(&format!(
                "\n{} ({}) \u{2014} {} members",
                g.name,
                gid.get(),
                g.member_count
            ));
        } else {
            lines.push_str(&format!("\n{} (uncached)", gid.get()));
        }
        if lines.len() > OUTPUT_LIMIT {
            lines.push_str("\n...");
            break;
        }
    }
    if lines.is_empty() {
        lines.push_str("\n(no servers)");
    }

    let embed = CreateEmbed::new()
        .title(format!("{bot_name} Server List \u{2014} {count}"))
        .description(format!("```\n{lines}\n```"))
        .color(colors::CYAN)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// List servers whose bot-to-human ratio looks suspicious (owner-only).
///
/// Flags a guild when bots outnumber humans (ratio >= `threshold`, default
/// 1.0) or when there are almost no humans next to a real bot cluster
/// (`humans < 5 && bots >= 3`). Reuses the same `bot_ratio` the auto-leave
/// policy uses; the ratio is computed from cached members.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "suspicious",
    aliases("sus")
)]
async fn dev_suspicious(
    ctx: Context<'_>,
    #[description = "Override the bot:human ratio threshold (default 1.0)"] threshold: Option<f64>,
) -> Result<(), Error> {
    let thresh = threshold.unwrap_or(1.0).max(0.0);
    let sctx = ctx.serenity_context();

    // Gather flagged guilds into owned rows first; never hold a cache guild
    // guard across the later await that sends the embed.
    let mut rows: Vec<(String, u64, usize, usize, usize, f64)> = Vec::new();
    for gid in sctx.cache.guilds() {
        if let Some(g) = sctx.cache.guild(gid) {
            let (bots, humans, total, _pct) = crate::cogs::events::bot_ratio(&g);
            if total == 0 || bots == 0 {
                continue;
            }
            let ratio = bots as f64 / humans.max(1) as f64;
            if ratio >= thresh || (humans < 5 && bots >= 3) {
                rows.push((g.name.to_string(), gid.get(), bots, humans, total, ratio));
            }
        }
    }
    rows.sort_by(|a, b| b.5.partial_cmp(&a.5).unwrap_or(std::cmp::Ordering::Equal));

    if rows.is_empty() {
        let embed = CreateEmbed::new()
            .title("No Suspicious Servers")
            .description(format!(
                "No servers at or above a bot:human ratio of `{thresh:.2}`."
            ))
            .color(colors::GREEN)
            .timestamp(Timestamp::now());
        return send_embed(ctx, embed).await;
    }

    let total_flagged = rows.len();
    let mut lines = String::new();
    let mut shown = 0usize;
    for (name, gid, bots, humans, total, ratio) in &rows {
        lines.push_str(&format!(
            "\n{name} ({gid}) — {bots} bots / {humans} humans / {total} total (ratio {ratio:.2})"
        ));
        shown += 1;
        if lines.len() > OUTPUT_LIMIT {
            break;
        }
    }
    let remaining = total_flagged - shown;
    if remaining > 0 {
        lines.push_str(&format!("\n... +{remaining} more"));
    }

    let embed = CreateEmbed::new()
        .title(format!("Suspicious Servers — {total_flagged}"))
        .description(format!("```{lines}\n```"))
        .color(colors::RED)
        .footer(CreateEmbedFooter::new(format!(
            "ratio = bots / humans • threshold {thresh:.2}"
        )))
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Make the bot leave a guild by ID.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "leave"
)]
async fn dev_leave(
    ctx: Context<'_>,
    #[description = "Guild ID to leave"]
    #[rest]
    guild_id: String,
) -> Result<(), Error> {
    let sctx = ctx.serenity_context();
    let Ok(id) = guild_id.trim().parse::<u64>() else {
        return send_error(ctx, "Usage: `dev leave <guild_id>`").await;
    };
    let gid = GuildId::new(id);
    // Resolve the name from cache before leaving (guard dropped before await).
    let name = sctx.cache.guild(gid).map(|g| g.name.clone());

    match gid.leave(&sctx.http).await {
        Ok(_) => {
            let title = match &name {
                Some(n) => format!("Left {n}"),
                None => format!("Left guild {id}"),
            };
            let embed = CreateEmbed::new()
                .title(title)
                .description(format!("Guild ID: `{id}`"))
                .color(colors::ORANGE)
                .timestamp(Timestamp::now());
            send_embed(ctx, embed).await
        }
        Err(e) => send_error(ctx, &format!("Failed to leave guild: {e}")).await,
    }
}

/// Stop the bot process immediately.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "close",
    aliases("end", "stop")
)]
async fn dev_close(ctx: Context<'_>) -> Result<(), Error> {
    let sctx = ctx.serenity_context();
    if let poise::Context::Prefix(pctx) = ctx {
        let _ = pctx.msg.react(&sctx.http, '\u{2705}').await;
    }
    let embed = CreateEmbed::new()
        .title("Shutting Down Bot")
        .description("Shutting down the bot...")
        .color(colors::RED)
        .timestamp(Timestamp::now());
    let _ = send_embed(ctx, embed).await;
    tracing::warn!(
        owner = ctx.author().id.get(),
        "dev close invoked; exiting process"
    );
    std::process::exit(0);
}

/// Raw Redis access: `get`, `set`, `search`, `info`, `cinfo`, `showall`.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "redis"
)]
async fn dev_redis(
    ctx: Context<'_>,
    #[description = "Redis subcommand and arguments"]
    #[rest]
    args: Option<String>,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let Some(redis) = &state.redis else {
        return send_error(ctx, "Redis is not connected in this build.").await;
    };
    let args_str = args.as_deref().unwrap_or("");
    let (action, args) = split_first(args_str);
    let action = action.to_lowercase();

    let mut conn = redis.lock().await;
    // Build an embed (or an error string) while holding the connection; the
    // guard is dropped before we reply so we never await a send under it.
    let result: Result<CreateEmbed, String> = match action.as_str() {
        "get" | "show" => {
            let key = args.trim();
            if key.is_empty() {
                Err("Usage: `dev redis get <key>`".to_string())
            } else {
                match redis::cmd("GET")
                    .arg(key)
                    .query_async::<Option<String>>(&mut *conn)
                    .await
                {
                    Ok(Some(v)) => Ok(CreateEmbed::new()
                        .title("Redis Key Data")
                        .description(format!("```\n{}\n```", truncate_str(&v, 1900)))
                        .color(colors::BLURPLE)
                        .timestamp(Timestamp::now())),
                    Ok(None) => Ok(CreateEmbed::new()
                        .title(format!("Key {key} Not Found"))
                        .color(colors::RED)
                        .timestamp(Timestamp::now())),
                    Err(e) => Err(format!("Redis error: {e}")),
                }
            }
        }
        "set" | "add" | "+" => {
            let (key, value) = split_first(args);
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                Err("Usage: `dev redis set <key> <value>`".to_string())
            } else {
                match redis::cmd("SET")
                    .arg(key)
                    .arg(value)
                    .exec_async(&mut *conn)
                    .await
                {
                    Ok(()) => Ok(CreateEmbed::new()
                        .title("Added Key")
                        .description(format!("```md\n[{key}]({value})\n```"))
                        .color(colors::GREEN)
                        .timestamp(Timestamp::now())),
                    Err(e) => Err(format!("Redis error: {e}")),
                }
            }
        }
        "search" => {
            let p = args.trim();
            let pattern = if p.is_empty() { "*" } else { p };
            match redis::cmd("KEYS")
                .arg(pattern)
                .query_async::<Vec<String>>(&mut *conn)
                .await
            {
                Ok(keys) => {
                    let total = keys.len();
                    let mut body = String::new();
                    for (i, k) in keys.iter().enumerate() {
                        body.push_str(&format!("\n{}. {k}", i + 1));
                        if body.len() > OUTPUT_LIMIT {
                            body.push_str("\n...");
                            break;
                        }
                    }
                    if body.is_empty() {
                        body = format!("[{pattern}][None]");
                    }
                    Ok(CreateEmbed::new()
                        .title(format!("Redis Keys in Database \u{2014} {total}"))
                        .description(format!("```md\n{body}\n```"))
                        .color(colors::BLURPLE)
                        .timestamp(Timestamp::now()))
                }
                Err(e) => Err(format!("Redis error: {e}")),
            }
        }
        "info" | "i" => {
            let who: String = redis::cmd("ACL")
                .arg("WHOAMI")
                .query_async(&mut *conn)
                .await
                .unwrap_or_else(|_| "?".to_string());
            let cname: String = redis::cmd("CLIENT")
                .arg("GETNAME")
                .query_async(&mut *conn)
                .await
                .unwrap_or_default();
            let cid: i64 = redis::cmd("CLIENT")
                .arg("ID")
                .query_async(&mut *conn)
                .await
                .unwrap_or(-1);
            let dbsize: i64 = redis::cmd("DBSIZE")
                .query_async(&mut *conn)
                .await
                .unwrap_or(-1);
            let desc = format!(
                "```asciidoc\n= ACL Info =\n[ User: {who} ]\n\n= Connection Info =\n[ Name: {cname} ]\n[ ID: {cid} ]\n\n= Misc =\n[ Database Size (Keys): {dbsize} ]\n```"
            );
            Ok(CreateEmbed::new()
                .title("Redis Info")
                .description(desc)
                .color(colors::BLURPLE)
                .timestamp(Timestamp::now()))
        }
        "cinfo" | "complex" | "c" => {
            match redis::cmd("INFO").query_async::<String>(&mut *conn).await {
                Ok(info) => Ok(CreateEmbed::new()
                    .title("Redis Complex Info")
                    .description(format!("```\n{}\n```", truncate_str(info.trim(), 3800)))
                    .color(colors::BLURPLE)
                    .timestamp(Timestamp::now())),
                Err(e) => Err(format!("Redis error: {e}")),
            }
        }
        "showall" | "sa" => {
            match redis::cmd("SCAN")
                .arg(0)
                .query_async::<(String, Vec<String>)>(&mut *conn)
                .await
            {
                Ok((_cursor, keys)) => {
                    let mut body = String::new();
                    for k in &keys {
                        let val: Option<String> = redis::cmd("GET")
                            .arg(k)
                            .query_async(&mut *conn)
                            .await
                            .unwrap_or(None);
                        let val = val.unwrap_or_else(|| "<non-string>".to_string());
                        body.push_str(&format!("\n{k}: {val}"));
                        if body.len() > OUTPUT_LIMIT {
                            body.push_str("\n...");
                            break;
                        }
                    }
                    if body.is_empty() {
                        body.push_str("(no keys)");
                    }
                    Ok(CreateEmbed::new()
                        .title("Redis \u{2014} All Keys")
                        .description(format!("```yaml\n{}\n```", truncate_str(&body, 1900)))
                        .color(colors::GREEN)
                        .timestamp(Timestamp::now()))
                }
                Err(e) => Err(format!("Redis error: {e}")),
            }
        }
        "" => Err("Usage: `dev redis <get|set|search|info|cinfo|showall> ...`".to_string()),
        other => Err(format!("Unknown redis subcommand `{other}`.")),
    };
    drop(conn);

    match result {
        Ok(embed) => send_embed(ctx, embed).await,
        Err(e) => send_error(ctx, &e).await,
    }
}

/// Show the last N lines of the bot log file.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "logs"
)]
async fn dev_logs(
    ctx: Context<'_>,
    #[description = "Number of lines to show (default 10, max 100)"]
    #[rest]
    args: Option<String>,
) -> Result<(), Error> {
    let rest = args.as_deref().unwrap_or("");
    let n: usize = rest.trim().parse().unwrap_or(10).clamp(1, 100);
    match std::fs::read_to_string(LOG_FILE) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(n);
            let tail = lines[start..].join("\n");
            let body = if tail.trim().is_empty() {
                "(log file is empty)".to_string()
            } else {
                truncate_str(&tail, 1900)
            };
            let embed = CreateEmbed::new()
                .title(format!("Last {n} log line(s)"))
                .description(format!("```\n{body}\n```"))
                .color(colors::DARK_GRAY)
                .timestamp(Timestamp::now());
            send_embed(ctx, embed).await
        }
        Err(_) => {
            let embed = CreateEmbed::new()
                .title("Not Supported")
                .description(format!(
                    "`{LOG_FILE}` does not exist yet \u{2014} no log entries have been written so far. \
                     It will be created once the bot writes its first log entry."
                ))
                .color(colors::YELLOW)
                .timestamp(Timestamp::now());
            send_embed(ctx, embed).await
        }
    }
}

/// Show how long the bot process has been running.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "uptime"
)]
async fn dev_uptime(ctx: Context<'_>) -> Result<(), Error> {
    let secs = ctx.data().state.uptime_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    ctx.say(format!("Uptime: {h}h {m}m {s}s")).await?;
    Ok(())
}

/// Trivial liveness check.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "ping"
)]
async fn dev_ping(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("Pong (dev)!").await?;
    Ok(())
}

/// Register the bot's global application (slash) commands.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "sync"
)]
async fn dev_sync(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_globally(
        ctx.serenity_context(),
        &ctx.framework().options().commands,
    )
    .await?;
    let embed = CreateEmbed::new()
        .title("Commands Synced")
        .description("Global application commands have been registered.")
        .color(colors::GREEN)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

/// Clear all registered global application (slash) commands.
#[poise::command(
    slash_command,
    prefix_command,
    owners_only,
    hide_in_help,
    rename = "clear"
)]
async fn dev_clear(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_globally(
        ctx.serenity_context(),
        &[] as &[poise::Command<Data, Error>],
    )
    .await?;
    let embed = CreateEmbed::new()
        .title("Commands Cleared")
        .description("All global application commands have been cleared.")
        .color(colors::RED)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}

// ---- free helpers ---------------------------------------------------------

/// Split off the first whitespace-delimited token, returning it plus the
/// remainder (with leading whitespace trimmed). Robust to runs of spaces.
fn split_first(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim_start()),
        None => (s, ""),
    }
}

/// Char-boundary-safe truncation with an ellipsis marker.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\u{2026}", &s[..end])
}

/// Human-readable byte size (1024 factor, B/KB/...).
fn human_bytes(bytes: u64) -> String {
    let mut b = bytes as f64;
    for unit in ["", "K", "M", "G", "T", "P"] {
        if b < 1024.0 {
            return format!("{b:.2}{unit}B");
        }
        b /= 1024.0;
    }
    format!("{b:.2}PB")
}

/// Run `git pull` and collect a single string of its combined output.
fn run_git_pull() -> String {
    match std::process::Command::new("git").arg("pull").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let mut s = String::new();
            if !stdout.trim().is_empty() {
                s.push_str(stdout.trim());
            }
            if !stderr.trim().is_empty() {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(stderr.trim());
            }
            if s.is_empty() {
                s.push_str("(no output)");
            }
            s
        }
        Err(e) => format!("Failed to run git pull: {e}"),
    }
}

/// Light ANSI colourisation of git output: highlight the fast-forward / update / summary lines.
fn format_git_msg(content: &str) -> String {
    content
        .lines()
        .map(|line| {
            if line.contains("Fast-forward") {
                format!("\u{1b}[0;35m{line}\u{1b}[0m")
            } else if line.starts_with("Updating") {
                format!("\u{1b}[0;33m{line}\u{1b}[0m")
            } else if line.contains("insertion")
                || line.contains("deletion")
                || line.contains("changed")
            {
                format!("\u{1b}[0;32m{line}\u{1b}[0m")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Embed wrapping git-pull output in an ANSI code block.
fn git_embed(raw: &str) -> CreateEmbed {
    let colored = format_git_msg(raw);
    let body = truncate_str(&colored, OUTPUT_LIMIT);
    CreateEmbed::new()
        .title("Git Pull")
        .description(format!("```ansi\n{body}\n```"))
        .color(colors::CYAN)
        .timestamp(Timestamp::now())
}

/// The Info / CPU / RAM button bar for the SystemView. All ids are `dev:`-prefixed.
fn system_view() -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(SYS_INFO_ID)
            .style(ButtonStyle::Primary)
            .label("Info")
            .emoji('\u{2139}'),
        CreateButton::new(SYS_CPU_ID)
            .style(ButtonStyle::Primary)
            .label("CPU")
            .emoji('\u{1f5a5}'),
        CreateButton::new(SYS_RAM_ID)
            .style(ButtonStyle::Primary)
            .label("RAM")
            .emoji('\u{1f4be}'),
    ])
}

const SYS_FOOTER: &str = "Select one of the below options for more info";

/// Overview embed shown by `dev system`: host, CPU, RAM, disk and network.
async fn base_system_embed() -> CreateEmbed {
    let mut sys = System::new();
    sys.refresh_cpu_all();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let os_name = System::name().unwrap_or_else(|| "Unknown".to_string());
    let host = System::host_name().unwrap_or_else(|| "Unknown".to_string());
    let cores = sys.cpus().len();
    let cpu = sys.global_cpu_usage();
    let used = sys.used_memory();
    let total = sys.total_memory();
    let mem_pct = if total > 0 {
        used as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    let disks = Disks::new_with_refreshed_list();
    let (disk_used, disk_total) = disks
        .list()
        .iter()
        .next()
        .map(|d| {
            (
                d.total_space().saturating_sub(d.available_space()),
                d.total_space(),
            )
        })
        .unwrap_or((0, 0));

    let nets = Networks::new_with_refreshed_list();
    let (rx, tx) = nets.list().values().fold((0u64, 0u64), |(r, t), d| {
        (r + d.total_received(), t + d.total_transmitted())
    });

    let desc = format!(
        "```asciidoc\n\
         [ System ]\n= {os_name} =\n\
         [ Hostname ]\n= {host} =\n\
         [ Total Cores ]\n= {cores} =\n\
         [ CPU Usage ]\n= {cpu:.1}% =\n\
         [ Memory (Used / Total) ]\n= {used_h} / {total_h} ({mem_pct:.1}%) =\n\
         [ Disk (Used / Total) ]\n= {du_h} / {dt_h} =\n\
         [ Network (RX / TX) ]\n= {rx_h} / {tx_h} =\n\
         ```",
        used_h = human_bytes(used),
        total_h = human_bytes(total),
        du_h = human_bytes(disk_used),
        dt_h = human_bytes(disk_total),
        rx_h = human_bytes(rx),
        tx_h = human_bytes(tx),
    );

    CreateEmbed::new()
        .title("System Info")
        .description(desc)
        .color(colors::GRAY)
        .footer(CreateEmbedFooter::new(SYS_FOOTER))
        .timestamp(Timestamp::now())
}

/// Info page: uname-style host details.
fn build_info_embed() -> CreateEmbed {
    let mut sys = System::new();
    sys.refresh_cpu_all();

    let system = System::name().unwrap_or_else(|| "Unknown".to_string());
    let node = System::host_name().unwrap_or_else(|| "Unknown".to_string());
    let release = System::kernel_version().unwrap_or_else(|| "Unknown".to_string());
    let version = System::os_version().unwrap_or_else(|| "Unknown".to_string());
    let machine = std::env::consts::ARCH;
    let processor = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "Unknown".to_string());

    let desc = format!(
        "```asciidoc\n\
         [ System ]\n= {system} =\n\
         [ Node Name ]\n= {node} =\n\
         [ Release ]\n= {release} =\n\
         [ Version ]\n= {version} =\n\
         [ Machine ]\n= {machine} =\n\
         [ Processor ]\n= {processor} =\n\
         ```"
    );

    CreateEmbed::new()
        .title("System Info - Info")
        .description(desc)
        .color(colors::GRAY)
        .footer(CreateEmbedFooter::new(SYS_FOOTER))
        .timestamp(Timestamp::now())
}

/// CPU page: per-core and total usage.
async fn build_cpu_embed() -> CreateEmbed {
    let mut sys = System::new();
    sys.refresh_cpu_all();
    tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
    sys.refresh_cpu_all();

    let mut per_core = String::new();
    for (i, cpu) in sys.cpus().iter().enumerate() {
        per_core.push_str(&format!("[Core {}]\n= {:.1}% =\n", i + 1, cpu.cpu_usage()));
    }
    let total_cores = sys.cpus().len();
    let total = sys.global_cpu_usage();

    let desc = format!(
        "```asciidoc\n\
         [ Total Cores ]\n= {total_cores} =\n\n\
         [ CPU Usage Per Core ]\n{per_core}\n\
         [ Total CPU Usage ]\n= {total:.1}% =\n\
         ```"
    );

    CreateEmbed::new()
        .title("System Info - CPU")
        .description(truncate_str(&desc, 4000))
        .color(colors::GRAY)
        .footer(CreateEmbedFooter::new(SYS_FOOTER))
        .timestamp(Timestamp::now())
}

/// RAM page: memory totals and usage.
fn build_ram_embed() -> CreateEmbed {
    let mut sys = System::new();
    sys.refresh_memory();

    let total = sys.total_memory();
    let available = sys.available_memory();
    let used = sys.used_memory();
    let pct = if total > 0 {
        used as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    let desc = format!(
        "```asciidoc\n\
         [ Total ]\n= {} =\n\
         [ Available ]\n= {} =\n\
         [ Used ]\n= {} =\n\
         [ Percentage Used ]\n= {pct:.1}% =\n\
         ```",
        human_bytes(total),
        human_bytes(available),
        human_bytes(used),
    );

    CreateEmbed::new()
        .title("System Info - Memory")
        .description(desc)
        .color(colors::GRAY)
        .footer(CreateEmbedFooter::new(SYS_FOOTER))
        .timestamp(Timestamp::now())
}
