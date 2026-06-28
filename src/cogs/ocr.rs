use super::Cog;
use crate::state::{AppState, CommandInvocation};
use async_trait::async_trait;
use serenity::all::{
    Attachment, Context, CreateAllowedMentions, CreateMessage, GetMessages, Message,
};
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
        [
            ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff",
        ]
        .iter()
        .any(|ext| name.ends_with(ext))
    }

    /// Upload text to mystb.in, returning the paste link on success.
    ///
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
    async fn on_command(&self, ctx: &Context, msg: &Message, inv: &CommandInvocation<'_>) -> bool {
        match inv.command {
            "ocr" | "imgread" | "read" => {}
            _ => return false,
        }
        if msg.guild_id.is_none() {
            return false;
        }
        let arg = inv.args;

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
                    return true;
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
                return true;
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
                return true;
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
            return true;
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
                        .send_message(
                            &ctx.http,
                            CreateMessage::new()
                                .content(format!(
                                    "```\n{truncated}\n```\n*(truncated — paste upload failed)*"
                                ))
                                .allowed_mentions(CreateAllowedMentions::new()),
                        )
                        .await;
                }
            }
        } else {
            // OCR'd text is attacker-controlled (an image can contain a
            // code-block breakout + @everyone), so suppress all pings.
            let _ = msg
                .channel_id
                .send_message(
                    &ctx.http,
                    CreateMessage::new()
                        .content(format!("```\n{text}\n```"))
                        .allowed_mentions(CreateAllowedMentions::new()),
                )
                .await;
        }
        true
    }
}
