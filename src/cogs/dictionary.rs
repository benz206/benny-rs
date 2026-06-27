use super::Cog;
use crate::state::AppState;
use async_trait::async_trait;
use dashmap::DashMap;
use serde::Deserialize;
use serenity::all::{
    Colour, ComponentInteraction, ComponentInteractionDataKind, Context, CreateActionRow,
    CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, Message, Timestamp,
};
use std::sync::Arc;

const API_URL: &str = "https://api.dictionaryapi.dev/api/v2/entries/en/";
/// style.Color.MAROON in the Python bot.
const MAROON: Colour = Colour::new(0x85144B);
/// Component custom_id namespace for this cog.
const SELECT_ID: &str = "dict:select";

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
    /// The first phonetic audio link, if present and non-empty (mirrors the
    /// Python `word.phonetics[0].audio if word.phonetics[0].audio else None`).
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

/// A fetched word cached for the lifetime of its dropdown message, keyed by the
/// bot's reply message id. `author_id` enforces the Python `interaction_check`
/// (only the invoker may drive the dropdown).
struct CachedWord {
    word: Word,
    author_id: u64,
}

pub struct DictionaryCog {
    state: Arc<AppState>,
    /// message id -> the word backing that message's dropdown.
    cache: DashMap<u64, CachedWord>,
}

impl DictionaryCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            cache: DashMap::new(),
        })
    }

    async fn fetch_word(&self, word: &str) -> Result<(u16, serde_json::Value), reqwest::Error> {
        // Build the URL through `Url` so the user-supplied word is percent-encoded
        // into the path segment rather than interpolated raw (which would let a
        // word with `/`, `?`, `#`, etc. alter the request target).
        let mut url = reqwest::Url::parse(API_URL).expect("API_URL is a valid base URL");
        url.path_segments_mut()
            .expect("API_URL is a base URL")
            .pop_if_empty()
            .push(word);
        let resp = self.state.http.get(url).send().await?;
        let status = resp.status().as_u16();
        let json = resp.json::<serde_json::Value>().await?;
        Ok((status, json))
    }

    /// The dropdown of meanings (`DictDropdown`): up to 25 options, label is the
    /// part of speech, description is the first definition (truncated like Python).
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

    fn author(word: &Word) -> CreateEmbedAuthor {
        let mut author = CreateEmbedAuthor::new(format!("License: {}", word.license.name));
        if word.license.url.starts_with("http") {
            author = author.url(word.license.url.clone());
        }
        author
    }

    /// The landing embed shown before any meaning is selected (Python
    /// `define_cmd`): title, the "select below" hint, phonetic text, audio url,
    /// license author and a `-/N` footer.
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
            .author(Self::author(word))
            .footer(CreateEmbedFooter::new(format!(
                "Meaning -/{}",
                word.meanings.len()
            )));
        if let Some(audio) = word.audio_url() {
            embed = embed.url(audio);
        }
        embed
    }

    /// The per-meaning embed shown after a dropdown selection (Python
    /// `DictDropdown.callback`): part-of-speech field, definition + example
    /// blockquote, license author and a `M/N` footer.
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
            .author(Self::author(word))
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
        if cmd != "define" && cmd != "dict" && cmd != "def" {
            return;
        }
        let word = it.next().unwrap_or("").trim();

        if word.is_empty() {
            let _ = msg.channel_id.say(&ctx.http, "Usage: define <word>").await;
            return;
        }

        // Python guards with `word.isalpha()` before hitting the API.
        if !word.chars().all(|c| c.is_alphabetic()) {
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "The requested definition must be alphabetic, this means no spaces or special characters",
                )
                .await;
            return;
        }

        let (status, json) = match self.fetch_word(word).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(error = ?e, "dictionary request failed");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Dictionary service unavailable.")
                    .await;
                return;
            }
        };

        if status != 200 {
            // The API returns a 404 JSON object with `title`/`message`.
            let message = json
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Sorry, we couldn't find definitions for that word.");
            let title = json
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("No Definitions Found");
            let _ = msg
                .channel_id
                .say(&ctx.http, format!("**{title}**\n{message}"))
                .await;
            return;
        }

        let entry = match json.as_array().and_then(|a| a.first()) {
            Some(e) => e.clone(),
            None => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format!("No definition found for `{word}`."))
                    .await;
                return;
            }
        };

        let parsed: Word = match serde_json::from_value(entry) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = ?e, "failed to parse dictionary response");
                let _ = msg
                    .channel_id
                    .say(&ctx.http, "Failed to parse definition.")
                    .await;
                return;
            }
        };

        if parsed.meanings.is_empty() {
            let _ = msg
                .channel_id
                .say(&ctx.http, format!("No definition found for `{word}`."))
                .await;
            return;
        }

        let builder = CreateMessage::new()
            .reference_message(msg)
            .embed(Self::initial_embed(&parsed))
            .components(vec![Self::build_select_menu(&parsed)]);

        match msg.channel_id.send_message(&ctx.http, builder).await {
            Ok(sent) => {
                self.cache.insert(
                    sent.id.get(),
                    CachedWord {
                        word: parsed,
                        author_id: msg.author.id.get(),
                    },
                );
            }
            Err(e) => {
                tracing::error!(error = ?e, "failed to send dictionary message");
            }
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
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

        // Primary path: the word is still cached. Fallback: re-fetch using the
        // word from the embed title (survives restarts / cache eviction).
        let cached = self.cache.get(&message_id);

        if let Some(entry) = cached.as_ref() {
            // interaction_check: only the original invoker may use the dropdown.
            if interaction.user.id.get() != entry.author_id {
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .ephemeral(true)
                                .content("This dictionary menu isn't yours to control."),
                        ),
                    )
                    .await;
                return;
            }
        }

        let word: Word = if let Some(entry) = cached.as_ref() {
            entry.word.clone()
        } else {
            // Cache miss: recover the word from the embed title ("<word> Definition").
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
            match self.fetch_word(lookup).await {
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
            }
        };
        drop(cached);

        if index >= word.meanings.len() {
            return;
        }

        let response = CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .embed(Self::meaning_embed(&word, index))
                .components(vec![Self::build_select_menu(&word)]),
        );
        if let Err(e) = interaction.create_response(&ctx.http, response).await {
            tracing::error!(error = ?e, "failed to update dictionary message");
        }
    }
}
