use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Attachment, Context, GetMessages, Message};
use std::sync::Arc;

pub struct OcrCog {
    state: Arc<AppState>,
}

impl OcrCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }

    /// Whether an attachment is an image (by content-type or filename extension).
    fn is_image(att: &Attachment) -> bool {
        if let Some(ct) = &att.content_type {
            if ct.starts_with("image/") {
                return true;
            }
        }
        let name = att.filename.to_lowercase();
        [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff"]
            .iter()
            .any(|ext| name.ends_with(ext))
    }

    /// Upload text to mystb.in, returning the paste link on success.
    ///
    /// Mirrors the request the `mystbin` Python client makes in the original bot:
    /// `POST https://mystb.in/api/paste` with `{"files": [{"content", "filename"}]}`,
    /// responding with `{"id": ...}` → `https://mystb.in/{id}`.
    async fn upload_to_mystbin(&self, text: &str) -> Option<String> {
        let body = serde_json::json!({
            "files": [{
                "content": text,
                "filename": "imgread.txt",
            }]
        });
        let resp = self
            .state
            .http
            .post("https://mystb.in/api/paste")
            .json(&body)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().await.ok()?;
        let id = json.get("id").and_then(|v| v.as_str())?;
        Some(format!("https://mystb.in/{id}"))
    }
}

#[async_trait]
impl Cog for OcrCog {
    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        if msg.guild_id.is_none() {
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
        match cmd {
            "ocr" | "imgread" | "read" => {}
            _ => return,
        }
        let arg = it.next().unwrap_or("").trim();

        // Resolve the image URL: explicit arg, then the current message's
        // attachments, then the most recent image attachment in channel history.
        let image_url = if !arg.is_empty() {
            arg.to_string()
        } else if let Some(att) = msg.attachments.iter().find(|a| Self::is_image(a)) {
            att.url.clone()
        } else {
            let recent = msg
                .channel_id
                .messages(&ctx.http, GetMessages::new().before(msg.id).limit(50))
                .await
                .ok()
                .and_then(|msgs| {
                    msgs.iter()
                        .find_map(|m| m.attachments.iter().find(|a| Self::is_image(a)))
                        .map(|a| a.url.clone())
                });
            match recent {
                Some(url) => url,
                None => {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Please provide an image or url to read.")
                        .await;
                    return;
                }
            }
        };

        let _ = msg.channel_id.broadcast_typing(&ctx.http).await;

        // Run OCR via the ocr.space free API (demo "helloworld" key).
        let form_data = [
            ("url", image_url.as_str()),
            ("apikey", "helloworld"),
            ("language", "eng"),
            ("isOverlayRequired", "false"),
        ];

        let resp = match self
            .state
            .http
            .post("https://api.ocr.space/parse/image")
            .form(&form_data)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::error!(error = ?e, "OCR request failed");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "OCR service unavailable.")
                    .await;
                return;
            }
        };

        let json = match resp.json::<serde_json::Value>().await {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(error = ?e, "failed to parse OCR response");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to parse OCR response.")
                    .await;
                return;
            }
        };

        if json
            .get("IsErroredOnProcessing")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            tracing::error!(response = ?json, "OCR processing error");
            let _ = msg
                .channel_id
                .say(&ctx.http, "OCR failed to process the image.")
                .await;
            return;
        }

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
            // Too long for a single Discord message: upload to mystb.in.
            match self.upload_to_mystbin(&text).await {
                Some(link) => {
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            format!(
                                "The text was {} characters, so it was uploaded: {link}",
                                text.len()
                            ),
                        )
                        .await;
                }
                None => {
                    // Degrade gracefully: truncate to fit in a message.
                    let truncated = crate::utils::format::truncate(&text, 1900);
                    let _ = msg
                        .channel_id
                        .say(
                            &ctx.http,
                            format!("```\n{truncated}\n```\n*(truncated — paste upload failed)*"),
                        )
                        .await;
                }
            }
        } else {
            let _ = msg
                .channel_id
                .say(&ctx.http, format!("```\n{text}\n```"))
                .await;
        }
    }
}
