use super::Cog;
use crate::entities::afk;
use crate::framework::{Context, Data, Error, send_embed};
use crate::state::{AfkEntry, AppState};
use crate::utils::format::humanize_duration;
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serenity::all::{
    Colour, CreateEmbed, CreateEmbedFooter, CreateMessage, Message, Timestamp,
};
use std::sync::Arc;
use std::time::Duration;

const AQUA: Colour = Colour::from_rgb(0x7F, 0xDB, 0xFF); // 0x7FDBFF
const PINK: Colour = Colour::from_rgb(0xF0, 0x12, 0xBE); // 0xF012BE

const DEFAULT_AFK_MESSAGE: &str = "I'm currently AFK.";

pub struct AfkCog {
    state: Arc<AppState>,
}

impl AfkCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for AfkCog {
    async fn on_ready(&self, _ctx: &serenity::all::Context) {
        let rows = afk::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        for m in rows {
            self.state.afk_cache.insert(
                (m.guild_id as u64, m.user_id as u64),
                AfkEntry {
                    message: m.message,
                    set_at: m.set_at,
                },
            );
        }
        tracing::info!("AFK cache loaded ({} entries)", self.state.afk_cache.len());
    }

    async fn on_message(&self, ctx: &serenity::all::Context, msg: &Message) {
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let now = Utc::now().timestamp();
        // Run on every non-bot message: clear the author's own AFK and announce
        // any mentioned AFK users.
        self.manage_afk(ctx, msg, guild_id, now).await;
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![afk()]
}

// ---- commands --------------------------------------------------------------

/// Set yourself as AFK with an optional reason.
#[poise::command(slash_command, prefix_command, guild_only, category = "AFK")]
async fn afk(
    ctx: Context<'_>,
    #[description = "AFK reason"]
    #[rest]
    reason: Option<String>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let state = &ctx.data().state;
    let author = ctx.author();
    let user_id = author.id.get();
    let message = reason.unwrap_or_else(|| DEFAULT_AFK_MESSAGE.to_string());
    let set_at = Utc::now().timestamp();

    state.afk_cache.insert(
        (guild_id.get(), user_id),
        AfkEntry {
            message: message.clone(),
            set_at,
        },
    );
    let _ = afk::Entity::insert(afk::ActiveModel {
        guild_id: Set(guild_id.get() as i64),
        user_id: Set(user_id as i64),
        message: Set(message.clone()),
        set_at: Set(set_at),
    })
    .on_conflict(
        OnConflict::columns([afk::Column::GuildId, afk::Column::UserId])
            .update_columns([afk::Column::Message, afk::Column::SetAt])
            .to_owned(),
    )
    .exec(state.servers_orm())
    .await;

    let avatar = author
        .avatar_url()
        .unwrap_or_else(|| author.default_avatar_url());
    let embed = CreateEmbed::new()
        .title("Set AFK")
        .description(format!(">>> {message}"))
        .color(AQUA)
        .timestamp(Timestamp::now())
        .footer(
            CreateEmbedFooter::new("To remove this AFK send a message anywhere")
                .icon_url(avatar),
        );
    send_embed(ctx, embed).await
}

// ---- cog helpers -----------------------------------------------------------

impl AfkCog {
    async fn manage_afk(
        &self,
        ctx: &serenity::all::Context,
        msg: &Message,
        guild_id: u64,
        now: i64,
    ) {
        let user_id = msg.author.id.get();

        // Author sent a message → clear their AFK if >3s have elapsed since it was set.
        let own = self
            .state
            .afk_cache
            .get(&(guild_id, user_id))
            .map(|e| e.clone());
        if let Some(entry) = own {
            if entry.set_at + 3 < now {
                self.state.afk_cache.remove(&(guild_id, user_id));
                let _ = afk::Entity::delete_many()
                    .filter(afk::Column::GuildId.eq(guild_id as i64))
                    .filter(afk::Column::UserId.eq(user_id as i64))
                    .exec(self.state.servers_orm())
                    .await;

                let dur =
                    humanize_duration(Duration::from_secs((now - entry.set_at).max(0) as u64));
                let embed = CreateEmbed::new()
                    .title("Removed AFK")
                    .description(format!(
                        "Welcome back <@{user_id}>!\n\nYou've been AFK for {dur}."
                    ))
                    .color(PINK)
                    .timestamp(Timestamp::now());
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embed).reference_message(msg),
                    )
                    .await;
            }
        }

        // Notify about any AFK users mentioned in this message (first 3, never self).
        for mentioned in msg.mentions.iter().take(3) {
            let mid = mentioned.id.get();
            if mid == user_id {
                continue;
            }
            let entry = self
                .state
                .afk_cache
                .get(&(guild_id, mid))
                .map(|e| e.clone());
            if let Some(entry) = entry {
                let dur =
                    humanize_duration(Duration::from_secs((now - entry.set_at).max(0) as u64));
                let embed = CreateEmbed::new()
                    .title(format!("{} is AFK", mentioned.name))
                    .description(entry.message.clone())
                    .color(PINK)
                    .timestamp(Timestamp::now())
                    .footer(CreateEmbedFooter::new(format!("Went AFK {dur} ago")));
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }
        }
    }
}
