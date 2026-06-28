use super::Cog;
use crate::state::{AppState, CommandInvocation};
use crate::utils::embeds::error_embed;
use async_trait::async_trait;
use dashmap::DashMap;
use serenity::all::{
    ButtonStyle, ChannelId, Colour, ComponentInteraction, ComponentInteractionDataKind, Context,
    CreateActionRow, CreateButton, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu,
    CreateSelectMenuKind, CreateSelectMenuOption, Message, ReactionType, Timestamp,
};
use std::sync::Arc;

// ---- component ids ---------------------------------------------------------
//
// Every custom_id this cog emits shares the `help:` prefix so `on_component`
// can early-return for ids it does not own (the trait fans interactions out to
// every cog).
const ID_PREFIX: &str = "help:";
const CATEGORY_SELECT_ID: &str = "help:category";
const COMMAND_SELECT_ID: &str = "help:command";
/// Nav buttons are `help:nav:<prev|next>:<category-key>`.
const NAV_PREFIX: &str = "help:nav:";

const HELP_COLOR: u32 = 0x7FDBFF;
const HELP_ICON: &str = "\u{1F4D8}"; // 📘

// ---- static command table --------------------------------------------------
//
// There is no central command registry, so the help menu is driven by this
// hand-maintained table. Categories correspond to the cogs under `src/cogs/`
// (internal Dev/Events cogs are excluded). Each category carries an ICON + COLOR.

struct CmdInfo {
    name: &'static str,
    aliases: &'static [&'static str],
    /// Usage WITHOUT the prefix, e.g. `ban <@member> [days] [reason]`.
    signature: &'static str,
    /// One-line summary used in lists and the select dropdown.
    brief: &'static str,
    /// Longer description shown on the command's detail page.
    help: &'static str,
    /// Optional permission / cooldown note shown on the detail page.
    notes: &'static str,
}

struct Category {
    /// Stable, lowercase id used in component values; kept distinct from
    /// command names so `help <arg>` resolution is unambiguous.
    key: &'static str,
    name: &'static str,
    icon: &'static str,
    color: u32,
    description: &'static str,
    commands: &'static [CmdInfo],
}

