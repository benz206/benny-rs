use super::Cog;
use crate::entities::{reminders, reminders_users};
use crate::state::AppState;
use crate::utils::time::parse_when;
use crate::utils::{colors, format};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set};
use serenity::all::{
    ButtonStyle, ComponentInteraction, ComponentInteractionDataKind, Context, CreateActionRow,
    CreateButton, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, Message, Timestamp,
};
use std::sync::Arc;

/// Maximum active reminders a single user may hold at once.
const MAX_REMINDERS: i64 = 10;
/// Reminder content length cap (validated before DB insertion).
const MAX_CONTENT_LEN: usize = 1000;

/// Preset durations (seconds, human label) offered by the interactive dropdown
/// when `remind` is called without a parseable time. Mirrors reminders.py's
/// `ReminderTimeDropdown`.
const PRESETS: [(i64, &str); 17] = [
    (60, "1 minute"),
    (300, "5 minutes"),
    (600, "10 minutes"),
    (900, "15 minutes"),
    (1800, "30 minutes"),
    (3600, "1 hour"),
    (7200, "2 hours"),
    (10800, "3 hours"),
    (21600, "6 hours"),
    (43200, "12 hours"),
    (86400, "1 day"),
    (172800, "2 days"),
    (259200, "3 days"),
    (604800, "1 week"),
    (1209600, "2 weeks"),
    (1814400, "3 weeks"),
    (2419200, "4 weeks"),
];

/// Pending interactive reminder, keyed by the bot's prompt message id.
#[derive(Clone)]
struct PendingReminder {
    content: String,
    chosen_time: Option<i64>,
}

pub struct RemindersCog {
    state: Arc<AppState>,
    /// message_id -> pending reminder awaiting a dropdown selection + confirm.
    pending: DashMap<u64, PendingReminder>,
}

impl RemindersCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            pending: DashMap::new(),
        })
    }
}

#[async_trait]
impl Cog for RemindersCog {
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
        let mut it = body.split_whitespace();
        let Some(cmd) = it.next() else { return };
        let args: Vec<&str> = it.collect();

        match cmd {
            "remind" | "remindme" => self.cmd_create(ctx, msg, &args.join(" ")).await,
            "reminder" => match args.first().copied() {
                Some("list") => self.cmd_list(ctx, msg).await,
                Some("delete") | Some("del") | Some("remove") => {
                    self.cmd_delete(ctx, msg, args.get(1).copied()).await
                }
                // `reminder <time> <text>` is also a create shortcut.
                _ => self.cmd_create(ctx, msg, &args.join(" ")).await,
            },
            "reminders" => match args.first().copied() {
                Some("list") => self.cmd_list(ctx, msg).await,
                Some("delete") | Some("del") | Some("remove") => {
                    self.cmd_delete(ctx, msg, args.get(1).copied()).await
                }
                _ => {
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            "Usage: `reminders list` | `reminders delete <id>`",
                        )
                        .await;
                }
            },
            _ => {}
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with("rem:") {
            return;
        }
        let msg_id = interaction.message.id.get();

        match cid {
            "rem:select" => {
                if let ComponentInteractionDataKind::StringSelect { values } =
                    &interaction.data.kind
                {
                    if let Some(ts) = values.first().and_then(|v| v.parse::<i64>().ok()) {
                        if let Some(mut p) = self.pending.get_mut(&msg_id) {
                            p.chosen_time = Some(ts);
                        }
                    }
                }
                // Acknowledge silently (deferred update), like view.defer() in py.
                let _ = interaction
                    .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
                    .await;
            }
            "rem:confirm" => self.on_confirm(ctx, interaction, msg_id).await,
            "rem:cancel" => self.on_cancel(ctx, interaction, msg_id).await,
            _ => {}
        }
    }
}

