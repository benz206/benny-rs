use super::Cog;
use crate::state::AppState;
use crate::utils::colors;
use crate::utils::embeds::error_embed;
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, ComponentInteraction, Context, CreateActionRow, CreateButton, CreateEmbed,
    CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    GuildId, Message, Timestamp,
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
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        // Owner-only enforcement on every dev command.
        if !self.state.is_owner(msg.author.id.get()) {
            return;
        }

        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        let (cmd, after) = split_first(body);
        if cmd != "dev" {
            return;
        }
        let (subcmd, rest) = split_first(after);

        match subcmd {
            "sysinfo" | "sys" | "system" => self.cmd_system(ctx, msg).await,
            "gitpull" | "pull" => self.cmd_gitpull(ctx, msg).await,
            "sync" => self.cmd_sync(ctx, msg).await,
            "syncs" => {
                self.reply_unsupported(
                    ctx,
                    msg,
                    "Slash-command tree sync is not available at runtime; commands are registered globally at startup.",
                )
                .await
            }
            "clear" => {
                self.reply_unsupported(
                    ctx,
                    msg,
                    "Clearing the slash-command tree is not available at runtime in the Rust build.",
                )
                .await
            }
            "load" | "unload" | "reload" => {
                self.reply_unsupported(
                    ctx,
                    msg,
                    "Cog load/unload/reload is not supported in the Rust build — cogs are compiled in statically. Use `dev gitpull` and restart the process.",
                )
                .await
            }
            "servers" | "guilds" => self.cmd_servers(ctx, msg).await,
            "leave" => self.cmd_leave(ctx, msg, rest).await,
            "close" | "end" | "stop" => self.cmd_close(ctx, msg).await,
            "redis" => self.cmd_redis(ctx, msg, rest).await,
            "logs" | "blogs" => self.cmd_logs(ctx, msg, rest).await,
            "uptime" => self.cmd_uptime(ctx, msg).await,
            "ping" => self.cmd_ping(ctx, msg).await,
            "eval" | "exec" => {
                self.reply_unsupported(ctx, msg, "The `eval` command is disabled in the Rust build.")
                    .await
            }
            _ => self.cmd_help(ctx, msg).await,
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let id = interaction.data.custom_id.as_str();
        // Early-return on ids this cog does not own.
        if !id.starts_with("dev:") {
            return;
        }
        // Owner-only enforcement on the SystemView controls too.
        if !self.state.is_owner(interaction.user.id.get()) {
            let _ = interaction
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("These controls are owner-only."),
                    ),
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

impl DevCog {
    // ---- shared helpers ---------------------------------------------------

    async fn reply_embed(&self, ctx: &Context, msg: &Message, embed: CreateEmbed) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    async fn reply_error(&self, ctx: &Context, msg: &Message, text: &str) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(error_embed(text)))
            .await;
    }

    /// Yellow "Not Supported" notice used for the Rust-build-only stubs
    /// (eval/load/unload/reload/syncs/clear) and the unconfigured-logging path.
    async fn reply_unsupported(&self, ctx: &Context, msg: &Message, text: &str) {
        let embed = CreateEmbed::new()
            .title("Not Supported")
            .description(text)
            .color(colors::YELLOW)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }

    // ---- commands ---------------------------------------------------------

    /// `dev system` / `dev sysinfo` (also `sys`): a system overview embed with a
    /// SystemView button bar to page through Info / CPU / RAM.
    async fn cmd_system(&self, ctx: &Context, msg: &Message) {
        let embed = base_system_embed().await;
        let _ = msg
            .channel_id
            .send_message(
                &ctx.http,
                CreateMessage::new()
                    .embed(embed)
                    .components(vec![system_view()]),
            )
            .await;
    }

    /// `dev gitpull` / `dev pull`: run `git pull` and report the output.
    async fn cmd_gitpull(&self, ctx: &Context, msg: &Message) {
        let raw = run_git_pull();
        self.reply_embed(ctx, msg, git_embed(&raw)).await;
    }

    /// `dev sync`: Rust cogs are static, so we pull and explain that reload requires a restart.
    async fn cmd_sync(&self, ctx: &Context, msg: &Message) {
        let raw = run_git_pull();
        self.reply_embed(ctx, msg, git_embed(&raw)).await;
        let note = CreateEmbed::new()
            .title("Sync")
            .description(
                "Pulled the latest from git. Cog hot-reload (load/unload/reload) is not supported \
                 in the Rust build — restart the process to apply changes.",
            )
            .color(colors::YELLOW)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, note).await;
    }

    /// `dev servers` (also `guilds`): list every guild the bot is in.
    async fn cmd_servers(&self, ctx: &Context, msg: &Message) {
        let guild_ids = ctx.cache.guilds();
        let bot_name = ctx.cache.current_user().name.clone();
        let count = guild_ids.len();

        let mut lines = String::new();
        for gid in &guild_ids {
            // Guild guard is dropped at the end of each iteration (no await held).
            if let Some(g) = ctx.cache.guild(*gid) {
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
        self.reply_embed(ctx, msg, embed).await;
    }

    /// `dev leave <guild_id>`: make the bot leave a guild.
    async fn cmd_leave(&self, ctx: &Context, msg: &Message, rest: &str) {
        let Ok(id) = rest.trim().parse::<u64>() else {
            self.reply_error(ctx, msg, "Usage: `dev leave <guild_id>`")
                .await;
            return;
        };
        let gid = GuildId::new(id);
        // Resolve the name from cache before leaving (guard dropped before await).
        let name = ctx.cache.guild(gid).map(|g| g.name.clone());

        match gid.leave(&ctx.http).await {
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
                self.reply_embed(ctx, msg, embed).await;
            }
            Err(e) => {
                self.reply_error(ctx, msg, &format!("Failed to leave guild: {e}"))
                    .await;
            }
        }
    }

    /// `dev close` (also `end` / `stop`): stop the bot immediately. The serenity
    /// client has no graceful shutdown handle here, so we exit the process.
    async fn cmd_close(&self, ctx: &Context, msg: &Message) {
        let _ = msg.react(&ctx.http, '\u{2705}').await;
        let embed = CreateEmbed::new()
            .title("Shutting Down Bot")
            .description("Shutting down the bot...")
            .color(colors::RED)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
        tracing::warn!(
            owner = msg.author.id.get(),
            "dev close invoked; exiting process"
        );
        std::process::exit(0);
    }

    /// `dev redis <get|set|search|info|cinfo|showall>`: raw Redis access.
    /// Degrades gracefully when Redis is not connected.
    async fn cmd_redis(&self, ctx: &Context, msg: &Message, rest: &str) {
        let Some(redis) = &self.state.redis else {
            self.reply_error(ctx, msg, "Redis is not connected in this build.")
                .await;
            return;
        };
        let (action, args) = split_first(rest);
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
            Ok(embed) => self.reply_embed(ctx, msg, embed).await,
            Err(e) => self.reply_error(ctx, msg, &e).await,
        }
    }

    /// `dev logs [n]` (also `blogs`): show the last N lines of the log file.
    /// The bot logs to stdout by default; this reads `logs/benny.log` if file
    /// logging has been wired up.
    async fn cmd_logs(&self, ctx: &Context, msg: &Message, rest: &str) {
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
                self.reply_embed(ctx, msg, embed).await;
            }
            Err(_) => {
                self.reply_unsupported(
                    ctx,
                    msg,
                    &format!(
                        "File logging is not configured \u{2014} `{LOG_FILE}` does not exist. \
                         The bot currently logs to stdout; wire `tracing-appender` in `main.rs` to enable file logs."
                    ),
                )
                .await;
            }
        }
    }

    /// `dev uptime`: how long the process has been running.
    async fn cmd_uptime(&self, ctx: &Context, msg: &Message) {
        let secs = self.state.uptime_secs();
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        let _ = msg
            .channel_id
            .say(&ctx.http, format!("Uptime: {h}h {m}m {s}s"))
            .await;
    }

    /// `dev ping`: trivial liveness check.
    async fn cmd_ping(&self, ctx: &Context, msg: &Message) {
        let _ = msg.channel_id.say(&ctx.http, "Pong (dev)!").await;
    }

    /// Usage list for a bare `dev` (or an unknown subcommand).
    async fn cmd_help(&self, ctx: &Context, msg: &Message) {
        let embed = CreateEmbed::new()
            .title("Developer Commands")
            .description(
                "**System:** `dev system` / `dev sysinfo`\n\
                 **Git:** `dev gitpull`, `dev sync`\n\
                 **Servers:** `dev servers`, `dev leave <guild_id>`\n\
                 **Redis:** `dev redis <get|set|search|info|cinfo|showall>`\n\
                 **Logs:** `dev logs [n]`\n\
                 **Process:** `dev uptime`, `dev ping`, `dev close`\n\
                 **Disabled in Rust build:** `dev eval`, `dev load`/`unload`/`reload`",
            )
            .color(colors::BLACK)
            .timestamp(Timestamp::now());
        self.reply_embed(ctx, msg, embed).await;
    }
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