static CATEGORIES: &[Category] = &[
    Category {
        key: "afk",
        name: "AFK",
        icon: "\u{1F4A4}", // 💤
        color: 0x39CCCC,
        description: "Away-from-keyboard status",
        commands: &[CmdInfo {
            name: "afk",
            aliases: &[],
            signature: "afk [reason]",
            brief: "Set your AFK status",
            help: "Marks you as AFK. When someone mentions you the bot replies with your message; your AFK clears automatically the next time you speak.",
            notes: "",
        }],
    },
    Category {
        key: "reminders",
        name: "Reminders",
        icon: "\u{1F397}", // 🎗️
        color: 0x01FF70,
        description: "Personal reminders",
        commands: &[
            CmdInfo {
                name: "remind",
                aliases: &["remindme"],
                signature: "remind <when> <message>",
                brief: "Set a reminder (e.g. 1h30m)",
                help: "Schedules a reminder. After the given delay the bot DMs you your message.",
                notes: "",
            },
            CmdInfo {
                name: "reminders",
                aliases: &["reminder"],
                signature: "reminders [list | delete <id>]",
                brief: "List or delete your reminders",
                help: "Lists your pending reminders or deletes one by id.",
                notes: "",
            },
        ],
    },
    Category {
        key: "tags",
        name: "Tags",
        icon: "\u{1F3F7}", // 🏷️
        color: 0xFF851B,
        description: "Custom saved text & TagScript",
        commands: &[
            CmdInfo {
                name: "tag",
                aliases: &[],
                signature: "tag <create|edit|delete|list|info|raw> [name]",
                brief: "Manage custom tags",
                help: "Create, edit, and invoke custom tags. Tag content supports TagScript for dynamic output. Invoke a tag by name with the prefix.",
                notes: "",
            },
            CmdInfo {
                name: "tagtest",
                aliases: &["tt", "playground", "testtag"],
                signature: "tagtest <tagscript>",
                brief: "Test TagScript without saving",
                help: "Runs a TagScript snippet and shows the rendered output without creating a tag.",
                notes: "",
            },
        ],
    },
    Category {
        key: "translate",
        name: "Translate",
        icon: "\u{1F310}", // 🌐
        color: 0xFFDC00,
        description: "Text translation",
        commands: &[CmdInfo {
            name: "translate",
            aliases: &["trans"],
            signature: "translate [--to <lang>] <text>",
            brief: "Translate text (default: English)",
            help: "Translates the given text. Defaults to English; pass `--to <lang>` to choose a target language.",
            notes: "",
        }],
    },
    Category {
        key: "dictionary",
        name: "Dictionary",
        icon: "\u{1F4D6}", // 📖
        color: 0x85144B,
        description: "Word definitions",
        commands: &[CmdInfo {
            name: "define",
            aliases: &["dict", "def"],
            signature: "define <word>",
            brief: "Look up a word definition",
            help: "Looks up a word and shows its meanings; pick a part of speech from the dropdown to view each definition.",
            notes: "",
        }],
    },
    Category {
        key: "ocr",
        name: "OCR",
        icon: "\u{1F50E}", // 🔎
        color: 0x0074D9,
        description: "Read text from images",
        commands: &[CmdInfo {
            name: "ocr",
            aliases: &["imgread", "read"],
            signature: "ocr [image_url]",
            brief: "Extract text from an image",
            help: "Runs OCR on an attached image or the given URL and returns the extracted text.",
            notes: "",
        }],
    },
    Category {
        key: "moderation",
        name: "Moderation",
        icon: "\u{1F6E0}", // 🛠️
        color: 0xDDDDDD,
        description: "Keep your server in order",
        commands: &[
            CmdInfo {
                name: "warn",
                aliases: &[],
                signature: "warn <@member> [reason]",
                brief: "Warn a member",
                help: "Issues a warning and records a moderation case.",
                notes: "Requires: Moderate Members",
            },
            CmdInfo {
                name: "kick",
                aliases: &[],
                signature: "kick <@member> [reason]",
                brief: "Kick a member",
                help: "Removes a member from the server.",
                notes: "Requires: Kick Members",
            },
            CmdInfo {
                name: "ban",
                aliases: &[],
                signature: "ban <@member|id> [days] [reason]",
                brief: "Ban a member",
                help: "Bans a user and optionally purges 0-7 days of their recent messages.",
                notes: "Requires: Ban Members",
            },
            CmdInfo {
                name: "unban",
                aliases: &[],
                signature: "unban <user_id> [reason]",
                brief: "Unban a user",
                help: "Lifts a ban for the given user id.",
                notes: "Requires: Ban Members",
            },
            CmdInfo {
                name: "mute",
                aliases: &[],
                signature: "mute <@member> <duration> [reason]",
                brief: "Temporarily mute a member",
                help: "Applies the Muted role for the given duration, then lifts it automatically.",
                notes: "Requires: Moderate Members",
            },
            CmdInfo {
                name: "unmute",
                aliases: &[],
                signature: "unmute <@member> [reason]",
                brief: "Unmute a member",
                help: "Removes the Muted role from a member.",
                notes: "Requires: Moderate Members",
            },
            CmdInfo {
                name: "case",
                aliases: &[],
                signature: "case <number>",
                brief: "View a moderation case",
                help: "Shows the details of a single moderation case by number.",
                notes: "",
            },
            CmdInfo {
                name: "cases",
                aliases: &[],
                signature: "cases <@member>",
                brief: "View a member's cases",
                help: "Lists all moderation cases recorded for a member.",
                notes: "",
            },
            CmdInfo {
                name: "modlog",
                aliases: &[],
                signature: "modlog",
                brief: "View recent moderation actions",
                help: "Shows the most recent moderation actions in this server.",
                notes: "",
            },
        ],
    },
    Category {
        key: "sentinel",
        name: "Sentinel",
        icon: "\u{1F6E1}", // 🛡️
        color: 0xFF4136,
        description: "Auto-moderation: toxicity & names",
        commands: &[
            CmdInfo {
                name: "sentinel",
                aliases: &[],
                signature: "sentinel <enable|disable|channel|threshold|delete|config>",
                brief: "Configure toxicity detection",
                help: "Configures the toxicity auto-moderator. Set the log channel, per-attribute thresholds, and whether flagged messages are deleted.",
                notes: "Requires: Manage Server",
            },
            CmdInfo {
                name: "decancer",
                aliases: &[],
                signature: "decancer <enable|disable|logs|user>",
                brief: "Clean unreadable nicknames",
                help: "Normalizes hard-to-read display names to readable characters, on join and on demand.",
                notes: "Requires: Manage Server",
            },
        ],
    },
    Category {
        key: "settings",
        name: "Settings",
        icon: "\u{2699}", // ⚙️
        color: 0x7FDBFF,
        description: "Server configuration",
        commands: &[
            CmdInfo {
                name: "settings",
                aliases: &[],
                signature: "settings <show|reset|timezone>",
                brief: "View or change server settings",
                help: "Shows the current server settings, resets them, or sets the server timezone.",
                notes: "Requires: Manage Server",
            },
            CmdInfo {
                name: "blacklist",
                aliases: &[],
                signature: "blacklist <add|remove> <@user>",
                brief: "Block users from using the bot",
                help: "Adds or removes a user from the command blacklist.",
                notes: "Requires: Manage Server",
            },
        ],
    },
    Category {
        key: "prefixes",
        name: "Prefixes",
        icon: "\u{1F4CC}", // 📌
        color: 0xF012BE,
        description: "Custom command prefixes",
        commands: &[CmdInfo {
            name: "prefix",
            aliases: &[],
            signature: "prefix <add|remove|list|reset> [prefix]",
            brief: "Manage server prefixes",
            help: "Add, remove, list, or reset the prefixes the bot responds to in this server.",
            notes: "Requires: Manage Server to modify",
        }],
    },
    Category {
        key: "welcome",
        name: "Welcome",
        icon: "\u{1F44B}", // 👋
        color: 0x2ECC40,
        description: "Join/leave greetings & auto-roles",
        commands: &[
            CmdInfo {
                name: "welcome",
                aliases: &["welc"],
                signature: "welcome <setup|channel|message|embed|enable|disable>",
                brief: "Configure welcome messages",
                help: "Configures the greeting sent when a member joins. Messages support TagScript placeholders.",
                notes: "Requires: Manage Server",
            },
            CmdInfo {
                name: "goodbye",
                aliases: &["leave"],
                signature: "goodbye <setup|channel|message|embed|enable|disable>",
                brief: "Configure goodbye messages",
                help: "Configures the message sent when a member leaves.",
                notes: "Requires: Manage Server",
            },
            CmdInfo {
                name: "autorole",
                aliases: &["autoroles"],
                signature: "autorole <set|add|remove|list|clear>",
                brief: "Auto-assign roles on join",
                help: "Manages the roles automatically granted to members when they join.",
                notes: "Requires: Manage Roles",
            },
            CmdInfo {
                name: "stickyrole",
                aliases: &["stickyroles"],
                signature: "stickyrole [enable|disable]",
                brief: "Persist roles across rejoins",
                help: "When enabled, a member's roles are restored if they leave and rejoin.",
                notes: "Requires: Manage Roles",
            },
        ],
    },
    Category {
        key: "logging",
        name: "Logging",
        icon: "\u{1F4F0}", // 📰
        color: 0xB10DC9,
        description: "Server event logging",
        commands: &[CmdInfo {
            name: "logging",
            aliases: &[],
            signature: "logging <setup|disable|test>",
            brief: "Configure event logging",
            help: "Streams an audit log of server events to a webhook. Set it up with a webhook URL, disable it, or send a test message.",
            notes: "Requires: Manage Server",
        }],
    },
    Category {
        key: "roles",
        name: "Roles",
        icon: "\u{1F3AD}", // 🎭
        color: 0x3D9970,
        description: "Role assignment",
        commands: &[
            CmdInfo {
                name: "role",
                aliases: &[],
                signature: "role <add|remove|custom|all> [args]",
                brief: "Manage member roles",
                help: "Adds or removes roles from members, creates a personal custom role, or bulk-applies roles.",
                notes: "Requires: Manage Roles",
            },
            CmdInfo {
                name: "roleall",
                aliases: &[],
                signature: "roleall [remove] <@role>",
                brief: "Add/remove a role for everyone",
                help: "Applies (or with `remove`, strips) a role for every member in the server.",
                notes: "Requires: Manage Roles",
            },
        ],
    },
    Category {
        key: "utility",
        name: "Info & Utility",
        icon: "\u{1F9F1}", // 🧱
        color: 0x7FDBFF,
        description: "Info cards & handy utilities",
        commands: &[
            CmdInfo {
                name: "ping",
                aliases: &[],
                signature: "ping",
                brief: "Check bot latency",
                help: "Reports the bot's gateway and message round-trip latency.",
                notes: "",
            },
            CmdInfo {
                name: "about",
                aliases: &[],
                signature: "about",
                brief: "About this bot",
                help: "Shows general information about the bot.",
                notes: "",
            },
            CmdInfo {
                name: "version",
                aliases: &[],
                signature: "version",
                brief: "Show version / recent commits",
                help: "Displays the running version and recent git history.",
                notes: "",
            },
            CmdInfo {
                name: "uptime",
                aliases: &[],
                signature: "uptime",
                brief: "How long the bot has been online",
                help: "Shows how long the bot has been running since its last restart.",
                notes: "",
            },
            CmdInfo {
                name: "files",
                aliases: &[],
                signature: "files [--full]",
                brief: "Count source files and lines",
                help: "Counts the bot's source files and lines of code.",
                notes: "",
            },
            CmdInfo {
                name: "invite",
                aliases: &[],
                signature: "invite",
                brief: "Get the bot invite link",
                help: "Generates an invite link for adding the bot to another server.",
                notes: "",
            },
            CmdInfo {
                name: "charinfo",
                aliases: &[],
                signature: "charinfo <characters>",
                brief: "Inspect Unicode characters",
                help: "Shows the Unicode name and code point of each character given.",
                notes: "",
            },
            CmdInfo {
                name: "perms",
                aliases: &["permissions"],
                signature: "perms [@member]",
                brief: "Show a member's permissions",
                help: "Lists the effective guild permissions of a member (defaults to you).",
                notes: "",
            },
            CmdInfo {
                name: "info",
                aliases: &["userinfo", "ui", "whois", "i"],
                signature: "info [@member]",
                brief: "Show member information",
                help: "Displays a member card: account/join dates, roles, badges and more.",
                notes: "",
            },
            CmdInfo {
                name: "serverinfo",
                aliases: &["si", "guildinfo"],
                signature: "serverinfo",
                brief: "Show server information",
                help: "Displays a card with this server's stats.",
                notes: "",
            },
            CmdInfo {
                name: "roleinfo",
                aliases: &["ri"],
                signature: "roleinfo <@role|id|name>",
                brief: "Show role information",
                help: "Displays details about a role: color, position, permissions and more.",
                notes: "",
            },
            CmdInfo {
                name: "avatar",
                aliases: &["av", "pfp"],
                signature: "avatar [@member]",
                brief: "Show a user's avatar",
                help: "Shows a user's avatar in full size (defaults to you).",
                notes: "",
            },
        ],
    },
    Category {
        key: "embed",
        name: "Embed",
        icon: "\u{1F4CB}", // 📋
        color: 0xAAAAAA,
        description: "Build rich embeds",
        commands: &[CmdInfo {
            name: "embed",
            aliases: &[],
            signature: "embed <new|title|description|color|author|footer|field|preview|send|clear>",
            brief: "Create custom embeds interactively",
            help: "Builds a custom embed step by step (title, description, color, fields, ...), previews it, then sends it to the channel.",
            notes: "Requires: Manage Messages",
        }],
    },
    Category {
        key: "premium",
        name: "Premium",
        icon: "\u{1F451}", // 👑
        color: 0x7FDBFF,
        description: "Premium / patron perks",
        commands: &[CmdInfo {
            name: "premium",
            aliases: &[],
            signature: "premium [info | activate <key>]",
            brief: "View or activate premium",
            help: "Shows your premium status, or activates a premium key.",
            notes: "",
        }],
    },
    Category {
        key: "music",
        name: "Music",
        icon: "\u{1F3B5}", // 🎵
        color: 0x2ECC40,
        description: "Music playback (coming soon)",
        commands: &[],
    },
];

