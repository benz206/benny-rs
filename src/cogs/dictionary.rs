use super::Cog;
use crate::framework::{Context, Data, Error, send_error};
use crate::state::AppState;
use crate::utils::interactions;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::Deserialize;
use serenity::all::{
    Colour, ComponentInteraction, ComponentInteractionDataKind, CreateActionRow, CreateEmbed,
    CreateEmbedAuthor, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, Timestamp,
};
use std::sync::{Arc, LazyLock};

const API_URL: &str = "https://api.dictionaryapi.dev/api/v2/entries/en/";
const MAROON: Colour = Colour::new(0x85144B);
/// Component custom_id namespace for this cog.
const SELECT_ID: &str = "dict:select";

/// Session map: message id → author id, used for owner check on the dropdown.
static SESSIONS: LazyLock<DashMap<u64, u64>> = LazyLock::new(DashMap::new);

fn na() -> String {
    "N/A".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct License {
    #[serde(default = "na")]
    name: String,
    #[serde(default = "na")]
    url: String,
}

impl Default for License {
    fn default() -> Self {
        Self {
            name: na(),
            url: na(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Phonetic {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    audio: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Definition {
    #[serde(default)]
    definition: String,
    #[serde(default)]
    example: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Meaning {
    #[serde(rename = "partOfSpeech", default = "na")]
    part_of_speech: String,
    #[serde(default)]
    definitions: Vec<Definition>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Word {
    #[serde(default)]
    word: String,
    #[serde(default)]
    phonetics: Vec<Phonetic>,
    #[serde(default)]
    meanings: Vec<Meaning>,
    #[serde(default)]
    license: License,
}

impl Word {
    /// The first phonetic audio link, if present and non-empty.
    fn audio_url(&self) -> Option<&str> {
        self.phonetics
            .first()
            .and_then(|p| p.audio.as_deref())
            .filter(|s| s.starts_with("http"))
    }

    /// The first phonetic text, if any.
    fn phonetic_text(&self) -> Option<&str> {
        self.phonetics
            .iter()
            .find_map(|p| p.text.as_deref())
            .filter(|s| !s.is_empty())
    }
}

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
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        if !interaction.data.custom_id.starts_with("dict:") {
            return;
        }

        let selected = match &interaction.data.kind {
            ComponentInteractionDataKind::StringSelect { values } => values.first().cloned(),
            _ => None,
        };
        let Some(index) = selected.and_then(|v| v.parse::<usize>().ok()) else {
            return;
        };

        let message_id = interaction.message.id.get();

        // Owner check: only the original invoker may use the dropdown.
        if let Some(author_id) = SESSIONS.get(&message_id)
            && interaction.user.id.get() != *author_id {
                interactions::respond_ephemeral_text(
                    ctx,
                    interaction,
                    "This dictionary menu isn't yours to control.",
                )
                .await;
                return;
            }

        // Recover the word from the embed title ("<word> Definition").
        let title = interaction
            .message
            .embeds
            .first()
            .and_then(|e| e.title.as_deref())
            .unwrap_or("");
        let lookup = title.strip_suffix(" Definition").unwrap_or(title).trim();
        if lookup.is_empty() {
            return;
        }

        let word = match fetch_word(&self.state.http, lookup).await {
            Ok((200, json)) => match json
                .as_array()
                .and_then(|a| a.first())
                .cloned()
                .map(serde_json::from_value::<Word>)
            {
                Some(Ok(w)) => w,
                _ => return,
            },
            _ => return,
        };

        if index >= word.meanings.len() {
            return;
        }

        let response = CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .embed(meaning_embed(&word, index))
                .components(vec![build_select_menu(&word)]),
        );
        if let Err(e) = interaction.create_response(&ctx.http, response).await {
            tracing::error!(error = ?e, "failed to update dictionary message");
        }
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![define()]
}

// ---- commands --------------------------------------------------------------

/// Look up a word in the dictionary and show a part-of-speech dropdown.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Dictionary",
    aliases("dict", "def")
)]
async fn define(
    ctx: Context<'_>,
    #[description = "Word"]
    #[rest]
    word: String,
) -> Result<(), Error> {
    let word = word.trim().to_string();

    if word.is_empty() {
        return send_error(ctx, "Usage: define <word>").await;
    }

    if !word.chars().all(|c| c.is_alphabetic()) {
        return send_error(
            ctx,
            "The requested definition must be alphabetic, this means no spaces or special characters",
        )
        .await;
    }

    let state = &ctx.data().state;

    let (status, json) = match fetch_word(&state.http, &word).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(error = ?e, "dictionary request failed");
            return send_error(ctx, "Dictionary service unavailable.").await;
        }
    };

    if status != 200 {
        let message = json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Sorry, we couldn't find definitions for that word.");
        let title = json
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("No Definitions Found");
        ctx.say(format!("**{title}**\n{message}")).await?;
        return Ok(());
    }

    let entry = match json.as_array().and_then(|a| a.first()) {
        Some(e) => e.clone(),
        None => {
            return send_error(ctx, &format!("No definition found for `{word}`.")).await;
        }
    };

    let parsed: Word = match serde_json::from_value(entry) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = ?e, "failed to parse dictionary response");
            return send_error(ctx, "Failed to parse definition.").await;
        }
    };

    if parsed.meanings.is_empty() {
        return send_error(ctx, &format!("No definition found for `{word}`.")).await;
    }

    let handle = ctx
        .send(
            poise::CreateReply::default()
                .embed(initial_embed(&parsed))
                .components(vec![build_select_menu(&parsed)]),
        )
        .await?;
    let sent = handle.message().await?;
    crate::utils::cache::bounded_insert(&SESSIONS, sent.id.get(), ctx.author().id.get(), 2000);

    Ok(())
}

