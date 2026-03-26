use crate::config::BotConfig;
use dashmap::DashMap;
use mongodb::Client as MongoClient;
use parking_lot::Mutex;
use redis::aio::ConnectionManager as RedisManager;
use reqwest::Client as HttpClient;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::time::{sleep, Duration};

// Simple data types for caches
#[derive(Debug, Clone)]
pub struct AfkEntry {
    pub message: String,
    pub set_at: i64, // Unix timestamp
}

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
    pub servers_db: SqlitePool,
    pub users_db: SqlitePool,
    pub mongo: Option<MongoClient>,
    pub redis: Option<Arc<tokio::sync::Mutex<RedisManager>>>,
    pub prefix_cache: Arc<DashMap<u64, Vec<String>>>,
    pub afk_cache: Arc<DashMap<(u64, u64), AfkEntry>>,
    pub tag_cache: Arc<DashMap<u64, HashMap<String, Tag>>>,
    pub sentinel_cache: Arc<DashMap<u64, SentinelConfig>>,
    pub welcome_cache: Arc<DashMap<u64, WelcomeConfig>>,
    pub goodbye_cache: Arc<DashMap<u64, GoodbyeConfig>>,
    pub logging_cache: Arc<DashMap<u64, LoggingConfig>>,
    pub latency_ms: Arc<Mutex<Vec<u64>>>,
    pub start_time: Instant,
}

impl AppState {
    pub fn new(
        config: Arc<BotConfig>,
        http: HttpClient,
        servers_db: SqlitePool,
        users_db: SqlitePool,
        mongo: Option<MongoClient>,
        redis: Option<Arc<tokio::sync::Mutex<RedisManager>>>,
    ) -> Self {
        Self {
            config,
            http,
            servers_db,
            users_db,
            mongo,
            redis,
            prefix_cache: Arc::new(DashMap::new()),
            afk_cache: Arc::new(DashMap::new()),
            tag_cache: Arc::new(DashMap::new()),
            sentinel_cache: Arc::new(DashMap::new()),
            welcome_cache: Arc::new(DashMap::new()),
            goodbye_cache: Arc::new(DashMap::new()),
            logging_cache: Arc::new(DashMap::new()),
            latency_ms: Arc::new(Mutex::new(Vec::with_capacity(64))),
            start_time: Instant::now(),
        }
    }

    pub fn http(&self) -> &HttpClient { &self.http }
    pub fn servers_db(&self) -> &SqlitePool { &self.servers_db }
    pub fn users_db(&self) -> &SqlitePool { &self.users_db }
    pub fn prefix(&self) -> &str { &self.config.prefix }
    pub fn latency(&self) -> Arc<Mutex<Vec<u64>>> { self.latency_ms.clone() }
    pub fn uptime_secs(&self) -> u64 { self.start_time.elapsed().as_secs() }
    pub fn is_owner(&self, user_id: u64) -> bool { self.config.owners.contains(&user_id) }
}

pub fn start_latency_task(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut value: u64 = 0;
        loop {
            {
                let mut history = state.latency_ms.lock();
                if history.len() >= 60 { history.remove(0); }
                history.push(value);
            }
            value = value.saturating_add(1);
            sleep(Duration::from_secs(30)).await;
        }
    });
}