// ---- lookups ---------------------------------------------------------------

fn total_commands() -> usize {
    CATEGORIES.iter().map(|c| c.commands.len()).sum()
}

fn category_index(key: &str) -> Option<usize> {
    CATEGORIES.iter().position(|c| c.key == key)
}

fn find_category_by_key(key: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.key == key)
}

/// Resolve a `help <arg>` token to a category by key or display name.
fn find_category(q: &str) -> Option<&'static Category> {
    CATEGORIES
        .iter()
        .find(|c| c.key.eq_ignore_ascii_case(q) || c.name.eq_ignore_ascii_case(q))
}

/// Resolve a `help <arg>` token to a command by name or alias.
fn find_command(q: &str) -> Option<(&'static Category, &'static CmdInfo)> {
    for c in CATEGORIES {
        for cmd in c.commands {
            if cmd.name.eq_ignore_ascii_case(q)
                || cmd.aliases.iter().any(|a| a.eq_ignore_ascii_case(q))
            {
                return Some((c, cmd));
            }
        }
    }
    None
}

// ---- cog -------------------------------------------------------------------

pub struct HelpCog {
    state: Arc<AppState>,
    /// message id -> invoking user id. Restricts navigation to the invoker
    /// A cache miss (restart / eviction) degrades to allowing anyone.
    sessions: DashMap<u64, u64>,
}

