use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct OcrCog {
    state: Arc<AppState>,
}

impl OcrCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for OcrCog {
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
        if cmd != "ocr" {
            return;
        }
        let arg = it.next().unwrap_or("").trim();

        // Find image URL from arg or message attachment
        let image_url = if !arg.is_empty() {
            arg.to_string()
        } else if let Some(attachment) = msg.attachments.first() {
            attachment.url.clone()
        } else {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Please provide an image URL or attach an image.",
                )
                .await;
            return;
        };

        let _ = msg
            .channel_id
            .say(&ctx.http, "Processing image...")
            .await;

        // Use ocr.space free API with form data
        let form_data = [
            ("url", image_url.as_str()),
            ("apikey", "helloworld"), // Free demo key
            ("language", "eng"),
            ("isOverlayRequired", "false"),
        ];

        match self
            .state
            .http
            .post("https://api.ocr.space/parse/image")
            .form(&form_data)
            .send()
            .await
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    let text = json
                        .get("ParsedResults")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|r| r.get("ParsedText"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();

                    if text.is_empty() {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "No text detected in image.")
                            .await;
                    } else if text.len() > 1900 {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                format!("```\n{}\n```\n*(truncated)*", &text[..1900]),
                            )
                            .await;
                    } else {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, format!("```\n{text}\n```"))
                            .await;
                    }
                }
                Err(e) => {
                    tracing::error!(error = ?e, "failed to parse OCR response");
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Failed to parse OCR response.")
                        .await;
                }
            },
            Err(e) => {
                tracing::error!(error = ?e, "OCR request failed");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "OCR service unavailable.")
                    .await;
            }
        }
    }
}
