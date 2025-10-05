use anyhow::Result;
use serde::Deserialize;
use serenity::all::{Client, Context, EventHandler, GatewayIntents, Ready};
use std::fs;
use tracing::{error, info};
mod state;
mod http;
mod cogs;
mod db;
mod slash;
use state::{start_latency_task, AppState};
// no custom timer for now; default formatter is sufficient

#[derive(Debug, Deserialize, Clone)]
struct BotConfig {
    Bot: BotSection,
    Cogs: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct BotSection {
    Prefix: String,
    Token: String,
    Dev_Token: String,
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .compact()
        .init();
}

fn load_config() -> Result<BotConfig> {
    let bytes = fs::read("config.json")?;
    let cfg: BotConfig = serde_json::from_slice(&bytes)?;
    Ok(cfg)
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

    info!("starting bot");

    // Shared runtime state
    let http_client = reqwest::Client::builder().build()?;
    tokio::fs::create_dir_all("databases").await.ok();
    let servers_db = sqlx::SqlitePool::connect("sqlite://databases/servers.db").await?;
    let users_db = sqlx::SqlitePool::connect("sqlite://databases/users.db").await?;
    let app_state = std::sync::Arc::new(AppState::new(
        http_client,
        servers_db,
        users_db,
        config.Bot.Prefix.clone(),
    ));
    start_latency_task(app_state.clone());

    // ensure DB schemas
    db::ensure_servers_schema(app_state.servers_db()).await?;
    db::ensure_users_schema(app_state.users_db()).await?;

    let token = if cfg!(debug_assertions) { &config.Bot.Dev_Token } else { &config.Bot.Token };
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS;

    use std::sync::Arc;
    use cogs::{base::BaseCog, prefixes::PrefixesCog, CogManager};
    struct Handler {
        cogs: Arc<CogManager>,
    }

    #[serenity::async_trait]
    impl EventHandler for Handler {
        async fn ready(&self, ctx: serenity::all::Context, ready: Ready) {
            info!("connected as {}", ready.user.name);
            self.cogs.dispatch_ready(&ctx).await;
            slash::register_global(&ctx).await;
        }

        async fn message(&self, ctx: Context, msg: serenity::all::Message) {
            if msg.author.bot { return; }
            self.cogs.dispatch_message(&ctx, &msg).await;
        }

        async fn interaction_create(&self, ctx: Context, interaction: serenity::all::Interaction) {
            slash::handle_interaction(&ctx, &interaction).await;
        }
    }

    let mut manager = CogManager::new(config.Bot.Prefix.clone());
    manager.register(BaseCog::new(config.Bot.Prefix.clone()));
    manager.register(PrefixesCog::new(app_state.servers_db().clone(), config.Bot.Prefix.clone()));
    let manager = Arc::new(manager);

    let mut client = Client::builder(token, intents)
        .event_handler(Handler { cogs: manager.clone() })
        .await
        .map_err(|e| {
            error!(error = ?e, "failed to create serenity client");
            e
        })?;

    // start HTTP API (optional) on 127.0.0.1:8080
    let api = http::router(app_state.clone());
    tokio::spawn(http::serve(api, "127.0.0.1:8080".parse().unwrap()));

    if let Err(e) = client.start().await {
        error!(error = ?e, "client exited with error");
    }

    Ok(())
}