impl HelpCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            sessions: DashMap::new(),
        })
    }
}

#[async_trait]
impl Cog for HelpCog {
    async fn on_command(&self, ctx: &Context, msg: &Message, inv: &CommandInvocation<'_>) -> bool {
        if inv.command != "help" {
            return false;
        }
        let arg = inv.args;
        let prefix = inv.prefix.to_string();

        let name = msg.author.name.clone();
        let icon = msg.author.face();

        if arg.is_empty() {
            // Overview with the category dropdown.
            let embed = overview_embed(&prefix, &name, &icon);
            self.send_interactive(
                ctx,
                msg.channel_id,
                msg.author.id.get(),
                embed,
                overview_components(),
            )
            .await;
            return true;
        }

        // `help <category>` wins over `help <command>` so e.g. `help moderation`
        // lands on the category page while `help ban` opens the command page.
        if let Some(cat) = find_category(arg) {
            let embed = category_embed(cat, &prefix, &name, &icon);
            self.send_interactive(
                ctx,
                msg.channel_id,
                msg.author.id.get(),
                embed,
                category_components(cat),
            )
            .await;
            return true;
        }
        if let Some((cat, command)) = find_command(arg) {
            let embed = command_embed(cat, command, &prefix, &name, &icon);
            let _ = msg
                .channel_id
                .send_message(&ctx.http, CreateMessage::new().embed(embed))
                .await;
            return true;
        }

        let _ = msg
            .channel_id
            .send_message(
                &ctx.http,
                CreateMessage::new().embed(error_embed(&format!(
                    "No command or category named `{arg}`. Use `{prefix}help` for the full list."
                ))),
            )
            .await;
        true
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let id = interaction.data.custom_id.clone();
        if !id.starts_with(ID_PREFIX) {
            return;
        }

        // Owner check: only the invoker may drive the menu (when we still know
        // who that is). Degrade to open access on a cache miss. Copy the id out
        // so no DashMap guard is held across the await below.
        let owner = self.sessions.get(&interaction.message.id.get()).map(|r| *r);
        if let Some(owner) = owner {
            if interaction.user.id.get() != owner {
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .ephemeral(true)
                                .content("These help controls aren't yours — run `help` yourself."),
                        ),
                    )
                    .await;
                return;
            }
        }

        let prefix = self.state.prefix().to_string();
        let name = interaction.user.name.clone();
        let icon = interaction.user.face();

        // Resolve the next view to render.
        let next: Option<(CreateEmbed, Vec<CreateActionRow>)> = if id == CATEGORY_SELECT_ID {
            selected_value(interaction)
                .and_then(|v| find_category_by_key(&v))
                .map(|cat| {
                    (
                        category_embed(cat, &prefix, &name, &icon),
                        category_components(cat),
                    )
                })
        } else if id == COMMAND_SELECT_ID {
            selected_value(interaction).and_then(|v| {
                let (key, cmd_name) = v.split_once(':')?;
                let cat = find_category_by_key(key)?;
                let command = cat.commands.iter().find(|c| c.name == cmd_name)?;
                Some((
                    command_embed(cat, command, &prefix, &name, &icon),
                    // Keep the navigation controls so the user can keep browsing.
                    category_components(cat),
                ))
            })
        } else if let Some(rest) = id.strip_prefix(NAV_PREFIX) {
            // `prev:<key>` / `next:<key>` — step to the neighbouring category.
            let mut p = rest.splitn(2, ':');
            let dir = p.next().unwrap_or("");
            let key = p.next().unwrap_or("");
            category_index(key).map(|idx| {
                let len = CATEGORIES.len();
                let nidx = if dir == "prev" {
                    (idx + len - 1) % len
                } else {
                    (idx + 1) % len
                };
                let cat = &CATEGORIES[nidx];
                (
                    category_embed(cat, &prefix, &name, &icon),
                    category_components(cat),
                )
            })
        } else {
            None
        };

        let Some((embed, rows)) = next else {
            // Unknown/garbled id: ack so Discord doesn't show "interaction failed".
            let _ = interaction
                .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
                .await;
            return;
        };

        let response = CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .embed(embed)
                .components(rows),
        );
        if let Err(e) = interaction.create_response(&ctx.http, response).await {
            tracing::error!(error = ?e, "failed to update help message");
        }
    }
}

