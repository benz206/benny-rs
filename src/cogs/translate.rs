use super::Cog;
use crate::state::AppState;
use crate::utils::{colors, format};
use async_trait::async_trait;
use dashmap::DashMap;
use serenity::all::{
    ButtonStyle, CommandInteraction, ComponentInteraction, Context, CreateActionRow, CreateButton,
    CreateEmbed, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    Message, MessageId, ResolvedTarget, Timestamp,
};
use std::sync::Arc;

/// custom_id prefix for this cog's interactive buttons.
const ORIGINAL_ID: &str = "tr:original";
const TRANSLATED_ID: &str = "tr:translated";

/// Per-message translation state, kept so the toggle buttons can rebuild the
/// original / translated embeds long after the gtx call has completed.
struct TranslateState {
    /// Detected source language code (e.g. `en`, `zh-cn`).
    src: String,
    /// Target language code the text was translated into.
    dest: String,
    /// Original (untranslated) text.
    origin: String,
    /// Translated text.
    translated: String,
}

pub struct TranslateCog {
    state: Arc<AppState>,
    /// Keyed by the id of the message that carries the buttons.
    states: DashMap<MessageId, TranslateState>,
}

impl TranslateCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            states: DashMap::new(),
        })
    }

    /// Call the unofficial Google Translate (gtx) endpoint. Returns the detected
    /// source language code and the translated text, or `None` on failure.
    pub async fn translate_text(&self, text: &str, target: &str) -> Option<(String, String)> {
        let response = self
            .state
            .http
            .get("https://translate.googleapis.com/translate_a/single")
            .query(&[
                ("client", "gtx"),
                ("sl", "auto"),
                ("tl", target),
                ("dt", "t"),
                ("q", text),
            ])
            .send()
            .await
            .ok()?;

        // Response format: [[["translated","original",...],...],null,"detected_lang",...]
        let json: serde_json::Value = response.json().await.ok()?;
        let translated = json.get(0).and_then(|arr| arr.as_array()).map(|chunks| {
            chunks
                .iter()
                .filter_map(|chunk| chunk.get(0).and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })?;
        let detected = json
            .get(2)
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();

        Some((detected, translated))
    }

    /// Build the summary embed shown alongside the toggle buttons, mirroring the
    /// Python `translate_cmd` embed (two fields, pink, timestamped).
    fn build_embed(src: &str, dest: &str, origin: &str, translated: &str) -> CreateEmbed {
        CreateEmbed::new()
            .title("Translating Text")
            .color(colors::PINK)
            .timestamp(Timestamp::now())
            .field(
                format!("Original: {}", language_name(src)),
                non_empty(format::truncate(origin, 1000)),
                false,
            )
            .field(
                format!("Translated: {}", language_name(dest)),
                non_empty(format::truncate(translated, 1000)),
                false,
            )
    }

    /// The "Original" / "Translated" toggle row (Python `TranslateView`).
    fn buttons() -> CreateActionRow {
        CreateActionRow::Buttons(vec![
            CreateButton::new(ORIGINAL_ID)
                .label("Original")
                .style(ButtonStyle::Secondary),
            CreateButton::new(TRANSLATED_ID)
                .label("Translated")
                .style(ButtonStyle::Success),
        ])
    }

    /// Translate a message's content and reply to an application-command
    /// interaction (used by the "Translate" message context menu in slash.rs).
    /// Defaults the target language to English, matching the Python context menu.
    pub async fn handle_context_menu(&self, ctx: &Context, interaction: &CommandInteraction) {
        let content = match interaction.data.target() {
            Some(ResolvedTarget::Message(m)) => m.content.clone(),
            _ => String::new(),
        };
        if content.trim().is_empty() {
            self.respond_ephemeral(ctx, interaction, "That message has no text to translate.")
                .await;
            return;
        }

        let target = "en".to_string();
        let (detected, translated) = match self.translate_text(&content, &target).await {
            Some(t) => t,
            None => {
                self.respond_ephemeral(ctx, interaction, "Translation service unavailable.")
                    .await;
                return;
            }
        };

        let embed = Self::build_embed(&detected, &target, &content, &translated);
        let response = CreateInteractionResponseMessage::new()
            .embed(embed)
            .components(vec![Self::buttons()]);
        if interaction
            .create_response(&ctx.http, CreateInteractionResponse::Message(response))
            .await
            .is_ok()
        {
            // Stash state under the response message id so the buttons work.
            if let Ok(sent) = interaction.get_response(&ctx.http).await {
                self.states.insert(
                    sent.id,
                    TranslateState {
                        src: detected,
                        dest: target,
                        origin: content,
                        translated,
                    },
                );
            }
        }
    }

    async fn respond_ephemeral(
        &self,
        ctx: &Context,
        interaction: &CommandInteraction,
        content: &str,
    ) {
        let _ = interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(content)
                        .ephemeral(true),
                ),
            )
            .await;
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
        if cmd != "translate" && cmd != "trans" {
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

        // Optional `--to <lang>` flag (default English), accepted at the front.
        let (target_lang, text) = if let Some(rest) = args.strip_prefix("--to ") {
            let mut parts = rest.trim_start().splitn(2, ' ');
            let lang = parts.next().unwrap_or("en").trim();
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

        let (detected, translated) = match self.translate_text(&text, &target_lang).await {
            Some(t) => t,
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Translation service unavailable.")
                    .await;
                return;
            }
        };

        let embed = Self::build_embed(&detected, &target_lang, &text, &translated);
        let builder = CreateMessage::new()
            .embed(embed)
            .components(vec![Self::buttons()])
            .reference_message(msg);

        match msg.channel_id.send_message(&ctx.http, builder).await {
            Ok(sent) => {
                self.states.insert(
                    sent.id,
                    TranslateState {
                        src: detected,
                        dest: target_lang,
                        origin: text,
                        translated,
                    },
                );
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to send translation message");
            }
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let custom_id = interaction.data.custom_id.as_str();
        if !custom_id.starts_with("tr:") {
            return;
        }

        // Build an owned (title, body) so the DashMap guard is dropped before we
        // await the interaction response.
        let view = match self.states.get(&interaction.message.id) {
            Some(state) => match custom_id {
                ORIGINAL_ID => Some((
                    format!("Original: {}", language_name(&state.src)),
                    format::truncate(&state.origin, 4000).to_string(),
                )),
                TRANSLATED_ID => Some((
                    format!("Translated: {}", language_name(&state.dest)),
                    format::truncate(&state.translated, 4000).to_string(),
                )),
                _ => None,
            },
            None => {
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("This translation has expired.")
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }
        };

        let Some((title, body)) = view else { return };
        let embed = CreateEmbed::new()
            .title(title)
            .description(non_empty(&body))
            .color(colors::PINK)
            .timestamp(Timestamp::now());
        let _ = interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .embed(embed)
                        .ephemeral(true),
                ),
            )
            .await;
    }
}

