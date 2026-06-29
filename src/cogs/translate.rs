use super::Cog;
use crate::framework::{Context, Data, Error, send_error};
use crate::state::AppState;
use crate::utils::{colors, format};
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, ComponentInteraction, CreateActionRow, CreateButton, CreateEmbed,
    CreateInteractionResponse, CreateInteractionResponseMessage, MessageId, Timestamp,
};
use std::sync::{Arc, LazyLock};

/// Custom_id prefix for the cog's interactive buttons.
const ORIGINAL_ID: &str = "tr:original";
const TRANSLATED_ID: &str = "tr:translated";

/// Per-message translation state, kept so toggle buttons rebuild
/// original / translated embeds long after the gtx call completed.
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

/// Cap on retained translation sessions to bound memory over long uptimes.
const MAX_STATES: usize = 1000;

static STATES: LazyLock<dashmap::DashMap<MessageId, TranslateState>> =
    LazyLock::new(dashmap::DashMap::new);

pub struct TranslateCog;

impl TranslateCog {
    pub fn new(_state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl Cog for TranslateCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let custom_id = interaction.data.custom_id.as_str();
        if !custom_id.starts_with("tr:") {
            return;
        }

        // Build an owned (title, body) so the DashMap guard is dropped before we
        // await the interaction response.
        let view = match STATES.get(&interaction.message.id) {
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

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![translate(), translate_context_menu()]
}

// ---- commands ---------------------------------------------------------------

/// Translate text into a target language.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Translate",
    aliases("trans")
)]
async fn translate(
    ctx: Context<'_>,
    #[description = "Target language"] to: Option<String>,
    #[description = "Text"]
    #[rest]
    text: String,
) -> Result<(), Error> {
    let state = &ctx.data().state;
    let target_lang = to.unwrap_or_else(|| "en".to_string());

    let (detected, translated) = match translate_text(state, &text, &target_lang).await {
        Some(t) => t,
        None => return send_error(ctx, "Translation service unavailable.").await,
    };

    let embed = build_embed(&detected, &target_lang, &text, &translated);
    let reply = poise::CreateReply::default()
        .embed(embed)
        .components(vec![buttons()]);

    let handle = ctx.send(reply).await?;
    let sent = handle.message().await?;
    crate::utils::cache::bounded_insert(
        &STATES,
        sent.id,
        TranslateState {
            src: detected,
            dest: target_lang,
            origin: text,
            translated,
        },
        MAX_STATES,
    );
    Ok(())
}

/// Right-click a message → Apps → Translate (to English, auto-detect source).
#[poise::command(context_menu_command = "Translate", category = "Translate")]
async fn translate_context_menu(
    ctx: Context<'_>,
    msg: serenity::all::Message,
) -> Result<(), Error> {
    if msg.content.is_empty() {
        return send_error(ctx, "That message has no text content to translate.").await;
    }

    let state = &ctx.data().state;
    let target_lang = "en".to_string();

    let (detected, translated) = match translate_text(state, &msg.content, &target_lang).await {
        Some(t) => t,
        None => return send_error(ctx, "Translation service unavailable.").await,
    };

    let embed = build_embed(&detected, &target_lang, &msg.content, &translated);
    let reply = poise::CreateReply::default()
        .ephemeral(true)
        .embed(embed)
        .components(vec![buttons()]);

    let handle = ctx.send(reply).await?;
    let sent = handle.message().await?;
    crate::utils::cache::bounded_insert(
        &STATES,
        sent.id,
        TranslateState {
            src: detected,
            dest: target_lang,
            origin: msg.content,
            translated,
        },
        MAX_STATES,
    );
    Ok(())
}

// ---- shared helpers ---------------------------------------------------------

/// Call the unofficial Google Translate (gtx) endpoint. Returns the detected
/// source language code and the translated text, or `None` on failure.
async fn translate_text(state: &AppState, text: &str, target: &str) -> Option<(String, String)> {
    let response = state
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

/// Build the summary embed shown alongside the toggle buttons (two fields, pink, timestamped).
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

/// The "Original" / "Translated" toggle row.
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

/// Discord rejects empty embed field/description values; fall back to a
/// zero-width space when the text is blank.
fn non_empty(s: &str) -> String {
    if s.is_empty() {
        "\u{200b}".to_string()
    } else {
        s.to_string()
    }
}

/// First char upper, rest unchanged (names below are already lowercase).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Map a language code to a human-readable name. Unknown codes fall back to the
/// capitalized code itself.
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