impl HelpCog {
    /// Send a help message that carries components, recording the invoker so
    /// `on_component` can restrict navigation to them.
    async fn send_interactive(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
        invoker_id: u64,
        embed: CreateEmbed,
        rows: Vec<CreateActionRow>,
    ) {
        let builder = CreateMessage::new().embed(embed).components(rows);
        match channel_id.send_message(&ctx.http, builder).await {
            Ok(sent) => {
                crate::utils::cache::bounded_insert(
                    &self.sessions,
                    sent.id.get(),
                    invoker_id,
                    2000,
                );
            }
            Err(e) => tracing::error!(error = ?e, "failed to send help message"),
        }
    }
}

// ---- component builders ----------------------------------------------------

fn selected_value(interaction: &ComponentInteraction) -> Option<String> {
    match &interaction.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => values.first().cloned(),
        _ => None,
    }
}

/// The category dropdown, present in every view so the user can always jump.
fn category_select() -> CreateActionRow {
    let options: Vec<CreateSelectMenuOption> = CATEGORIES
        .iter()
        .map(|c| {
            CreateSelectMenuOption::new(c.name, c.key)
                .description(c.description)
                .emoji(ReactionType::Unicode(c.icon.to_string()))
        })
        .collect();
    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(CATEGORY_SELECT_ID, CreateSelectMenuKind::String { options })
            .placeholder("Select a category")
            .min_values(1)
            .max_values(1),
    )
}

