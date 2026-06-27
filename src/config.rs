use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, sync::Arc};

#[derive(Debug, Deserialize, Clone)]
pub struct BotConfig {
    pub token: String,
    pub dev_token: Option<String>,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    #[serde(default)]
    pub cogs: Vec<String>,
    #[serde(default = "default_mongo_uri")]
    pub mongodb_uri: String,
    #[serde(default = "default_redis_uri")]
    pub redis_uri: String,
    #[serde(default)]
    pub lavalink: LavalinkConfig,
    #[serde(default)]
    pub spotify: SpotifyConfig,
    #[serde(default)]
    pub owners: Vec<u64>,
    pub support_guild: Option<u64>,
    #[serde(default)]
    pub sentiment_api_url: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct LavalinkConfig {
    #[serde(default = "default_lavalink_host")]
    pub host: String,
    #[serde(default = "default_lavalink_port")]
    pub port: u16,
    #[serde(default = "default_lavalink_password")]
    pub password: String,
    #[serde(default = "default_search_source")]
    pub search_source: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SpotifyConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
}

fn default_prefix() -> String {
    ">".to_string()
}
fn default_mongo_uri() -> String {
    "mongodb://localhost:27017".to_string()
}
fn default_redis_uri() -> String {
    "redis://localhost:6379".to_string()
}
fn default_lavalink_host() -> String {
    "localhost".to_string()
}
fn default_lavalink_port() -> u16 {
    2333
}
fn default_lavalink_password() -> String {
    "youshallnotpass".to_string()
}
fn default_search_source() -> String {
    "ytsearch".to_string()
}

pub fn load_config() -> Result<Arc<BotConfig>> {
    let bytes = fs::read("config.json").context("reading config.json")?;
    let cfg: BotConfig = serde_json::from_slice(&bytes).context("parsing config.json")?;
    Ok(Arc::new(cfg))
}
