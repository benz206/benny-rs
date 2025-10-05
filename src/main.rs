use anyhow::Result;
use serde::Deserialize;
use serenity::all::{Client, Context, EventHandler, GatewayIntents, Ready};
use std::fs;
use tracing::{error, info};
mod state;
mod http;
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
    let servers_db = sqlx::SqlitePool::connect("sqlite:databases/servers.db").await?;
    let users_db = sqlx::SqlitePool::connect("sqlite:databases/users.db").await?;
    let app_state = std::sync::Arc::new(AppState::new(
        http_client,
        servers_db,
        users_db,
        config.Bot.Prefix.clone(),
    ));
    start_latency_task(app_state.clone());

    let token = if cfg!(debug_assertions) { &config.Bot.Dev_Token } else { &config.Bot.Token };
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS;

    struct Handler;

    #[serenity::async_trait]
    impl EventHandler for Handler {
        async fn ready(&self, _ctx: serenity::all::Context, ready: Ready) {
            info!("connected as {}", ready.user.name);
        }

        async fn message(&self, ctx: Context, msg: serenity::all::Message) {
            if msg.author.bot { return; }
            let content = msg.content.trim();
            // very simple prefix routing using config prefix
            // for now use '?' default since handler doesn't hold state
            let prefix = "?";
            if let Some(rest) = content.strip_prefix(prefix) {
                let cmd = rest.split_whitespace().next().unwrap_or("");
                match cmd {
                    "ping" => {
                        let _ = msg.channel_id.say(&ctx.http, "Pong!").await;
                    }
                    _ => {}
                }
            }
        }
    }

    let mut client = Client::builder(token, intents)
        .event_handler(Handler)
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