/// The per-category command dropdown (drill into a single command's page).
/// Returns `None` for a category with no commands (a String select must have at
/// least one option).
fn command_select(cat: &Category) -> Option<CreateActionRow> {
    if cat.commands.is_empty() {
        return None;
    }
    let options: Vec<CreateSelectMenuOption> = cat
        .commands
        .iter()
        .map(|c| {
            CreateSelectMenuOption::new(c.name, format!("{}:{}", cat.key, c.name))
                .description(truncate(c.brief, 100))
        })
        .collect();
    Some(CreateActionRow::SelectMenu(
        CreateSelectMenu::new(COMMAND_SELECT_ID, CreateSelectMenuKind::String { options })
            .placeholder("Select a command for details")
            .min_values(1)
            .max_values(1),
    ))
}

/// Previous / Next buttons that cycle through categories.
fn nav_buttons(cat: &Category) -> CreateActionRow {
    let prev = CreateButton::new(format!("{NAV_PREFIX}prev:{}", cat.key))
        .label("Previous")
        .style(ButtonStyle::Secondary)
        .emoji('\u{25C0}'); // ◀
    let next = CreateButton::new(format!("{NAV_PREFIX}next:{}", cat.key))
        .label("Next")
        .style(ButtonStyle::Secondary)
        .emoji('\u{25B6}'); // ▶
    CreateActionRow::Buttons(vec![prev, next])
}

fn overview_components() -> Vec<CreateActionRow> {
    vec![category_select()]
}

fn category_components(cat: &Category) -> Vec<CreateActionRow> {
    let mut rows = vec![category_select()];
    if let Some(cmd_row) = command_select(cat) {
        rows.push(cmd_row);
    }
    rows.push(nav_buttons(cat));
    rows
}

// ---- embed builders --------------------------------------------------------

fn author(name: &str, icon: &str) -> CreateEmbedAuthor {
    CreateEmbedAuthor::new(name.to_string()).icon_url(icon.to_string())
}

