use anyhow::Result;
use serenity::all::{
    ChannelId, Client, Context, EventHandler, GatewayIntents,
    Guild, GuildId, Interaction, Member, Message, MessageId,
    Reaction, Ready, UnavailableGuild, User,
};
use serenity::model::event::MessageUpdateEvent;
use std::sync::Arc;
use tracing::{error, info};

mod config;
mod error;
mod state;
mod http;
mod cogs;
mod db;
mod slash;
mod utils;

use config::load_config;
use state::{start_latency_task, AppState};

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .compact()
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            error!(error = ?e, "failed to load config.json");
            return Err(e);
        }
    };

    info!("starting benny-rs");

    let http_client = reqwest::Client::builder().build()?;
    tokio::fs::create_dir_all("databases").await.ok();
    let servers_db = sqlx::SqlitePool::connect("sqlite://databases/servers.db?mode=rwc").await?;
    let users_db = sqlx::SqlitePool::connect("sqlite://databases/users.db?mode=rwc").await?;

    db::ensure_servers_schema(&servers_db).await?;
    db::ensure_users_schema(&users_db).await?;

    // Connect MongoDB (optional - warn if unavailable)
    let mongo = match mongodb::Client::with_uri_str(&config.mongodb_uri).await {
        Ok(client) => {
            info!("MongoDB connected");
            Some(client)
        }
        Err(e) => {
            tracing::warn!(error = ?e, "MongoDB unavailable, moderation features disabled");
            None
        }
    };

    // Connect Redis (optional - warn if unavailable)
    let redis = match redis::Client::open(config.redis_uri.as_str()) {
        Ok(client) => match redis::aio::ConnectionManager::new(client).await {
            Ok(mgr) => {
                info!("Redis connected");
                Some(Arc::new(tokio::sync::Mutex::new(mgr)))
            }
            Err(e) => {
                tracing::warn!(error = ?e, "Redis unavailable, caching features disabled");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = ?e, "Redis unavailable, caching features disabled");
            None
        }
    };

    let app_state = Arc::new(AppState::new(
        config.clone(),
        http_client,
        servers_db,
        users_db,
        mongo,
        redis,
    ));
    start_latency_task(app_state.clone());

    let token = if cfg!(debug_assertions) {
        config.dev_token.as_deref().unwrap_or(&config.token)
    } else {
        &config.token
    };

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::GUILD_MESSAGE_REACTIONS
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::DIRECT_MESSAGES;

    use cogs::{base::BaseCog, prefixes::PrefixesCog, CogManager};

    struct Handler {
        cogs: Arc<CogManager>,
    }

    #[serenity::async_trait]
    impl EventHandler for Handler {
        async fn ready(&self, ctx: Context, ready: Ready) {
            info!("connected as {} ({})", ready.user.name, ready.user.id);
            self.cogs.dispatch_ready(&ctx).await;
            slash::register_global(&ctx).await;
        }

        async fn message(&self, ctx: Context, msg: Message) {
            if msg.author.bot { return; }
            self.cogs.dispatch_message(&ctx, &msg).await;
        }

        async fn guild_member_addition(&self, ctx: Context, member: Member) {
            self.cogs.dispatch_member_join(&ctx, &member).await;
        }

        async fn guild_member_removal(&self, ctx: Context, guild_id: GuildId, user: User, _member: Option<Member>) {
            self.cogs.dispatch_member_leave(&ctx, guild_id, &user).await;
        }

        async fn message_update(&self, ctx: Context, old: Option<Message>, new: Option<Message>, event: MessageUpdateEvent) {
            self.cogs.dispatch_message_update(&ctx, old, new, &event).await;
        }

        async fn message_delete(&self, ctx: Context, channel_id: ChannelId, msg_id: MessageId, guild_id: Option<GuildId>) {
            self.cogs.dispatch_message_delete(&ctx, channel_id, msg_id, guild_id).await;
        }

        async fn reaction_add(&self, ctx: Context, reaction: Reaction) {
            self.cogs.dispatch_reaction_add(&ctx, reaction).await;
        }

        async fn guild_create(&self, ctx: Context, guild: Guild, _is_new: Option<bool>) {
            self.cogs.dispatch_guild_create(&ctx, &guild).await;
        }

        async fn guild_delete(&self, ctx: Context, incomplete: UnavailableGuild, full: Option<Guild>) {
            self.cogs.dispatch_guild_delete(&ctx, incomplete, full).await;
        }

        async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
            match &interaction {
                Interaction::Command(_) | Interaction::Autocomplete(_) => {
                    slash::handle_interaction(&ctx, &interaction).await;
                }
                Interaction::Component(c) => {
                    self.cogs.dispatch_component(&ctx, c).await;
                }
                Interaction::Modal(m) => {
                    self.cogs.dispatch_modal(&ctx, m).await;
                }
                _ => {}
            }
        }
    }

    let mut manager = CogManager::new(config.prefix.clone());
    manager.register(BaseCog::new(config.prefix.clone()));
    manager.register(PrefixesCog::new(app_state.servers_db().clone(), config.prefix.clone()));
    let manager = Arc::new(manager);

    let mut client = Client::builder(token, intents)
        .event_handler(Handler { cogs: manager.clone() })
        .await
        .map_err(|e| {
            error!(error = ?e, "failed to create serenity client");
            anyhow::anyhow!(e)
        })?;

    let api = http::router(app_state.clone());
    tokio::spawn(http::serve(api, "127.0.0.1:8080".parse().unwrap()));

    if let Err(e) = client.start().await {
        error!(error = ?e, "client exited with error");
    }

    Ok(())
}
