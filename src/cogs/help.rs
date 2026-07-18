use super::Cog;
use crate::framework::{Context, Data, Error};
use crate::state::AppState;
use crate::utils::embeds::error_embed;
use crate::utils::interactions;
use dashmap::DashMap;
use serenity::all::{
    ButtonStyle, Colour, ComponentInteraction, ComponentInteractionDataKind,
    CreateActionRow, CreateButton, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateSelectMenu,
    CreateSelectMenuKind, CreateSelectMenuOption, ReactionType, Timestamp,
};
use std::sync::{Arc, LazyLock, OnceLock};

// ---- component ids ---------------------------------------------------------
//
// Every custom_id this cog emits shares the `help:` prefix so `on_component`
// can early-return for ids it does not own (interactions fan out to every cog).
const ID_PREFIX: &str = "help:";
const CATEGORY_SELECT_ID: &str = "help:category";
const COMMAND_SELECT_ID: &str = "help:command";
/// Nav buttons are `help:nav:<prev|next>:<category-key>`.
const NAV_PREFIX: &str = "help:nav:";

const HELP_COLOR: u32 = 0x7FDBFF;
const HELP_ICON: &str = "\u{1F4D8}"; // 📘

// ---- help index ------------------------------------------------------------
//
// The command list is generated from poise's command registry at startup (see
// `init_help_index`), so it stays in sync automatically — there is no
// hand-maintained command table. Each category's presentation (icon, colour,
// blurb) comes from the static `CATEGORY_META` map below, keyed by the poise
// `category` string the commands declare. Commands with no matching category,
// or marked `hide_in_help` (e.g. the Dev cog), are omitted.

/// Presentation metadata per category, in display order:
/// `(poise category name, url-ish key, icon, colour, blurb)`.
const CATEGORY_META: &[(&str, &str, &str, u32, &str)] = &[
    ("AFK", "afk", "\u{1F4A4}", 0x39CCCC, "Away-from-keyboard status"),
    ("Reminders", "reminders", "\u{1F397}", 0x01FF70, "Personal reminders"),
    ("Tags", "tags", "\u{1F3F7}", 0xFF851B, "Custom saved text & TagScript"),
    ("Translate", "translate", "\u{1F310}", 0xFFDC00, "Text translation"),
    ("Dictionary", "dictionary", "\u{1F4D6}", 0x85144B, "Word definitions"),
    ("OCR", "ocr", "\u{1F50E}", 0x0074D9, "Read text from images"),
    ("Moderation", "moderation", "\u{1F6E0}", 0xDDDDDD, "Keep your server in order"),
    ("Sentinel", "sentinel", "\u{1F6E1}", 0xFF4136, "Auto-moderation: toxicity & names"),
    ("Settings", "settings", "\u{2699}", 0x7FDBFF, "Server configuration"),
    ("Prefixes", "prefixes", "\u{1F4CC}", 0xF012BE, "Custom command prefixes"),
    ("Welcome", "welcome", "\u{1F44B}", 0x2ECC40, "Join/leave greetings & auto-roles"),
    ("Logging", "logging", "\u{1F4F0}", 0xB10DC9, "Server event logging"),
    ("Roles", "roles", "\u{1F3AD}", 0x3D9970, "Role assignment"),
    ("Info & Utility", "utility", "\u{1F9F1}", 0x7FDBFF, "Info cards & handy utilities"),
    ("Embed", "embed", "\u{1F4CB}", 0xAAAAAA, "Build rich embeds"),
    ("Premium", "premium", "\u{1F451}", 0x7FDBFF, "Premium / patron perks"),
    ("Music", "music", "\u{1F3B5}", 0x2ECC40, "Music playback"),
];

struct CmdInfo {
    name: String,
    aliases: Vec<String>,
    /// Usage WITHOUT the prefix, e.g. `ban <member> [reason]`.
    signature: String,
    /// One-line summary used in lists and the select dropdown.
    brief: String,
    /// Longer description shown on the command's detail page.
    help: String,
    /// Optional permission note shown on the detail page.
    notes: String,
}

