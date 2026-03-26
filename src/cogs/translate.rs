use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct TranslateCog {
    state: Arc<AppState>,
}

impl TranslateCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for TranslateCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();
        if !content.starts_with(&prefix) {
            return;
        }
        let body = content[prefix.len()..].trim();
        let mut it = body.splitn(2, ' ');
        let Some(cmd) = it.next() else { return };
        if cmd != "translate" && cmd != "tr" {
            return;
        }
        let args = it.next().unwrap_or("").trim();

        if args.is_empty() {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Usage: translate [--to <lang>] <text>\nExample: translate --to es Hello world",
                )
                .await;
            return;
        }

        // Parse --to flag
        let (target_lang, text) = if args.starts_with("--to ") {
            let rest = &args[5..];
            let mut parts = rest.splitn(2, ' ');
            let lang = parts.next().unwrap_or("en");
            let text = parts.next().unwrap_or("").trim();
            (lang.to_string(), text.to_string())
        } else {
            ("en".to_string(), args.to_string())
        };

        if text.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Please provide text to translate.")
                .await;
            return;
        }

        // Call Google Translate unofficial API using query params (reqwest handles encoding)
        match self
            .state
            .http
            .get("https://translate.googleapis.com/translate_a/single")
            .query(&[
                ("client", "gtx"),
                ("sl", "auto"),
                ("tl", &target_lang),
                ("dt", "t"),
                ("q", &text),
            ])
            .send()
            .await
        {
            Ok(response) => match response.json::<serde_json::Value>().await {
                Ok(json) => {
                    // Response format: [[["translated","original",null,null,10],...],null,"detected_lang",...]
                    let translated = json
                        .get(0)
                        .and_then(|arr| arr.as_array())
                        .map(|chunks| {
                            chunks
                                .iter()
                                .filter_map(|chunk| chunk.get(0).and_then(|v| v.as_str()))
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_else(|| "Translation failed.".to_string());

                    let detected_lang = json
                        .get(2)
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            format!(
                                "**Translation** (detected: `{detected_lang}` → `{target_lang}`)\n{translated}"
                            ),
                        )
                        .await;
                }
                Err(e) => {
                    tracing::error!(error = ?e, "failed to parse translation response");
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Failed to parse translation.")
                        .await;
                }
            },
            Err(e) => {
                tracing::error!(error = ?e, "translation request failed");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Translation service unavailable.")
                    .await;
            }
        }
    }
}