impl RemindersCog {
    /// Create a reminder: `remind <time> <text>`. If a leading time parses, the
    /// reminder is created directly; otherwise an interactive dropdown is shown.
    async fn cmd_create(&self, ctx: &Context, msg: &Message, args: &str) {
        let args = args.trim();
        if args.is_empty() {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Usage: `remind <time> <message>`\nExample: `remind 10m Take a break`",
                )
                .await;
            return;
        }

        let now = Utc::now();
        let (fire_at, text) = split_time_and_text(args, now);
        let text = text.trim().to_string();

        match fire_at {
            // Time parsed but nothing left for the message.
            Some(_) if text.is_empty() => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Please provide a reminder message.")
                    .await;
            }
            // Time parsed + message -> create directly.
            Some(ts) => {
                let user_id = msg.author.id.get() as i64;
                match self.insert_reminder(user_id, &text, ts).await {
                    Ok(id) => {
                        let _ = msg
                            .channel_id
                            .send_message(
                                &ctx.http,
                                CreateMessage::new().embed(created_embed(&text, ts, id)),
                            )
                            .await;
                    }
                    Err(e) => {
                        let _ = msg.channel_id.say(&ctx.http, e).await;
                    }
                }
            }
            // No parseable time -> interactive dropdown over the preset durations.
            None => self.show_interactive(ctx, msg, &text, now).await,
        }
    }

    /// Send the preview embed + duration dropdown + confirm/cancel buttons and
    /// register the pending reminder keyed by the prompt message id.
    async fn show_interactive(&self, ctx: &Context, msg: &Message, text: &str, now: DateTime<Utc>) {
        if text.len() > MAX_CONTENT_LEN {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    format!("Reminder is too long (max {MAX_CONTENT_LEN} characters)."),
                )
                .await;
            return;
        }

        let now_ts = now.timestamp();
        let options: Vec<CreateSelectMenuOption> = PRESETS
            .iter()
            .map(|(secs, label)| {
                CreateSelectMenuOption::new(format!("In {label}"), (now_ts + secs).to_string())
            })
            .collect();

        let menu = CreateSelectMenu::new("rem:select", CreateSelectMenuKind::String { options })
            .placeholder("Choose when to be reminded");
        let confirm = CreateButton::new("rem:confirm")
            .label("Confirm")
            .style(ButtonStyle::Success)
            .emoji('✅');
        let cancel = CreateButton::new("rem:cancel")
            .label("Cancel")
            .style(ButtonStyle::Danger)
            .emoji('❌');

        let preview = CreateEmbed::new()
            .title("Creating Reminder")
            .description(format!("> {text}"))
            .color(colors::GRAY)
            .footer(CreateEmbedFooter::new(
                "This is what your reminder will look like",
            ))
            .timestamp(Timestamp::now());

        let builder = CreateMessage::new().embed(preview).components(vec![
            CreateActionRow::SelectMenu(menu),
            CreateActionRow::Buttons(vec![confirm, cancel]),
        ]);

        match msg.channel_id.send_message(&ctx.http, builder).await {
            Ok(sent) => {
                self.pending.insert(
                    sent.id.get(),
                    PendingReminder {
                        content: text.to_string(),
                        chosen_time: None,
                    },
                );
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to send reminder prompt");
            }
        }
    }

    async fn on_confirm(&self, ctx: &Context, interaction: &ComponentInteraction, msg_id: u64) {
        let Some(pending) = self.pending.get(&msg_id).map(|p| p.clone()) else {
            ephemeral_error(
                ctx,
                interaction,
                "This reminder prompt has expired. Run the command again.",
            )
            .await;
            return;
        };

        let Some(ts) = pending.chosen_time else {
            ephemeral_error(
                ctx,
                interaction,
                "You need to select a time for the reminder.",
            )
            .await;
            return;
        };

        // Faithful to reminders.py: the reminder belongs to whoever confirms.
        let user_id = interaction.user.id.get() as i64;
        let response = match self.insert_reminder(user_id, &pending.content, ts).await {
            Ok(id) => CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(created_embed(&pending.content, ts, id))
                    .components(vec![]),
            ),
            Err(e) => CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(
                        CreateEmbed::new()
                            .title("Error")
                            .description(e)
                            .color(colors::RED)
                            .timestamp(Timestamp::now()),
                    )
                    .components(vec![]),
            ),
        };
        let _ = interaction.create_response(&ctx.http, response).await;
        self.pending.remove(&msg_id);
    }

    async fn on_cancel(&self, ctx: &Context, interaction: &ComponentInteraction, msg_id: u64) {
        let content = self
            .pending
            .get(&msg_id)
            .map(|p| p.content.clone())
            .unwrap_or_default();
        let embed = CreateEmbed::new()
            .title("Cancelled Reminder Creation")
            .description(format!("> {content}"))
            .color(colors::RED)
            .timestamp(Timestamp::now());
        let _ = interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(embed)
                        .components(vec![]),
                ),
            )
            .await;
        self.pending.remove(&msg_id);
    }

    /// Insert a reminder after enforcing the per-user limit and length cap, then
    /// resync the user's count (SQLite + Redis). Returns the new reminder id or a
    /// user-facing error string.
    async fn insert_reminder(
        &self,
        user_id: i64,
        content: &str,
        fire_at: i64,
    ) -> Result<i64, &'static str> {
        if content.len() > MAX_CONTENT_LEN {
            return Err("Reminder is too long (max 1000 characters).");
        }

        let count = reminders::Entity::find()
            .filter(reminders::Column::UserId.eq(user_id))
            .count(self.state.users_orm())
            .await
            .unwrap_or(0);
        if count as i64 >= MAX_REMINDERS {
            return Err("You already have 10 reminders. Delete some with `reminders delete <id>`.");
        }

        let result = reminders::Entity::insert(reminders::ActiveModel {
            user_id: Set(user_id),
            content: Set(content.to_string()),
            fire_at: Set(fire_at),
            ..Default::default()
        })
        .exec(self.state.users_orm())
        .await;

        match result {
            Ok(r) => {
                let id = r.last_insert_id;
                sync_user_count(&self.state, user_id).await;
                Ok(id)
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to create reminder");
                Err("Failed to create reminder.")
            }
        }
    }

    async fn cmd_list(&self, ctx: &Context, msg: &Message) {
        let user_id = msg.author.id.get() as i64;
        let rows = reminders::Entity::find()
            .filter(reminders::Column::UserId.eq(user_id))
            .order_by_asc(reminders::Column::FireAt)
            .all(self.state.users_orm())
            .await
            .unwrap_or_default();

        let plural = if rows.len() == 1 { "" } else { "s" };
        let mut embed = CreateEmbed::new()
            .title("Your Reminders")
            .description(format!(
                "You currently have {} reminder{plural}.",
                rows.len()
            ))
            .color(colors::BLUE)
            .timestamp(Timestamp::now());

        for row in rows {
            let (id, content, fire_at) = (row.id, row.content, row.fire_at);
            embed = embed.field(
                format!("Reminder ID: {id}"),
                format!(
                    "**Date:** <t:{fire_at}:F> (<t:{fire_at}:R>)\n**Reminder:** {}",
                    format::truncate(&content, 100)
                ),
                false,
            );
        }

        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    async fn cmd_delete(&self, ctx: &Context, msg: &Message, id_str: Option<&str>) {
        let id: i64 = match id_str.and_then(|s| s.parse().ok()) {
            Some(n) => n,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Usage: `reminders delete <id>`")
                    .await;
                return;
            }
        };
        let user_id = msg.author.id.get() as i64;

        // Look up first so we can distinguish "not found" from "not yours".
        let row = reminders::Entity::find_by_id(id)
            .one(self.state.users_orm())
            .await
            .ok()
            .flatten();

        let (owner_id, content) = match row {
            Some(m) => (m.user_id, m.content),
            None => {
                let _ = msg.channel_id.say(&ctx.http, "Reminder not found.").await;
                return;
            }
        };
        if owner_id != user_id {
            let _ = msg
                .channel_id
                .say(&ctx.http, "You do not own this reminder.")
                .await;
            return;
        }

        let result = reminders::Entity::delete_many()
            .filter(reminders::Column::Id.eq(id))
            .filter(reminders::Column::UserId.eq(user_id))
            .exec(self.state.users_orm())
            .await;

        match result {
            Ok(res) if res.rows_affected > 0 => {
                sync_user_count(&self.state, user_id).await;
                let embed = CreateEmbed::new()
                    .title("Deleted Reminder")
                    .description(format!("> {content}"))
                    .color(colors::GREEN)
                    .footer(CreateEmbedFooter::new(format!("Reminder ID: {id}")))
                    .timestamp(Timestamp::now());
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }
            Ok(_) => {
                let _ = msg.channel_id.say(&ctx.http, "Reminder not found.").await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to delete reminder");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to delete reminder.")
                    .await;
            }
        }
    }
}

