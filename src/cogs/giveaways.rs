//! Dyno-style button-entry giveaways: `giveaway start` posts an embed with an
//! "Enter 🎉" button, members toggle their entry via `on_component`, and a
//! background sweeper draws winners once `ends_at` passes.

use super::Cog;
use crate::entities::{giveaway_entries, giveaways};
use crate::framework::{Context, Data, Error, send_embed, send_error, send_plain};
use crate::state::AppState;
use crate::utils::colors;
use crate::utils::time::parse_when;
use async_trait::async_trait;
use chrono::Utc;
use rand::RngExt;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, DbErr, EntityTrait, QueryFilter, Set};
use serenity::all::{
    ButtonStyle, ChannelId, ComponentInteraction, CreateActionRow, CreateButton, CreateEmbed,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, EditMessage,
    Http, Timestamp,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, sleep};

/// custom_id namespace for this cog's interactive components. `on_component`
/// is fanned out to every cog, so we early-return unless the id belongs to us.
const ID_PREFIX: &str = "gw:";

/// How often the background task scans for giveaways whose `ends_at` passed.
const SWEEP_INTERVAL_SECS: u64 = 30;

pub struct GiveawaysCog {
    state: Arc<AppState>,
    /// Guards the self-spawned sweeper so gateway reconnects (which re-fire
    /// `on_ready`) do not stack duplicate loops.
    sweeper_spawned: AtomicBool,
}

impl GiveawaysCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            sweeper_spawned: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl Cog for GiveawaysCog {
    async fn on_ready(&self, ctx: &serenity::all::Context) {
        // Spawn the sweeper exactly once for the process lifetime.
        if self.sweeper_spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        spawn_sweeper_task(self.state.clone(), ctx.http.clone());
        tracing::info!("Giveaway sweeper started");
    }

    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let Some(id_str) = cid.strip_prefix("gw:enter:") else {
            return;
        };
        let Ok(giveaway_id) = id_str.parse::<i64>() else {
            return;
        };

        let found = matches!(
            giveaways::Entity::find_by_id(giveaway_id)
                .one(self.state.servers_orm())
                .await,
            Ok(Some(g)) if !g.ended
        );
        if !found {
            let _ = interaction
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content("This giveaway has ended."),
                    ),
                )
                .await;
            return;
        }

        let user_id = interaction.user.id.get() as i64;
        let insert = giveaway_entries::Entity::insert(giveaway_entries::ActiveModel {
            giveaway_id: Set(giveaway_id),
            user_id: Set(user_id),
        })
        .on_conflict(
            OnConflict::columns([
                giveaway_entries::Column::GiveawayId,
                giveaway_entries::Column::UserId,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(self.state.servers_orm())
        .await;

        let content = match insert {
            Ok(_) => "Entry confirmed! 🎉",
            Err(DbErr::RecordNotInserted) => {
                // Already entered — toggle off.
                let _ = giveaway_entries::Entity::delete_by_id((giveaway_id, user_id))
                    .exec(self.state.servers_orm())
                    .await;
                "You left the giveaway."
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to record giveaway entry");
                "Something went wrong recording your entry."
            }
        };

        let _ = interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .ephemeral(true)
                        .content(content),
                ),
            )
            .await;
    }
}

