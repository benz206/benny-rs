use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct HelpCog {
    state: Arc<AppState>,
}

impl HelpCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

// (command, usage, description)
static COMMANDS: &[(&str, &str, &str)] = &[
    ("ping", "ping", "Check bot latency"),
    ("about", "about", "About this bot"),
    ("files", "files", "Count source files and lines"),
    (
        "prefix",
        "prefix add/remove/list/reset <p>",
        "Manage server prefixes",
    ),
    ("afk", "afk [reason]", "Set your AFK status"),
    (
        "remind",
        "remind <duration> <message>",
        "Set a reminder (e.g. 1h30m)",
    ),
    (
        "reminders",
        "reminders list | delete <id>",
        "Manage your reminders",
    ),
    (
        "tag",
        "tag create/edit/delete/list/info/raw <name>",
        "Manage custom tags",
    ),
    (
        "welcome",
        "welcome channel/message/enable/disable",
        "Configure welcome messages",
    ),
    (
        "goodbye",
        "goodbye channel/message/enable/disable",
        "Configure goodbye messages",
    ),
    (
        "logging",
        "logging setup/disable/test",
        "Configure event logging",
    ),
    (
        "settings",
        "settings show | reset",
        "View or reset server settings",
    ),
    (
        "blacklist",
        "blacklist add/remove <@user>",
        "Blacklist users from commands",
    ),
    ("warn", "warn <@user> [reason]", "Warn a user"),
    ("kick", "kick <@user> [reason]", "Kick a user"),
    ("ban", "ban <@user> [reason]", "Ban a user"),
    ("unban", "unban <user_id>", "Unban a user"),
    ("case", "case <number>", "View a moderation case"),
    ("cases", "cases <@user>", "View all cases for a user"),
    ("userinfo", "userinfo [@user]", "View user information"),
    ("serverinfo", "serverinfo", "View server information"),
    ("roleinfo", "roleinfo <@role>", "View role information"),
    ("avatar", "avatar [@user]", "View a user's avatar"),
    ("roleall", "roleall <@role>", "Add a role to all members"),
    (
        "translate",
        "translate [--to lang] <text>",
        "Translate text",
    ),
    ("define", "define <word>", "Look up a word definition"),
    ("ocr", "ocr [image_url]", "Extract text from an image"),
    (
        "sentinel",
        "sentinel enable/disable/threshold",
        "Configure toxicity detection",
    ),
    (
        "dev",
        "dev sysinfo/gitpull/logs",
        "Developer commands (owner only)",
    ),
    (
        "premium",
        "premium info | activate <key>",
        "View premium status",
    ),
];

#[async_trait]
impl Cog for HelpCog {
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
        let mut it = body.splitn(2, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "help" {
            return;
        }
        let arg = it.next().unwrap_or("").trim();

        if arg.is_empty() {
            self.send_help_list(ctx, msg, &prefix).await;
        } else {
            self.send_command_help(ctx, msg, arg, &prefix).await;
        }
    }
}

impl HelpCog {
    async fn send_help_list(&self, ctx: &Context, msg: &Message, prefix: &str) {
        let header = format!(
            "**Benny-rs Help** — Use `{prefix}help <command>` for details.\n\n"
        );

        // Build all lines first
        let mut all_lines: Vec<String> = Vec::with_capacity(COMMANDS.len());
        for (_cmd, usage, desc) in COMMANDS {
            all_lines.push(format!("`{prefix}{usage}` — {desc}"));
        }
        let full_body = all_lines.join("\n");
        let full_text = format!("{header}{full_body}");

        if full_text.len() <= 1900 {
            let _ = msg.channel_id.say(&ctx.http, full_text).await;
        } else {
            // Split into chunks respecting 1900 char limit
            let mut chunk = header.clone();
            for (_cmd, usage, desc) in COMMANDS {
                let line = format!("`{prefix}{usage}` — {desc}\n");
                if chunk.len() + line.len() > 1900 {
                    let _ = msg.channel_id.say(&ctx.http, &chunk).await;
                    chunk = String::new();
                }
                chunk.push_str(&line);
            }
            if !chunk.is_empty() {
                let _ = msg.channel_id.say(&ctx.http, &chunk).await;
            }
        }
    }

    async fn send_command_help(&self, ctx: &Context, msg: &Message, command: &str, prefix: &str) {
        let found = COMMANDS.iter().find(|(cmd, _, _)| *cmd == command);
        match found {
            Some((_cmd, usage, desc)) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("**`{prefix}{usage}`**\n{desc}"))
                    .await;
            }
            None => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        format!(
                            "Command `{command}` not found. Use `{prefix}help` for the full list."
                        ),
                    )
                    .await;
            }
        }
    }
}
