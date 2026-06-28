use super::Cog;
use crate::framework::{Context, Data, Error, send_error};
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Attachment, CreateAllowedMentions};
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
impl Cog for OcrCog {}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![ocr()]
}

/// Read text from an image using OCR.
#[poise::command(
    slash_command,
    prefix_command,
    category = "OCR",
    aliases("imgread", "read")
)]
async fn ocr(
    ctx: Context<'_>,
    #[description = "Image attachment"] attachment: Option<Attachment>,
    #[description = "Image URL"]
    #[rest]
    image_url: Option<String>,
) -> Result<(), Error> {
    let image_url = if let Some(att) = attachment {
        att.url.clone()
    } else if let Some(url) = image_url {
        url
    } else if let poise::Context::Prefix(p) = ctx {
        match p.msg.attachments.first() {
            Some(att) => att.url.clone(),
            None => return send_error(ctx, "Please provide an image or url to read.").await,
        }
    } else {
        return send_error(ctx, "Please provide an image or url to read.").await;
    };

    let sctx = ctx.serenity_context();
    let state = &ctx.data().state;

    let _ = ctx.channel_id().broadcast_typing(&sctx.http).await;

    // Run OCR via the ocr.space free API (demo "helloworld" key).
    let form_data = [
        ("url", image_url.as_str()),
        ("apikey", "helloworld"),
        ("language", "eng"),
        ("isOverlayRequired", "false"),
    ];

    let resp = match state
        .http
        .post("https://api.ocr.space/parse/image")
        .form(&form_data)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = ?e, "OCR request failed");
            ctx.say("OCR service unavailable.").await?;
            return Ok(());
        }
    };

    let json = match resp.json::<serde_json::Value>().await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(error = ?e, "failed to parse OCR response");
            ctx.say("Failed to parse OCR response.").await?;
            return Ok(());
        }
    };

    if json
        .get("IsErroredOnProcessing")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        tracing::error!(response = ?json, "OCR processing error");
        ctx.say("OCR failed to process the image.").await?;
        return Ok(());
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
        ctx.say("No text detected in image.").await?;
    } else if text.len() > 1900 {
        // Too long for a single Discord message: upload to mystb.in.
        match upload_to_mystbin(state, &text).await {
            Some(link) => {
                ctx.say(format!(
                    "The text was {} characters, so it was uploaded: {link}",
                    text.len()
                ))
                .await?;
            }
            None => {
                // Degrade gracefully: truncate to fit in a message.
                let truncated = crate::utils::format::truncate(&text, 1900);
                ctx.send(
                    poise::CreateReply::default()
                        .content(format!(
                            "```\n{truncated}\n```\n*(truncated — paste upload failed)*"
                        ))
                        .allowed_mentions(CreateAllowedMentions::new()),
                )
                .await?;
            }
        }
    } else {
        // OCR'd text is attacker-controlled (an image can contain a
        // code-block breakout + @everyone), so suppress all pings.
        ctx.send(
            poise::CreateReply::default()
                .content(format!("```\n{text}\n```"))
                .allowed_mentions(CreateAllowedMentions::new()),
        )
        .await?;
    }

    Ok(())
}

/// Upload text to mystb.in, returning the paste link on success.
///
/// `POST https://mystb.in/api/paste` with `{"files": [{"content", "filename"}]}`,
/// responding with `{"id": ...}` → `https://mystb.in/{id}`.
async fn upload_to_mystbin(state: &AppState, text: &str) -> Option<String> {
    let body = serde_json::json!({
        "files": [{
            "content": text,
            "filename": "imgread.txt",
        }]
    });
    let resp = state
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