/// `help` with no args: overview listing every category, plus the dropdown.
fn overview_embed(prefix: &str, name: &str, icon: &str) -> CreateEmbed {
    let mut listing = String::new();
    for c in CATEGORIES {
        listing.push_str(&format!("{} **{}** — {}\n", c.icon, c.name, c.description));
    }

    let description = format!(
        "Welcome to Benny's help page — find information about all of Benny's commands here!\n\n\
         Benny currently has **{}** commands across **{}** categories.\n\n\
         Use the dropdown below to browse a category, or `{prefix}help <category>` / `{prefix}help <command>` for details.",
        total_commands(),
        CATEGORIES.len()
    );

    CreateEmbed::new()
        .title("Benny Help")
        .description(description)
        .color(Colour::new(HELP_COLOR))
        .author(author(name, icon))
        .field("Categories", listing, false)
        .footer(CreateEmbedFooter::new(format!("{HELP_ICON} Benny Help")))
        .timestamp(Timestamp::now())
}

/// `help <category>` / category selection: all commands in one category.
fn category_embed(cat: &Category, prefix: &str, name: &str, icon: &str) -> CreateEmbed {
    let body = if cat.commands.is_empty() {
        format!("{} No commands here yet — coming soon!", cat.icon)
    } else {
        let mut s = String::new();
        for c in cat.commands {
            s.push_str(&format!("`{prefix}{}` — {}\n", c.signature, c.brief));
        }
        s
    };

    CreateEmbed::new()
        .title(format!("{} {}", cat.icon, cat.name))
        .description(cat.description)
        .color(Colour::new(cat.color))
        .author(author(name, icon))
        .field("Commands", body, false)
        .footer(CreateEmbedFooter::new(format!(
            "Use {prefix}help <command> for full details"
        )))
        .timestamp(Timestamp::now())
}

/// `help <command>` / command selection: the detailed command page, with an
/// ANSI-coloured usage block.
fn command_embed(
    cat: &Category,
    cmd: &CmdInfo,
    prefix: &str,
    name: &str,
    icon: &str,
) -> CreateEmbed {
    let grey = esc("30");
    let wb = esc("1;37");
    let blue_ul = esc("4;34");
    let pink_ul = esc("4;35");
    let red_bul = esc("1;4;31");
    let cyan_ul = esc("4;36");
    let reset = esc("0");

    // Legend: prefix command_name <Required> [Optional]
    let legend = format!(
        "{grey}prefix{wb}command_name {wb}<{blue_ul}Required{wb}>{reset} {wb}[{pink_ul}Optional{wb}]{reset}"
    );

    // Colour the real signature in a single pass (chained `replace` would corrupt
    // the `[` inside the ANSI escapes themselves).
    let mut colored = format!("{grey}{prefix}{wb}");
    for ch in cmd.signature.chars() {
        match ch {
            '<' => {
                colored.push_str(&wb);
                colored.push('<');
                colored.push_str(&blue_ul);
            }
            '>' => {
                colored.push_str(&wb);
                colored.push('>');
                colored.push_str(&reset);
            }
            '[' => {
                colored.push_str(&wb);
                colored.push('[');
                colored.push_str(&pink_ul);
            }
            ']' => {
                colored.push_str(&wb);
                colored.push(']');
                colored.push_str(&reset);
            }
            c => colored.push(c),
        }
    }
    colored.push_str(&reset);

    let alias_text = if cmd.aliases.is_empty() {
        "No Aliases".to_string()
    } else {
        cmd.aliases.join(", ")
    };

    let description = format!(
        "{help}\n```ansi\n{red_bul}Usage{reset}\n{legend}\n{colored}\n\n{red_bul}Aliases{reset}\n{cyan_ul}{alias_text}{reset}\n```",
        help = cmd.help
    );

    let mut embed = CreateEmbed::new()
        .title(format!("{prefix}{}", cmd.signature))
        .description(description)
        .color(Colour::new(cat.color))
        .author(author(name, icon))
        .field("Category", format!("{} {}", cat.icon, cat.name), true)
        .timestamp(Timestamp::now());
    if !cmd.notes.is_empty() {
        embed = embed.field("Notes", cmd.notes, true);
    }
    embed
}

// ---- small helpers ---------------------------------------------------------

/// One ANSI SGR escape for the Discord ```ansi code block.
fn esc(codes: &str) -> String {
    format!("\u{1b}[{codes}m")
}

/// Truncate to at most `max` chars (Discord select descriptions cap at 100).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
    }
}
