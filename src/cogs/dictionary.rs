use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

pub struct DictionaryCog {
    state: Arc<AppState>,
}

impl DictionaryCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for DictionaryCog {
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
        if cmd != "define" && cmd != "dict" {
            return;
        }
        let word = it.next().unwrap_or("").trim();

        if word.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, "Usage: define <word>")
                .await;
            return;
        }

        let url = format!("https://api.dictionaryapi.dev/api/v2/entries/en/{word}");
        match self.state.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => {
                        let entry = match json.as_array().and_then(|a| a.first()) {
                            Some(e) => e,
                            None => {
                                let _ = msg
                                    .channel_id
                                    .say(
                                        &ctx.http,
                                        format!("No definition found for `{word}`."),
                                    )
                                    .await;
                                return;
                            }
                        };

                        let phonetic = entry
                            .get("phonetic")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        let meanings = entry
                            .get("meanings")
                            .and_then(|v| v.as_array())
                            .map(|m| m.iter().take(3).collect::<Vec<_>>())
                            .unwrap_or_default();

                        let mut lines = vec![format!("**{word}** {phonetic}")];
                        for meaning in meanings {
                            let part_of_speech = meaning
                                .get("partOfSpeech")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            lines.push(format!("\n**{part_of_speech}**"));

                            if let Some(defs) =
                                meaning.get("definitions").and_then(|v| v.as_array())
                            {
                                for (i, def) in defs.iter().take(2).enumerate() {
                                    let definition = def
                                        .get("definition")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let example = def
                                        .get("example")
                                        .and_then(|v| v.as_str())
                                        .map(|e| format!("\n   *\"{e}\"*"))
                                        .unwrap_or_default();
                                    lines.push(format!(
                                        "{}. {definition}{example}",
                                        i + 1
                                    ));
                                }
                            }
                        }

                        let text = lines.join("\n");
                        let text = if text.len() > 1900 {
                            &text[..1900]
                        } else {
                            &text
                        };
                        let _ = msg.channel_id.say(&ctx.http, text).await;
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to parse dictionary response");
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "Failed to parse definition.")
                            .await;
                    }
                }
            }
            Ok(_) => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("No definition found for `{word}`."))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "dictionary request failed");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Dictionary service unavailable.")
                    .await;
            }
        }
    }
}
