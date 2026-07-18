use anyhow::Result;
use serenity::all::{ClientBuilder, GatewayIntents, GuildId};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{error, info};

mod cogs;
mod config;
mod entities;
mod framework;
mod http;
mod migrations;
mod state;
mod tagscript;
mod tasks;
mod utils;

use config::load_config;
use state::{AppState, start_latency_task};

fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::prelude::*;
    std::fs::create_dir_all("logs").ok();
    // Without a filter the registry emits everything at TRACE (gateway frames,
    // TLS handshakes, per-socket reads), which floods the log and leaks data.
    // Default to info; RUST_LOG still overrides.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let (file_writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::never("logs", "benny.log"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact(),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .init();
    guard
}

#[tokio::main]
async fn main() -> Result<()> {
    // Held until the end of main so the non-blocking file writer flushes on exit.
    let _log_guard = init_tracing();

    // The dependency tree pulls in both rustls crypto providers (aws-lc-rs and
    // ring), so rustls cannot pick a process-default on its own. Install one
    // explicitly up front, or any TLS client relying on the default — notably
    // the Lavalink client — panics on first use.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("install rustls crypto provider");

    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            error!(error = ?e, "failed to load config.json");
            return Err(e);
        }
    };

    info!("starting benny-rs");

    // Shared by every user-triggered external fetch (translate, dictionary,
    // ocr, sentiment, mystbin); reqwest has no default timeout, so without one
    // a slow upstream would hang the calling task indefinitely.
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    tokio::fs::create_dir_all("databases").await.ok();
    let servers_db = sqlx::SqlitePool::connect("sqlite://databases/servers.db?mode=rwc").await?;
    let users_db = sqlx::SqlitePool::connect("sqlite://databases/users.db?mode=rwc").await?;

    // Run SeaORM migrations (replaces the old inline CREATE TABLE schema in
    // db.rs). Each migrator runs against the SeaORM connection that shares the
    // corresponding sqlx pool.
    use sea_orm_migration::MigratorTrait;
    migrations::ServersMigrator::up(&sea_orm::DatabaseConnection::from(servers_db.clone()), None)
        .await?;
    migrations::UsersMigrator::up(&sea_orm::DatabaseConnection::from(users_db.clone()), None)
        .await?;

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
        | GatewayIntents::GUILD_MODERATION
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::GUILD_MESSAGE_REACTIONS
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::DIRECT_MESSAGES;

    use cogs::{
        CogManager, afk::AfkCog, automod::AutomodCog, base::BaseCog, dev::DevCog,
        dictionary::DictionaryCog, embed::EmbedCog, events::EventsCog, giveaways::GiveawaysCog,
        help::HelpCog, info::InfoCog, levels::LevelsCog, logging::LoggingCog,
        moderation::ModerationCog, music::MusicCog, ocr::OcrCog, prefixes::PrefixesCog,
        premium::PremiumCog, reminders::RemindersCog, roles::RolesCog, sentinel::SentinelCog,
        settings::SettingsCog, starboard::StarboardCog, translate::TranslateCog,
        welcome::WelcomeCog,
    };

    // Cogs own the gateway-event hooks (AFK, sentinel, logging, welcome, ...);
    // poise owns command dispatch. Both share the same `Arc<AppState>`.
    let mut manager = CogManager::new();
    manager.register(BaseCog::new(app_state.clone()));
    manager.register(PrefixesCog::new(app_state.clone()));
    manager.register(AfkCog::new(app_state.clone()));
    manager.register(RemindersCog::new(app_state.clone()));
    // TEMPORARILY DISABLED: TagScript engine has known issues (see tagscript/mod.rs).
    // manager.register(TagsCog::new(app_state.clone()));
    manager.register(WelcomeCog::new(app_state.clone()));
    manager.register(LoggingCog::new(app_state.clone()));
    manager.register(SettingsCog::new(app_state.clone()));
    manager.register(ModerationCog::new(app_state.clone()));
    manager.register(InfoCog::new(app_state.clone()));
    manager.register(HelpCog::new(app_state.clone()));
    manager.register(RolesCog::new(app_state.clone()));
    manager.register(TranslateCog::new(app_state.clone()));
    manager.register(DictionaryCog::new(app_state.clone()));
    manager.register(OcrCog::new(app_state.clone()));
    manager.register(EmbedCog::new(app_state.clone()));
    manager.register(SentinelCog::new(app_state.clone()));
    manager.register(AutomodCog::new(app_state.clone()));
    manager.register(GiveawaysCog::new(app_state.clone()));
    manager.register(LevelsCog::new(app_state.clone()));
    manager.register(StarboardCog::new(app_state.clone()));
    manager.register(DevCog::new(app_state.clone()));
    manager.register(PremiumCog::new(app_state.clone()));
    manager.register(MusicCog::new(app_state.clone()));
    manager.register(EventsCog::new(app_state.clone()));
    let manager = Arc::new(manager);

    let owners: HashSet<serenity::all::UserId> = config
        .owners
        .iter()
        .map(|&id| serenity::all::UserId::new(id))
        .collect();

    let mut commands = framework::all_commands();
    framework::apply_rate_limits(&mut commands);
    let options = poise::FrameworkOptions {
        commands,
        owners,
        prefix_options: poise::PrefixFrameworkOptions {
            // Guild-aware longest-match prefix; falls back to the global default.
            stripped_dynamic_prefix: Some(|ctx, msg, data| {
                framework::dynamic_prefix(ctx, msg, data)
            }),
            case_insensitive_commands: true,
            ..Default::default()
        },
        event_handler: |ctx, event, fw, data| {
            Box::pin(framework::event_handler(ctx, event, fw, data))
        },
        on_error: |error| Box::pin(framework::on_error(error)),
        pre_command: |ctx| Box::pin(framework::pre_command(ctx)),
        ..Default::default()
    };

    // Slash commands register to the support guild in debug builds (instant) and
    // globally in release (up to ~1h to propagate).
    let support_guild = config.support_guild;
    // app_state is moved into the framework setup closure below; keep a clone
    // for the HTTP server.
    let state_for_http = app_state.clone();
    let poise_framework = poise::Framework::builder()
        .options(options)
        .setup(move |ctx, ready, fw| {
            let state = app_state.clone();
            let cogs = manager.clone();
            Box::pin(async move {
                info!("connected as {} ({})", ready.user.name, ready.user.id);
                info!("===========================================");
                info!("  benny-rs v{}", env!("CARGO_PKG_VERSION"));
                info!("  Guilds: {}", ctx.cache.guilds().len());
                info!("===========================================");

                // Mirror the bot's identity and current guild membership into
                // shared state so the dashboard HTTP API (which has no serenity
                // Context) can authorize guild-scoped routes. `cogs::events`
                // keeps `guild_set` current on later joins/leaves.
                let _ = state.bot_id.set(ready.user.id.get());
                for gid in ctx.cache.guilds() {
                    state.guild_set.insert(gid.get(), ());
                }

                // Build the help menu from the registered command set.
                cogs::help::init_help_index(&fw.options().commands);

                if cfg!(debug_assertions) && let Some(gid) = support_guild {
                    poise::builtins::register_in_guild(
                        ctx,
                        &fw.options().commands,
                        GuildId::new(gid),
                    )
                    .await?;
                    info!("registered slash commands in support guild {gid}");
                } else {
                    poise::builtins::register_globally(ctx, &fw.options().commands).await?;
                    info!("registered slash commands globally");
                }

                // Connect to Lavalink exactly once and store the client in shared state.
                if state.lavalink.get().is_none() {
                    let client = cogs::music::connect_lavalink(&state, ready.user.id).await;
                    let _ = state.lavalink.set(client);
                }

                tasks::reminders::spawn_reminder_task(state.clone(), ctx.http.clone());

                Ok(framework::Data { state, cogs })
            })
        })
        .build();

    // Keep a bounded message cache so logging can show edited/deleted content.
    let mut cache_settings = serenity::cache::Settings::default();
    cache_settings.max_messages = 1000;

    let mut client = ClientBuilder::new(token, intents)
        .cache_settings(cache_settings)
        .framework(poise_framework)
        .await
        .map_err(|e| {
            error!(error = ?e, "failed to create serenity client");
            anyhow::anyhow!(e)
        })?;

    let api_addr: std::net::SocketAddr = state_for_http
        .config
        .dashboard_api_addr
        .as_deref()
        .unwrap_or("127.0.0.1:8080")
        .parse()
        .expect("dashboard_api_addr must be a valid host:port");
    let api = http::router(state_for_http);
    tokio::spawn(http::serve(api, api_addr));

    if let Err(e) = client.start().await {
        error!(error = ?e, "client exited with error");
    }

    Ok(())
}