/// Draw winners for `gw`, announce them in its channel, and (unless this is a
/// reroll) mark the giveaway ended and strip the button from the original
/// message. Best-effort throughout: a failed announce/edit does not stop the
/// giveaway from being marked ended.
async fn finish_giveaway(state: &AppState, http: &Http, gw: &giveaways::Model, reroll: bool) {
    let entries = giveaway_entries::Entity::find()
        .filter(giveaway_entries::Column::GiveawayId.eq(gw.id))
        .all(state.servers_orm())
        .await
        .unwrap_or_default();
    let mut pool: Vec<i64> = entries.into_iter().map(|e| e.user_id).collect();

    let want = if reroll { 1 } else { gw.winners as usize };
    let draw_count = want.min(pool.len());
    let mut winners = Vec::with_capacity(draw_count);
    for _ in 0..draw_count {
        let i = rand::rng().random_range(0..pool.len());
        winners.push(pool.swap_remove(i));
    }
    let mentions = winners
        .iter()
        .map(|w| format!("<@{w}>"))
        .collect::<Vec<_>>()
        .join(", ");

    let channel_id = ChannelId::new(gw.channel_id as u64);
    let announcement = if winners.is_empty() {
        format!("No valid entries for **{}** — no winner.", gw.prize)
    } else {
        format!(
            "🎉 Congratulations {mentions} — you won **{}**!",
            gw.prize
        )
    };
    // Mark the giveaway ended BEFORE announcing. If we announced first and the
    // DB write then failed (or the process restarted in between), the sweeper
    // would re-run `finish_giveaway` on the next pass and draw a *fresh* random
    // set of winners — announcing a different result every 30s. Reroll must
    // leave `ended` untouched so the original giveaway stays closed.
    if !reroll {
        if let Err(e) = giveaways::Entity::update_many()
            .col_expr(giveaways::Column::Ended, Expr::value(true))
            .filter(giveaways::Column::Id.eq(gw.id))
            .exec(state.servers_orm())
            .await
        {
            tracing::error!(
                error = ?e,
                id = gw.id,
                "failed to mark giveaway ended; skipping announce to avoid re-draw",
            );
            return;
        }
    }

    let _ = channel_id
        .send_message(http, CreateMessage::new().content(announcement))
        .await;

    if reroll {
        return;
    }

    if gw.message_id != 0 {
        let embed = CreateEmbed::new()
            .title("🎉 Giveaway ended")
            .description(format!(
                "**{}**\nWinners: {}",
                gw.prize,
                if winners.is_empty() {
                    "none".to_string()
                } else {
                    mentions
                }
            ))
            .color(colors::BLURPLE);
        let _ = channel_id
            .edit_message(
                http,
                gw.message_id as u64,
                EditMessage::new().embed(embed).components(vec![]),
            )
            .await;
    }
}

/// Background sweeper: every `SWEEP_INTERVAL_SECS` it draws winners for every
/// giveaway whose `ends_at` has passed.
fn spawn_sweeper_task(state: Arc<AppState>, http: Arc<Http>) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(SWEEP_INTERVAL_SECS)).await;
            let now = Utc::now().timestamp();

            let rows = match giveaways::Entity::find()
                .filter(giveaways::Column::Ended.eq(false))
                .filter(giveaways::Column::EndsAt.lte(now))
                .all(state.servers_orm())
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = ?e, "giveaway sweep failed");
                    continue;
                }
            };

            // Normally only a few end per 30s tick, but a backlog after downtime
            // could make this a burst of send+edit calls — space them out.
            for gw in &rows {
                finish_giveaway(&state, &http, gw, false).await;
                sleep(Duration::from_millis(500)).await;
            }
        }
    });
}

// ---- commands --------------------------------------------------------------

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![giveaway()]
}

/// Run button-entry giveaways.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    aliases("gw"),
    category = "Giveaways",
    required_permissions = "MANAGE_GUILD",
    subcommand_required,
    subcommands("gw_start", "gw_end", "gw_reroll", "gw_list")
)]
async fn giveaway(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Start a giveaway with an entry button.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "start",
    category = "Giveaways",
    required_permissions = "MANAGE_GUILD"
)]
async fn gw_start(
    ctx: Context<'_>,
    #[description = "Duration, e.g. 1d, 2h30m"] duration: String,
    #[description = "Number of winners"]
    #[min = 1]
    #[max = 20]
    winners: i64,
    #[description = "Prize"]
    #[rest]
    prize: String,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;
    let sctx = ctx.serenity_context();
    let author_id = ctx.author().id.get();

    let now = Utc::now();
    let Some(ends_at) = parse_when(&duration, now).filter(|dt| *dt > now) else {
        return send_error(
            ctx,
            "Could not parse that duration. Example: `giveaway start 1d 1 Nitro`",
        )
        .await;
    };
    let ends_ts = ends_at.timestamp();

    let insert = giveaways::Entity::insert(giveaways::ActiveModel {
        guild_id: Set(guild_id.get() as i64),
        channel_id: Set(ctx.channel_id().get() as i64),
        message_id: Set(0),
        prize: Set(prize.clone()),
        winners: Set(winners),
        host_id: Set(author_id as i64),
        ends_at: Set(ends_ts),
        ended: Set(false),
        ..Default::default()
    })
    .exec(state.servers_orm())
    .await;
    let id = match insert {
        Ok(r) => r.last_insert_id,
        Err(e) => {
            tracing::error!(error = ?e, "failed to create giveaway");
            return send_error(ctx, "Failed to create the giveaway.").await;
        }
    };

    let embed = CreateEmbed::new()
        .title(format!("🎉 Giveaway: {prize}"))
        .description(format!(
            "Ends <t:{ends_ts}:R>\nWinners: **{winners}**\nHosted by <@{author_id}>"
        ))
        .color(colors::BLURPLE);
    let button = CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{ID_PREFIX}enter:{id}"))
            .label("Enter 🎉")
            .style(ButtonStyle::Primary),
    ]);

    let posted = ctx
        .channel_id()
        .send_message(
            &sctx.http,
            CreateMessage::new().embed(embed).components(vec![button]),
        )
        .await;
    let message_id = match posted {
        Ok(m) => m.id.get(),
        Err(e) => {
            tracing::error!(error = ?e, "failed to post giveaway announcement");
            return send_error(
                ctx,
                "Created the giveaway but failed to post the announcement.",
            )
            .await;
        }
    };

    let _ = giveaways::Entity::update_many()
        .col_expr(giveaways::Column::MessageId, Expr::value(message_id as i64))
        .filter(giveaways::Column::Id.eq(id))
        .exec(state.servers_orm())
        .await;

    send_plain(ctx, format!("🎉 Giveaway #{id} started for **{prize}**.")).await
}