struct Category {
    /// Stable, lowercase id used in component values.
    key: &'static str,
    name: &'static str,
    icon: &'static str,
    color: u32,
    description: &'static str,
    commands: Vec<CmdInfo>,
}

/// Built once from the registry in `init_help_index`; read by the `help`
/// command and the component handler.
static HELP_INDEX: OnceLock<Vec<Category>> = OnceLock::new();

/// message id -> invoking user id. Restricts navigation to the invoker; a cache
/// miss (restart / eviction) degrades to allowing anyone.
static SESSIONS: LazyLock<DashMap<u64, u64>> = LazyLock::new(DashMap::new);

/// Build the help index from the poise command registry. Call once at startup.
pub fn init_help_index(commands: &[poise::Command<Data, Error>]) {
    let mut cats: Vec<Category> = Vec::new();
    for &(cat_name, key, icon, color, description) in CATEGORY_META {
        let mut cmds = Vec::new();
        for c in commands {
            if c.hide_in_help || c.category.as_deref() != Some(cat_name) {
                continue;
            }
            cmds.push(CmdInfo {
                name: c.name.clone(),
                aliases: c.aliases.clone(),
                signature: signature_for(c),
                brief: c.description.clone().unwrap_or_default(),
                help: c
                    .help_text
                    .clone()
                    .or_else(|| c.description.clone())
                    .unwrap_or_default(),
                notes: notes_for(c),
            });
        }
        if !cmds.is_empty() {
            cats.push(Category {
                key,
                name: cat_name,
                icon,
                color,
                description,
                commands: cmds,
            });
        }
    }
    let _ = HELP_INDEX.set(cats);
}

fn categories() -> &'static [Category] {
    HELP_INDEX.get().map(Vec::as_slice).unwrap_or(&[])
}

/// `name <sub1|sub2>` for groups, else `name <required> [optional]` from params.
fn signature_for(c: &poise::Command<Data, Error>) -> String {
    if !c.subcommands.is_empty() {
        let subs = c
            .subcommands
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("|");
        return format!("{} <{subs}>", c.name);
    }
    let mut s = c.name.clone();
    for p in &c.parameters {
        if p.required {
            s.push_str(&format!(" <{}>", p.name));
        } else {
            s.push_str(&format!(" [{}]", p.name));
        }
    }
    s
}

/// "Requires: …" from the command's (and, for groups, its subcommands') perms.
fn notes_for(c: &poise::Command<Data, Error>) -> String {
    let mut perms = c.required_permissions;
    for s in &c.subcommands {
        perms |= s.required_permissions;
    }
    let names = perms.get_permission_names();
    if names.is_empty() {
        String::new()
    } else {
        format!("Requires: {}", names.join(", "))
    }
}

// ---- lookups ---------------------------------------------------------------

fn total_commands() -> usize {
    categories().iter().map(|c| c.commands.len()).sum()
}

fn category_index(key: &str) -> Option<usize> {
    categories().iter().position(|c| c.key == key)
}

fn find_category_by_key(key: &str) -> Option<&'static Category> {
    categories().iter().find(|c| c.key == key)
}

/// Resolve a `help <arg>` token to a category by key or display name.
fn find_category(q: &str) -> Option<&'static Category> {
    categories()
        .iter()
        .find(|c| c.key.eq_ignore_ascii_case(q) || c.name.eq_ignore_ascii_case(q))
}

/// Resolve a `help <arg>` token to a command by name or alias.
fn find_command(q: &str) -> Option<(&'static Category, &'static CmdInfo)> {
    for c in categories() {
        for cmd in &c.commands {
            if cmd.name.eq_ignore_ascii_case(q)
                || cmd.aliases.iter().any(|a| a.eq_ignore_ascii_case(q))
            {
                return Some((c, cmd));
            }
        }
    }
    None
}

// ---- command ---------------------------------------------------------------