/// Discord rejects empty embed field/description values; fall back to a
/// zero-width space when the text is blank.
fn non_empty(s: &str) -> String {
    if s.is_empty() {
        "\u{200b}".to_string()
    } else {
        s.to_string()
    }
}

/// Capitalize like Python's `str.capitalize`: first char upper, rest unchanged
/// (names below are already lowercase, matching googletrans output).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Map a language code to a human-readable name, mirroring
/// `aiogtrans.LANGUAGES` (the googletrans table). Unknown codes fall back to
/// the capitalized code itself.
fn language_name(code: &str) -> String {
    let lc = code.to_ascii_lowercase();
    let name = match lc.as_str() {
        "af" => "afrikaans",
        "sq" => "albanian",
        "am" => "amharic",
        "ar" => "arabic",
        "hy" => "armenian",
        "az" => "azerbaijani",
        "eu" => "basque",
        "be" => "belarusian",
        "bn" => "bengali",
        "bs" => "bosnian",
        "bg" => "bulgarian",
        "ca" => "catalan",
        "ceb" => "cebuano",
        "ny" => "chichewa",
        "zh-cn" | "zh" => "chinese (simplified)",
        "zh-tw" => "chinese (traditional)",
        "co" => "corsican",
        "hr" => "croatian",
        "cs" => "czech",
        "da" => "danish",
        "nl" => "dutch",
        "en" => "english",
        "eo" => "esperanto",
        "et" => "estonian",
        "tl" => "filipino",
        "fi" => "finnish",
        "fr" => "french",
        "fy" => "frisian",
        "gl" => "galician",
        "ka" => "georgian",
        "de" => "german",
        "el" => "greek",
        "gu" => "gujarati",
        "ht" => "haitian creole",
        "ha" => "hausa",
        "haw" => "hawaiian",
        "iw" | "he" => "hebrew",
        "hi" => "hindi",
        "hmn" => "hmong",
        "hu" => "hungarian",
        "is" => "icelandic",
        "ig" => "igbo",
        "id" => "indonesian",
        "ga" => "irish",
        "it" => "italian",
        "ja" => "japanese",
        "jw" => "javanese",
        "kn" => "kannada",
        "kk" => "kazakh",
        "km" => "khmer",
        "ko" => "korean",
        "ku" => "kurdish (kurmanji)",
        "ky" => "kyrgyz",
        "lo" => "lao",
        "la" => "latin",
        "lv" => "latvian",
        "lt" => "lithuanian",
        "lb" => "luxembourgish",
        "mk" => "macedonian",
        "mg" => "malagasy",
        "ms" => "malay",
        "ml" => "malayalam",
        "mt" => "maltese",
        "mi" => "maori",
        "mr" => "marathi",
        "mn" => "mongolian",
        "my" => "myanmar (burmese)",
        "ne" => "nepali",
        "no" => "norwegian",
        "or" => "odia",
        "ps" => "pashto",
        "fa" => "persian",
        "pl" => "polish",
        "pt" => "portuguese",
        "pa" => "punjabi",
        "ro" => "romanian",
        "ru" => "russian",
        "sm" => "samoan",
        "gd" => "scots gaelic",
        "sr" => "serbian",
        "st" => "sesotho",
        "sn" => "shona",
        "sd" => "sindhi",
        "si" => "sinhala",
        "sk" => "slovak",
        "sl" => "slovenian",
        "so" => "somali",
        "es" => "spanish",
        "su" => "sundanese",
        "sw" => "swahili",
        "sv" => "swedish",
        "tg" => "tajik",
        "ta" => "tamil",
        "te" => "telugu",
        "th" => "thai",
        "tr" => "turkish",
        "uk" => "ukrainian",
        "ur" => "urdu",
        "ug" => "uyghur",
        "uz" => "uzbek",
        "vi" => "vietnamese",
        "cy" => "welsh",
        "xh" => "xhosa",
        "yi" => "yiddish",
        "yo" => "yoruba",
        "zu" => "zulu",
        _ => return capitalize(&lc),
    };
    capitalize(name)
}