/// End a giveaway early — winners are drawn on the next sweep.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "end",
    category = "Giveaways",
    required_permissions = "MANAGE_GUILD"
)]
async fn gw_end(
    ctx: Context<'_>,
    #[description = "Giveaway ID"] giveaway_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;

    match giveaways::Entity::find_by_id(giveaway_id)
        .one(state.servers_orm())
        .await
    {
        Ok(Some(g)) if g.guild_id == guild_id.get() as i64 && !g.ended => {
            let _ = giveaways::Entity::update_many()
                .col_expr(
                    giveaways::Column::EndsAt,
                    Expr::value(Utc::now().timestamp() - 1),
                )
                .filter(giveaways::Column::Id.eq(giveaway_id))
                .exec(state.servers_orm())
                .await;
            send_plain(ctx, format!("Ending giveaway #{giveaway_id} shortly.")).await
        }
        Ok(Some(_)) => send_error(ctx, "That giveaway has already ended.").await,
        Ok(None) => send_error(ctx, &format!("Giveaway #{giveaway_id} not found.")).await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load giveaway");
            send_error(ctx, "Failed to look up that giveaway.").await
        }
    }
}

/// Reroll a single winner for a giveaway that has already ended.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "reroll",
    category = "Giveaways",
    required_permissions = "MANAGE_GUILD"
)]
async fn gw_reroll(
    ctx: Context<'_>,
    #[description = "Giveaway ID"] giveaway_id: i64,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;
    let sctx = ctx.serenity_context();

    match giveaways::Entity::find_by_id(giveaway_id)
        .one(state.servers_orm())
        .await
    {
        Ok(Some(g)) if g.guild_id == guild_id.get() as i64 && g.ended => {
            finish_giveaway(state, &sctx.http, &g, true).await;
            send_plain(ctx, format!("Rerolled giveaway #{giveaway_id}.")).await
        }
        Ok(Some(_)) => send_error(ctx, "That giveaway hasn't ended yet.").await,
        Ok(None) => send_error(ctx, &format!("Giveaway #{giveaway_id} not found.")).await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load giveaway");
            send_error(ctx, "Failed to look up that giveaway.").await
        }
    }
}

/// List active giveaways in this server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    rename = "list",
    category = "Giveaways",
    required_permissions = "MANAGE_GUILD"
)]
async fn gw_list(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;

    let rows = match giveaways::Entity::find()
        .filter(giveaways::Column::GuildId.eq(guild_id.get() as i64))
        .filter(giveaways::Column::Ended.eq(false))
        .all(state.servers_orm())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = ?e, "failed to list giveaways");
            return send_error(ctx, "Failed to list giveaways.").await;
        }
    };

    if rows.is_empty() {
        return send_error(ctx, "No active giveaways in this server.").await;
    }

    let description = rows
        .iter()
        .map(|g| {
            format!(
                "`#{}` — **{}** in <#{}>, ends <t:{}:R>",
                g.id, g.prize, g.channel_id, g.ends_at
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let embed = CreateEmbed::new()
        .title("Active Giveaways")
        .description(description)
        .color(colors::BLURPLE)
        .timestamp(Timestamp::now());
    send_embed(ctx, embed).await
}