/// Show the interactive help menu, or details for a category or command.
#[poise::command(slash_command, prefix_command, category = "Info & Utility")]
pub async fn help(
    ctx: Context<'_>,
    #[description = "A category or command to get details on"]
    #[rest]
    query: Option<String>,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let prefix = prefix_for(state, ctx.guild_id().map(|g| g.get()));
    let name = ctx.author().name.clone();
    let icon = ctx.author().face();
    let arg = query.unwrap_or_default();
    let arg = arg.trim();

    if arg.is_empty() {
        let embed = overview_embed(&prefix, &name, &icon);
        return send_interactive(ctx, embed, overview_components()).await;
    }

    // `help <category>` wins over `help <command>` so e.g. `help moderation`
    // lands on the category page while `help ban` opens the command page.
    if let Some(cat) = find_category(arg) {
        let embed = category_embed(cat, &prefix, &name, &icon);
        return send_interactive(ctx, embed, category_components(cat)).await;
    }
    if let Some((cat, command)) = find_command(arg) {
        let embed = command_embed(cat, command, &prefix, &name, &icon);
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    ctx.send(poise::CreateReply::default().embed(error_embed(&format!(
        "No command or category named `{arg}`. Use `{prefix}help` for the full list."
    ))))
    .await?;
    Ok(())
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![help()]
}

/// Send an interactive help message and record the invoker so navigation can be
/// restricted to them.
async fn send_interactive(
    ctx: Context<'_>,
    embed: CreateEmbed,
    rows: Vec<CreateActionRow>,
) -> Result<(), Error> {
    let handle = ctx
        .send(poise::CreateReply::default().embed(embed).components(rows))
        .await?;
    if let Ok(sent) = handle.message().await {
        crate::utils::cache::bounded_insert(&SESSIONS, sent.id.get(), ctx.author().id.get(), 2000);
    }
    Ok(())
}

/// Primary guild prefix (or the global default) for display in help text.
fn prefix_for(state: &AppState, guild_id: Option<u64>) -> String {
    state
        .guild_prefixes(guild_id)
        .into_iter()
        .next()
        .unwrap_or_else(|| state.prefix().to_string())
}

// ---- cog (component navigation only) ---------------------------------------

pub struct HelpCog {
    state: Arc<AppState>,
}

impl HelpCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait::async_trait]
impl Cog for HelpCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let id = interaction.data.custom_id.clone();
        if !id.starts_with(ID_PREFIX) {
            return;
        }

        // Owner check: only the invoker may drive the menu (when we still know
        // who that is). Degrade to open access on a cache miss.
        let owner = SESSIONS.get(&interaction.message.id.get()).map(|r| *r);
        if let Some(owner) = owner
            && interaction.user.id.get() != owner
        {
            interactions::respond_ephemeral_text(
                ctx,
                interaction,
                "These help controls aren't yours — run `help` yourself.",
            )
            .await;
            return;
        }

        let prefix = prefix_for(&self.state, interaction.guild_id.map(|g| g.get()));
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
                let cats = categories();
                let len = cats.len();
                let nidx = if dir == "prev" {
                    (idx + len - 1) % len
                } else {
                    (idx + 1) % len
                };
                let cat = &cats[nidx];
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

// ---- component builders ----------------------------------------------------

fn selected_value(interaction: &ComponentInteraction) -> Option<String> {
    match &interaction.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => values.first().cloned(),
        _ => None,
    }
}

/// The category dropdown, present in every view so the user can always jump.
fn category_select() -> CreateActionRow {
    let options: Vec<CreateSelectMenuOption> = categories()
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
            CreateSelectMenuOption::new(&c.name, format!("{}:{}", cat.key, c.name))
                .description(truncate(&c.brief, 100))
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
    for c in categories() {
        listing.push_str(&format!("{} **{}** — {}\n", c.icon, c.name, c.description));
    }

    let description = format!(
        "Welcome to Benny's help page — find information about all of Benny's commands here!\n\n\
         Benny currently has **{}** commands across **{}** categories.\n\n\
         Use the dropdown below to browse a category, or `{prefix}help <category>` / `{prefix}help <command>` for details.",
        total_commands(),
        categories().len()
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
        for c in &cat.commands {
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
fn command_embed(cat: &Category, cmd: &CmdInfo, prefix: &str, name: &str, icon: &str) -> CreateEmbed {
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
        embed = embed.field("Notes", &cmd.notes, true);
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