// ---- helpers ---------------------------------------------------------------

async fn fetch_word(
    client: &reqwest::Client,
    word: &str,
) -> Result<(u16, serde_json::Value), reqwest::Error> {
    let mut url = reqwest::Url::parse(API_URL).expect("API_URL is a valid base URL");
    url.path_segments_mut()
        .expect("API_URL is a base URL")
        .pop_if_empty()
        .push(word);
    let resp = client.get(url).send().await?;
    let status = resp.status();
    // Only 200 (found) and 404 (not found) reliably return a JSON body from
    // this API; other statuses (e.g. rate-limited/5xx) may return plain text
    // or HTML, so surface those as an error instead of parsing them as JSON.
    if !status.is_success() && status.as_u16() != 404 {
        return Err(resp.error_for_status().unwrap_err());
    }
    let json = resp.json::<serde_json::Value>().await?;
    Ok((status.as_u16(), json))
}

/// The dropdown of meanings: up to 25 options, label is the part of speech,
/// description is the first definition (truncated at 47 chars).
fn build_select_menu(word: &Word) -> CreateActionRow {
    let options: Vec<CreateSelectMenuOption> = word
        .meanings
        .iter()
        .take(25)
        .enumerate()
        .map(|(counter, meaning)| {
            let label = if meaning.part_of_speech.is_empty() {
                "N/A".to_string()
            } else {
                meaning.part_of_speech.clone()
            };
            let definition = meaning
                .definitions
                .first()
                .map(|d| d.definition.as_str())
                .unwrap_or("No definition");
            let description = if definition.chars().count() > 50 {
                format!("{}...", definition.chars().take(47).collect::<String>())
            } else {
                definition.to_string()
            };
            CreateSelectMenuOption::new(label, counter.to_string()).description(description)
        })
        .collect();

    CreateActionRow::SelectMenu(
        CreateSelectMenu::new(SELECT_ID, CreateSelectMenuKind::String { options })
            .placeholder("Choose a Meaning to View")
            .min_values(1)
            .max_values(1),
    )
}

fn word_license_author(word: &Word) -> CreateEmbedAuthor {
    let mut author = CreateEmbedAuthor::new(format!("License: {}", word.license.name));
    if word.license.url.starts_with("http") {
        author = author.url(word.license.url.clone());
    }
    author
}

/// The landing embed shown before any meaning is selected.
fn initial_embed(word: &Word) -> CreateEmbed {
    let mut description =
        String::from("Select one of the below to view different meanings of the word.");
    if let Some(text) = word.phonetic_text() {
        description.push_str(&format!("\n\n{text}"));
    }

    let mut embed = CreateEmbed::new()
        .title(format!("{} Definition", word.word))
        .description(description)
        .timestamp(Timestamp::now())
        .color(MAROON)
        .author(word_license_author(word))
        .footer(CreateEmbedFooter::new(format!(
            "Meaning -/{}",
            word.meanings.len()
        )));
    if let Some(audio) = word.audio_url() {
        embed = embed.url(audio);
    }
    embed
}

/// The per-meaning embed shown after a dropdown selection.
fn meaning_embed(word: &Word, index: usize) -> CreateEmbed {
    let meaning = &word.meanings[index];
    let definition = meaning
        .definitions
        .first()
        .map(|d| d.definition.clone())
        .unwrap_or_default();
    let example = meaning
        .definitions
        .first()
        .and_then(|d| d.example.as_deref())
        .filter(|s| !s.is_empty())
        .unwrap_or("No Example");

    let mut embed = CreateEmbed::new()
        .title(format!("{} Definition", word.word))
        .timestamp(Timestamp::now())
        .color(MAROON)
        .field("Part of Speech", &meaning.part_of_speech, false)
        .field("Definition", format!("{definition}\n>>> {example}"), false)
        .author(word_license_author(word))
        .footer(CreateEmbedFooter::new(format!(
            "Meaning {}/{}",
            index + 1,
            word.meanings.len()
        )));
    if let Some(audio) = word.audio_url() {
        embed = embed.url(audio);
    }
    embed
}
