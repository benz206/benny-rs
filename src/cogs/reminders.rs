use super::Cog;
use crate::entities::{reminders, reminders_users};
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::time::parse_when;
use crate::utils::{colors, format};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set};
use serenity::all::{
    ButtonStyle, ComponentInteraction, ComponentInteractionDataKind, CreateActionRow,
    CreateButton, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, Timestamp,
};
use std::sync::{Arc, LazyLock};

/// Maximum active reminders a single user may hold at once.
const MAX_REMINDERS: i64 = 10;
/// Reminder content length cap (validated before DB insertion).
const MAX_CONTENT_LEN: usize = 1000;

/// Preset durations (seconds, human label) offered by the interactive dropdown
/// when `remind` is called without a parseable time.
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
    /// The user who created the prompt — only they may drive its controls.
    owner_id: u64,
}

/// Module-level session map shared between command fns and the `on_component` hook.
static PENDING: LazyLock<DashMap<u64, PendingReminder>> = LazyLock::new(DashMap::new);

pub struct RemindersCog {
    state: Arc<AppState>,
}

impl RemindersCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for RemindersCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with("rem:") {
            return;
        }
        let msg_id = interaction.message.id.get();

        // Only the user who created the prompt may drive its controls — otherwise
        // anyone could confirm or cancel someone else's pending reminder. (`.map`
        // drops the DashMap guard before any await.)
        if let Some(owner_id) = PENDING.get(&msg_id).map(|p| p.owner_id)
            && owner_id != interaction.user.id.get()
        {
            ephemeral_error(ctx, interaction, "This isn't your reminder prompt.").await;
            return;
        }

        match cid {
            "rem:select" => {
                if let ComponentInteractionDataKind::StringSelect { values } =
                    &interaction.data.kind
                {
                    if let Some(ts) = values.first().and_then(|v| v.parse::<i64>().ok()) {
                        if let Some(mut p) = PENDING.get_mut(&msg_id) {
                            p.chosen_time = Some(ts);
                        }
                    }
                }
                // Acknowledge silently (deferred update).
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
    async fn on_confirm(
        &self,
        ctx: &serenity::all::Context,
        interaction: &ComponentInteraction,
        msg_id: u64,
    ) {
        let Some(pending) = PENDING.get(&msg_id).map(|p| p.clone()) else {
            ephemeral_error(
                ctx,
                interaction,
                "This reminder prompt has expired. Run the command again.",
            )
            .await;
            return;
        };

        let Some(ts) = pending.chosen_time else {
            ephemeral_error(ctx, interaction, "You need to select a time for the reminder.").await;
            return;
        };

        // The reminder belongs to whoever confirms.
        let user_id = interaction.user.id.get() as i64;
        let response = match insert_reminder(&self.state, user_id, &pending.content, ts).await {
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
        PENDING.remove(&msg_id);
    }

    async fn on_cancel(
        &self,
        ctx: &serenity::all::Context,
        interaction: &ComponentInteraction,
        msg_id: u64,
    ) {
        let content = PENDING
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
        PENDING.remove(&msg_id);
    }
}

/// The reminders command surface (prefix + slash).
pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![remind(), reminders()]
}

// ---- commands --------------------------------------------------------------

/// Set a reminder. If the time can't be parsed a time-picker is shown instead.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Reminders",
    aliases("remindme")
)]
async fn remind(
    ctx: Context<'_>,
    #[description = "When, e.g. 1h30m"] when: String,
    #[description = "Message"]
    #[rest]
    message: String,
) -> Result<(), Error> {
    let now = Utc::now();
    let state = &ctx.data().state;
    let user_id = ctx.author().id.get() as i64;
    let text = message.trim().to_string();

    if text.is_empty() {
        return send_error(ctx, "Please provide a reminder message.").await;
    }

    match parse_when(&when, now).filter(|dt| *dt > now) {
        Some(dt) => {
            let ts = dt.timestamp();
            match insert_reminder(state, user_id, &text, ts).await {
                Ok(id) => send_embed(ctx, created_embed(&text, ts, id)).await,
                Err(e) => send_error(ctx, e).await,
            }
        }
        None => show_interactive(ctx, &text, now).await,
    }
}

/// List and manage your reminders.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Reminders",
    aliases("reminder"),
    subcommands("reminders_list", "reminders_delete")
)]
async fn reminders(ctx: Context<'_>) -> Result<(), Error> {
    list_user_reminders(ctx).await
}

