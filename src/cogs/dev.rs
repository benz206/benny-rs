use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;
use sysinfo::System;

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
        if msg.author.bot { return; }
        // Owner-only check
        if !self.state.is_owner(msg.author.id.get()) { return; }

        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) { return; }
        let body = content[prefix.len()..].trim();
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "dev" { return; }
        let subcmd = it.next().unwrap_or("").trim();
        let _arg = it.next().unwrap_or("").trim();

        match subcmd {
            "sysinfo" | "sys" => self.cmd_sysinfo(ctx, msg).await,
            "gitpull" | "pull" => self.cmd_gitpull(ctx, msg).await,
            "uptime" => {
                let secs = self.state.uptime_secs();
                let h = secs / 3600;
                let m = (secs % 3600) / 60;
                let s = secs % 60;
                let _ = msg.channel_id.say(&ctx.http, format!("Uptime: {h}h {m}m {s}s")).await;
            }
            "ping" => {
                let _ = msg.channel_id.say(&ctx.http, "Pong (dev)!").await;
            }
            _ => {
                let _ = msg.channel_id.say(
                    &ctx.http,
                    "Dev commands: `dev sysinfo` | `dev gitpull` | `dev uptime` | `dev ping`"
                ).await;
            }
        }
    }
}

impl DevCog {
    async fn cmd_sysinfo(&self, ctx: &Context, msg: &Message) {
        let mut sys = System::new_all();
        sys.refresh_all();

        let cpu_usage: f32 = sys.global_cpu_usage();
        let total_mem = sys.total_memory() / 1024 / 1024; // MB
        let used_mem = sys.used_memory() / 1024 / 1024;   // MB
        let os_name = System::long_os_version().unwrap_or_else(|| "Unknown".to_string());
        let hostname = System::host_name().unwrap_or_else(|| "Unknown".to_string());

        let uptime_secs = self.state.uptime_secs();
        let h = uptime_secs / 3600;
        let m = (uptime_secs % 3600) / 60;

        let text = format!(
            "**System Info**\n\
            **OS:** {os_name}\n\
            **Hostname:** {hostname}\n\
            **CPU Usage:** {cpu_usage:.1}%\n\
            **Memory:** {used_mem} MB / {total_mem} MB\n\
            **Bot Uptime:** {h}h {m}m"
        );
        let _ = msg.channel_id.say(&ctx.http, text).await;
    }

    async fn cmd_gitpull(&self, ctx: &Context, msg: &Message) {
        let _ = msg.channel_id.say(&ctx.http, "Running git pull...").await;
        match std::process::Command::new("git")
            .arg("pull")
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let result = if !stdout.is_empty() { stdout.to_string() } else { stderr.to_string() };
                let result = result.trim().to_string();
                let result = if result.len() > 1800 { result[..1800].to_string() } else { result };
                let _ = msg.channel_id.say(&ctx.http, format!("```\n{result}\n```")).await;
            }
            Err(e) => {
                let _ = msg.channel_id.say(&ctx.http, format!("Failed to run git pull: {e}")).await;
            }
        }
    }
}