/// Greedily consume a leading time expression from `args`, returning the
/// resolved fire-at timestamp (if any) and the remaining text. Picks the
/// longest leading token-prefix that `parse_when` resolves to a future instant.
fn split_time_and_text(args: &str, now: DateTime<Utc>) -> (Option<i64>, String) {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() {
        return (None, String::new());
    }

    let mut best: Option<(usize, i64)> = None;
    for k in 1..=tokens.len() {
        let candidate = tokens[..k].join(" ");
        if let Some(dt) = parse_when(&candidate, now) {
            best = Some((k, dt.timestamp()));
        }
    }

    match best {
        Some((k, ts)) => (Some(ts), tokens[k..].join(" ")),
        None => (None, args.trim().to_string()),
    }
}

/// Embed shown when a reminder is successfully created (direct or interactive).
fn created_embed(content: &str, fire_at: i64, id: i64) -> CreateEmbed {
    CreateEmbed::new()
        .title("Reminder Created")
        .description(format!(
            "> {content}\n\nFires <t:{fire_at}:F> (<t:{fire_at}:R>)"
        ))
        .color(colors::GREEN)
        .footer(CreateEmbedFooter::new(format!("Reminder ID: {id}")))
        .timestamp(Timestamp::now())
}

async fn ephemeral_error(ctx: &Context, interaction: &ComponentInteraction, text: &str) {
    let embed = CreateEmbed::new()
        .title("Error")
        .description(text)
        .color(colors::RED)
        .timestamp(Timestamp::now());
    let _ = interaction
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(embed)
                    .ephemeral(true),
            ),
        )
        .await;
}

/// Recompute a user's active-reminder count from SQLite and mirror it into both
/// `reminders_users.reminder_count` and the Redis key `reminder:count:{user_id}`
/// (Redis is skipped gracefully when unavailable). Shared with the background
/// task so fires and deletions keep the counters authoritative.
pub(crate) async fn sync_user_count(state: &AppState, user_id: i64) {
    let count = reminders::Entity::find()
        .filter(reminders::Column::UserId.eq(user_id))
        .count(state.users_orm())
        .await
        .unwrap_or(0);

    let _ = reminders_users::Entity::insert(reminders_users::ActiveModel {
        user_id: Set(user_id),
        reminder_count: Set(count as i64),
    })
    .on_conflict(
        OnConflict::column(reminders_users::Column::UserId)
            .update_columns([reminders_users::Column::ReminderCount])
            .to_owned(),
    )
    .exec(state.users_orm())
    .await;

    if let Some(redis) = &state.redis {
        let mut conn = redis.lock().await;
        let key = format!("reminder:count:{user_id}");
        let _ = redis::cmd("SET")
            .arg(&key)
            .arg(count as i64)
            .exec_async(&mut *conn)
            .await;
    }
}