/// List all your active reminders.
#[poise::command(slash_command, prefix_command, rename = "list")]
async fn reminders_list(ctx: Context<'_>) -> Result<(), Error> {
    list_user_reminders(ctx).await
}

/// Delete one of your reminders by id.
#[poise::command(slash_command, prefix_command, rename = "delete")]
async fn reminders_delete(
    ctx: Context<'_>,
    #[description = "Reminder id"] id: i64,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get() as i64;
    let state = &ctx.data().state;

    // Look up first so we can distinguish "not found" from "not yours".
    let row = reminders::Entity::find_by_id(id)
        .one(state.users_orm())
        .await
        .ok()
        .flatten();

    let (owner_id, content) = match row {
        Some(m) => (m.user_id, m.content),
        None => return send_error(ctx, "Reminder not found.").await,
    };
    if owner_id != user_id {
        return send_error(ctx, "You do not own this reminder.").await;
    }

    let result = reminders::Entity::delete_many()
        .filter(reminders::Column::Id.eq(id))
        .filter(reminders::Column::UserId.eq(user_id))
        .exec(state.users_orm())
        .await;

    match result {
        Ok(res) if res.rows_affected > 0 => {
            sync_user_count(state, user_id).await;
            let embed = CreateEmbed::new()
                .title("Deleted Reminder")
                .description(format!("> {content}"))
                .color(colors::GREEN)
                .footer(CreateEmbedFooter::new(format!("Reminder ID: {id}")))
                .timestamp(Timestamp::now());
            send_embed(ctx, embed).await
        }
        Ok(_) => send_error(ctx, "Reminder not found.").await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to delete reminder");
            send_error(ctx, "Failed to delete reminder.").await
        }
    }
}

// ---- shared helpers --------------------------------------------------------

/// Fetch and display the caller's active reminders. Shared between the
/// `reminders` parent body and the `reminders list` subcommand.
async fn list_user_reminders(ctx: Context<'_>) -> Result<(), Error> {
    let user_id = ctx.author().id.get() as i64;
    let state = &ctx.data().state;
    let rows = reminders::Entity::find()
        .filter(reminders::Column::UserId.eq(user_id))
        .order_by_asc(reminders::Column::FireAt)
        .all(state.users_orm())
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

    send_embed(ctx, embed).await
}

/// Send the preview embed + duration dropdown + confirm/cancel buttons and
/// register the pending reminder keyed by the prompt message id.
async fn show_interactive(ctx: Context<'_>, text: &str, now: DateTime<Utc>) -> Result<(), Error> {
    if text.len() > MAX_CONTENT_LEN {
        return send_error(
            ctx,
            &format!("Reminder is too long (max {MAX_CONTENT_LEN} characters)."),
        )
        .await;
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

    let rows = vec![
        CreateActionRow::SelectMenu(menu),
        CreateActionRow::Buttons(vec![confirm, cancel]),
    ];

    let handle = ctx
        .send(poise::CreateReply::default().embed(preview).components(rows))
        .await?;
    let sent = handle.message().await?;
    crate::utils::cache::bounded_insert(
        &PENDING,
        sent.id.get(),
        PendingReminder {
            content: text.to_string(),
            chosen_time: None,
            owner_id: ctx.author().id.get(),
        },
        1000,
    );
    Ok(())
}

/// Insert a reminder after enforcing the per-user limit and length cap, then
/// resync the user's count (SQLite + Redis). Returns the new reminder id or a
/// user-facing error string.
async fn insert_reminder(
    state: &AppState,
    user_id: i64,
    content: &str,
    fire_at: i64,
) -> Result<i64, &'static str> {
    if content.len() > MAX_CONTENT_LEN {
        return Err("Reminder is too long (max 1000 characters).");
    }

    let count = reminders::Entity::find()
        .filter(reminders::Column::UserId.eq(user_id))
        .count(state.users_orm())
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
    .exec(state.users_orm())
    .await;

    match result {
        Ok(r) => {
            let id = r.last_insert_id;
            sync_user_count(state, user_id).await;
            Ok(id)
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to create reminder");
            Err("Failed to create reminder.")
        }
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

async fn ephemeral_error(
    ctx: &serenity::all::Context,
    interaction: &ComponentInteraction,
    text: &str,
) {
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
