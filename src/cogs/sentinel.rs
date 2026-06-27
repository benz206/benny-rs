use super::Cog;
use crate::state::{AppState, SentinelConfig};
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct SentinelCog {
    state: Arc<AppState>,
}

impl SentinelCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for SentinelCog {
    async fn on_ready(&self, _ctx: &Context) {
        // Load sentinel configs
        let rows: Vec<(i64, i64, Option<i64>, f64, f64, f64, f64, f64, f64, f64)> = sqlx::query_as(
            "SELECT guild_id, enabled, log_channel_id, toxicity, severe_toxicity, obscene, threat, insult, identity_attack, sexual_explicit FROM sentinels_config"
        )
        .fetch_all(self.state.servers_db())
        .await
        .unwrap_or_default();

        for (guild_id, enabled, log_channel_id, toxicity, severe_toxicity, obscene, threat, insult, identity_attack, sexual_explicit) in rows {
            self.state.sentinel_cache.insert(guild_id as u64, SentinelConfig {
                enabled: enabled != 0,
                log_channel_id,
                toxicity,
                severe_toxicity,
                obscene,
                threat,
                insult,
                identity_attack,
                sexual_explicit,
            });
        }
        tracing::info!("Sentinel configs loaded");
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot { return; }
        let guild_id = match msg.guild_id { Some(g) => g.get(), None => return };
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();

        // Handle sentinel management commands
        if content.starts_with(&prefix) {
            let body = content[prefix.len()..].trim();
            let mut it = body.splitn(3, ' ');
            if let Some("sentinel") = it.next() {
                let subcmd = it.next().unwrap_or("");
                let arg = it.next().unwrap_or("").trim();
                self.handle_sentinel_cmd(ctx, msg, guild_id, subcmd, arg).await;
                return;
            }
        }

        // Toxicity check
        let config = match self.state.sentinel_cache.get(&guild_id) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };

        // Skip bot messages and commands
        if content.starts_with(&prefix) { return; }

        // Call toxicity API if configured
        if let Some(ref api_url) = self.state.config.sentiment_api_url {
            self.check_toxicity(ctx, msg, guild_id, &config, api_url).await;
        }
    }
}

impl SentinelCog {
    async fn check_toxicity(&self, ctx: &Context, msg: &Message, guild_id: u64, config: &SentinelConfig, api_url: &str) {
        let payload = serde_json::json!({ "text": &msg.content });
        match self.state.http.post(api_url)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let toxicity = json.get("toxicity").and_then(|v| v.as_f64()).unwrap_or(0.0);

                    if toxicity > config.toxicity {
                        // Log to webhook
                        if let Some(_log_channel_id) = config.log_channel_id {
                            let payload = serde_json::json!({
                                "embeds": [{
                                    "title": "Sentinel Alert",
                                    "color": 0xed4245,
                                    "fields": [
                                        { "name": "User", "value": format!("<@{}>", msg.author.id.get()), "inline": true },
                                        { "name": "Toxicity", "value": format!("{:.1}%", toxicity * 100.0), "inline": true },
                                        { "name": "Message", "value": &msg.content, "inline": false },
                                    ]
                                }]
                            });
                            if let Some(logging_config) = self.state.logging_cache.get(&guild_id) {
                                if logging_config.enabled {
                                    let _ = self.state.http.post(&logging_config.webhook_url)
                                        .json(&payload)
                                        .send()
                                        .await;
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => tracing::debug!(error = ?e, "sentinel API call failed"),
        }
    }

    async fn handle_sentinel_cmd(&self, ctx: &Context, msg: &Message, guild_id: u64, subcmd: &str, arg: &str) {
        match subcmd {
            "enable" => {
                let _ = sqlx::query(
                    "INSERT INTO sentinels_config (guild_id, enabled) VALUES (?, 1) ON CONFLICT(guild_id) DO UPDATE SET enabled = 1"
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                let mut entry = self.state.sentinel_cache.entry(guild_id).or_insert_with(|| SentinelConfig {
                    enabled: true,
                    log_channel_id: None,
                    toxicity: 0.85,
                    severe_toxicity: 0.85,
                    obscene: 0.85,
                    threat: 0.85,
                    insult: 0.85,
                    identity_attack: 0.85,
                    sexual_explicit: 0.85,
                });
                entry.enabled = true;
                let _ = msg.channel_id.say(&ctx.http, "Sentinel enabled.").await;
            }
            "disable" => {
                let _ = sqlx::query(
                    "UPDATE sentinels_config SET enabled = 0 WHERE guild_id = ?"
                )
                .bind(guild_id as i64)
                .execute(self.state.servers_db())
                .await;
                if let Some(mut e) = self.state.sentinel_cache.get_mut(&guild_id) { e.enabled = false; }
                let _ = msg.channel_id.say(&ctx.http, "Sentinel disabled.").await;
            }
            "threshold" => {
                // sentinel threshold toxicity 0.7
                let mut parts = arg.splitn(2, ' ');
                let category = parts.next().unwrap_or("toxicity");
                let value: f64 = match parts.next().and_then(|v| v.parse().ok()) {
                    Some(v) if v >= 0.0 && v <= 1.0 => v,
                    _ => {
                        let _ = msg.channel_id.say(&ctx.http, "Usage: sentinel threshold <category> <0.0-1.0>").await;
                        return;
                    }
                };

                let col = match category {
                    "toxicity" => "toxicity",
                    "severe_toxicity" | "severe" => "severe_toxicity",
                    "obscene" => "obscene",
                    "threat" => "threat",
                    "insult" => "insult",
                    "identity_attack" | "identity" => "identity_attack",
                    "sexual_explicit" | "sexual" => "sexual_explicit",
                    _ => {
                        let _ = msg.channel_id.say(&ctx.http, "Invalid category. Use: toxicity, severe_toxicity, obscene, threat, insult, identity_attack, sexual_explicit").await;
                        return;
                    }
                };

                // Can't use dynamic column names with sqlx bind, so use match
                let query = format!(
                    "INSERT INTO sentinels_config (guild_id, {col}) VALUES (?, ?) ON CONFLICT(guild_id) DO UPDATE SET {col} = excluded.{col}"
                );
                let _ = sqlx::query(sqlx::AssertSqlSafe(query))
                    .bind(guild_id as i64)
                    .bind(value)
                    .execute(self.state.servers_db())
                    .await;

                if let Some(mut e) = self.state.sentinel_cache.get_mut(&guild_id) {
                    match col {
                        "toxicity" => e.toxicity = value,
                        "severe_toxicity" => e.severe_toxicity = value,
                        "obscene" => e.obscene = value,
                        "threat" => e.threat = value,
                        "insult" => e.insult = value,
                        "identity_attack" => e.identity_attack = value,
                        "sexual_explicit" => e.sexual_explicit = value,
                        _ => {}
                    }
                }
                let _ = msg.channel_id.say(&ctx.http, format!("{col} threshold set to {value:.2}.")).await;
            }
            _ => {
                let _ = msg.channel_id.say(&ctx.http, "Usage: `sentinel enable` | `sentinel disable` | `sentinel threshold <category> <0.0-1.0>`").await;
            }
        }
    }
}
