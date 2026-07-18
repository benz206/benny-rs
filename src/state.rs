use crate::config::BotConfig;
use dashmap::DashMap;
use parking_lot::Mutex;
use redis::aio::ConnectionManager as RedisManager;
use reqwest::Client as HttpClient;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serenity::all::Message;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::time::{Duration, sleep};

// Simple data types for caches
#[derive(Debug, Clone)]
pub struct AfkEntry {
    pub message: String,
    pub set_at: i64, // Unix timestamp
}

// Fields beyond `content` are only read by the dormant TagsCog (disabled while
// the TagScript engine is off); the live HTTP tags API still writes them.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Tag {
    pub name: String,
    pub content: String,
    pub owner_id: i64,
    pub uses: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct SentinelConfig {
    pub enabled: bool,
    pub log_channel_id: Option<i64>,
    pub toxicity: f64,
    pub severe_toxicity: f64,
    pub obscene: f64,
    pub threat: f64,
    pub insult: f64,
    pub identity_attack: f64,
    pub sexual_explicit: f64,
}

#[derive(Debug, Clone)]
pub struct WelcomeConfig {
    pub channel_id: Option<i64>,
    pub message: String,
    pub embed_json: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct GoodbyeConfig {
    pub channel_id: Option<i64>,
    pub message: String,
    pub embed_json: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    pub webhook_url: String,
    pub enabled: bool,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<BotConfig>,
    pub http: HttpClient,
    /// SeaORM handles wrapping the sqlx pools (built via `From<SqlitePool>`).
    /// These own the only references to the underlying pools; cogs query
    /// exclusively through them.
    pub servers_orm: DatabaseConnection,
    pub users_orm: DatabaseConnection,
    pub redis: Option<Arc<tokio::sync::Mutex<RedisManager>>>,
    pub prefix_cache: Arc<DashMap<u64, Vec<String>>>,
    pub afk_cache: Arc<DashMap<(u64, u64), AfkEntry>>,
    pub tag_cache: Arc<DashMap<u64, HashMap<String, Tag>>>,
    pub sentinel_cache: Arc<DashMap<u64, SentinelConfig>>,
    pub welcome_cache: Arc<DashMap<u64, WelcomeConfig>>,
    pub goodbye_cache: Arc<DashMap<u64, GoodbyeConfig>>,
    pub logging_cache: Arc<DashMap<u64, LoggingConfig>>,
    /// Set of guild ids the bot is currently a member of. Serenity's cache is
    /// the source of truth on the gateway side, but an HTTP handler has no
    /// serenity `Context`; this mirror (hydrated at `ready`, maintained on
    /// guild join/leave in `cogs::events`) lets the dashboard API answer
    /// "is the bot in this guild?" without one. Used as a set; the unit value
    /// is irrelevant.
    pub guild_set: Arc<DashMap<u64, ()>>,
    /// The bot's own user id, set once at `ready`. None until then.
    pub bot_id: Arc<tokio::sync::OnceCell<u64>>,
    /// Lavalink client, set once at `ready` (see `cogs::music`). Empty until then,
    /// so the bot runs fine without a Lavalink server.
    pub lavalink: Arc<tokio::sync::OnceCell<lavalink_rs::client::LavalinkClient>>,
    pub latency_ms: Arc<Mutex<Vec<u64>>>,
    pub start_time: Instant,
}

impl AppState {
    pub fn new(
        config: Arc<BotConfig>,
        http: HttpClient,
        servers_db: SqlitePool,
        users_db: SqlitePool,
        redis: Option<Arc<tokio::sync::Mutex<RedisManager>>>,
    ) -> Self {
        // Wrap the sqlx pools so SeaORM owns them directly (no second pool /
        // no extra SQLite file locking).
        let servers_orm = DatabaseConnection::from(servers_db);
        let users_orm = DatabaseConnection::from(users_db);
        Self {
            config,
            http,
            servers_orm,
            users_orm,
            redis,
            prefix_cache: Arc::new(DashMap::new()),
            afk_cache: Arc::new(DashMap::new()),
            tag_cache: Arc::new(DashMap::new()),
            sentinel_cache: Arc::new(DashMap::new()),
            welcome_cache: Arc::new(DashMap::new()),
            goodbye_cache: Arc::new(DashMap::new()),
            logging_cache: Arc::new(DashMap::new()),
            guild_set: Arc::new(DashMap::new()),
            bot_id: Arc::new(tokio::sync::OnceCell::new()),
            lavalink: Arc::new(tokio::sync::OnceCell::new()),
            latency_ms: Arc::new(Mutex::new(Vec::with_capacity(64))),
            start_time: Instant::now(),
        }
    }

    pub fn servers_orm(&self) -> &DatabaseConnection {
        &self.servers_orm
    }
    pub fn users_orm(&self) -> &DatabaseConnection {
        &self.users_orm
    }
    pub fn prefix(&self) -> &str {
        &self.config.prefix
    }
    /// A guild's raw custom prefix list — empty when it has none — cache-first
    /// with a DB fallback for a cold cache, sorted by length to match the cache
    /// ordering. This is for the prefix-management UIs (`prefix list/add`,
    /// `settings`), which must distinguish "no custom prefixes" from the
    /// default. Command dispatch uses `guild_prefixes` instead, which falls back
    /// to the default and never touches the DB.
    pub async fn custom_prefixes(&self, guild_id: u64) -> Vec<String> {
        if let Some(entry) = self.prefix_cache.get(&guild_id) {
            return entry.clone();
        }
        let rows = crate::entities::prefixes::Entity::find()
            .filter(crate::entities::prefixes::Column::GuildId.eq(guild_id as i64))
            .all(&self.servers_orm)
            .await
            .unwrap_or_default();
        let mut result: Vec<String> = rows.into_iter().map(|m| m.prefix).collect();
        result.sort_by_key(|p| p.len());
        result
    }
    /// Active prefixes for a guild: the guild's custom prefixes if it has any,
    /// otherwise the global default. Cache-only — this is the single source of
    /// truth used by command dispatch, so it must not touch the DB on the hot
    /// path (the cache is hydrated on ready and on guild join). `None` (DMs)
    /// always resolves to the default prefix.
    pub fn guild_prefixes(&self, guild_id: Option<u64>) -> Vec<String> {
        if let Some(gid) = guild_id
            && let Some(entry) = self.prefix_cache.get(&gid)
                && !entry.is_empty() {
                    return entry.clone();
                }
        vec![self.config.prefix.clone()]
    }
    /// Whether the message begins with one of the guild's active prefixes.
    /// Used by passive scanners (e.g. Sentinel) to skip command messages.
    /// poise handles actual command parsing/dispatch.
    pub fn starts_with_prefix(&self, msg: &Message) -> bool {
        let content = msg.content.trim_start();
        self.guild_prefixes(msg.guild_id.map(|g| g.get()))
            .iter()
            .any(|p| !p.is_empty() && content.starts_with(p.as_str()))
    }
    pub fn latency(&self) -> Arc<Mutex<Vec<u64>>> {
        self.latency_ms.clone()
    }
    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }
    pub fn is_owner(&self, user_id: u64) -> bool {
        self.config.owners.contains(&user_id)
    }
    /// Whether the bot is currently a member of `guild_id`, per the `guild_set`
    /// mirror. Used by the dashboard API to 404 guild-scoped routes for guilds
    /// the bot isn't in (an HTTP handler has no serenity cache to consult).
    pub fn in_guild(&self, guild_id: u64) -> bool {
        self.guild_set.contains_key(&guild_id)
    }
    /// The Lavalink client, if it has been initialized at `ready`.
    pub fn lavalink(&self) -> Option<lavalink_rs::client::LavalinkClient> {
        self.lavalink.get().cloned()
    }
}

/// Samples real gateway heartbeat latency from the shard runners every 30s and
/// keeps the last 60 samples for `/ping`. Shards report `None` until their
/// first heartbeat ack, so early ticks may record nothing.
pub fn start_latency_task(state: Arc<AppState>, shards: Arc<serenity::gateway::ShardManager>) {
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(30)).await;
            let avg_ms = {
                let runners = shards.runners.lock().await;
                let latencies: Vec<u128> = runners
                    .values()
                    .filter_map(|r| r.latency)
                    .map(|d| d.as_millis())
                    .collect();
                if latencies.is_empty() {
                    None
                } else {
                    Some((latencies.iter().sum::<u128>() / latencies.len() as u128) as u64)
                }
            };
            if let Some(ms) = avg_ms {
                let mut history = state.latency_ms.lock();
                if history.len() >= 60 {
                    history.remove(0);
                }
                history.push(ms);
            }
        }
    });
}
