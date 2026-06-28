use super::Cog;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::format::truncate;
use crate::utils::{colors, embeds};
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{Value, json};
use serenity::all::{
    ActionRowComponent, ButtonStyle, Channel, ChannelId, ChannelType, Colour, ComponentInteraction,
    ComponentInteractionDataKind, CreateActionRow, CreateAttachment, CreateButton, CreateEmbed,
    CreateEmbedAuthor, CreateEmbedFooter, CreateInputText, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage,
    CreateModal, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, GuildId,
    InputTextStyle, ModalInteraction, Permissions, Timestamp, UserId,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

// ---- custom_id namespace --------------------------------------------------
//
// `on_component`/`on_modal` are fanned out to every cog, so every id this cog
// owns is prefixed with `emb:` and we early-return for anything else.
const ID_PREFIX: &str = "emb:";

// Buttons / selects on the builder message.
const BTN_AUTHOR: &str = "emb:author";
const BTN_BASE: &str = "emb:base";
const BTN_IMAGES: &str = "emb:images";
const BTN_FOOTER: &str = "emb:footer";
const BTN_ADDFIELD: &str = "emb:addfield";
const BTN_REMOVEFIELD: &str = "emb:removefield";
const SEL_REMOVE: &str = "emb:removeselect";
const BTN_SEND: &str = "emb:send";
const SEL_SEND: &str = "emb:sendselect";
const BTN_BACK: &str = "emb:back";
const BTN_IMPORT: &str = "emb:import";
const BTN_EXPORT_JSON: &str = "emb:exportjson";
const BTN_EXPORT_MYST: &str = "emb:exportmyst";
const BTN_CANCEL: &str = "emb:cancel";
const BTN_COMPLETE: &str = "emb:complete";

// Modal ids.
const MODAL_AUTHOR: &str = "emb:modal:author";
const MODAL_BASE: &str = "emb:modal:base";
const MODAL_IMAGES: &str = "emb:modal:images";
const MODAL_FOOTER: &str = "emb:modal:footer";
const MODAL_ADDFIELD: &str = "emb:modal:addfield";
const MODAL_IMPORT: &str = "emb:modal:import";

/// Discord hard limits (used to keep previews valid).
const MAX_FIELDS: usize = 25;

// ---- the embed model ------------------------------------------------------

/// A full, editable representation of a Discord embed. Renders to a `CreateEmbed`
/// preview and round-trips through Discord's embed-object JSON format.
#[derive(Debug, Clone, Default)]
struct EmbedData {
    title: Option<String>,
    description: Option<String>,
    url: Option<String>,
    color: Option<u32>,
    /// Unix seconds.
    timestamp: Option<i64>,
    author_name: Option<String>,
    author_url: Option<String>,
    author_icon: Option<String>,
    footer_text: Option<String>,
    footer_icon: Option<String>,
    image_url: Option<String>,
    thumbnail_url: Option<String>,
    fields: Vec<EmbedField>,
}

#[derive(Debug, Clone, Default)]
struct EmbedField {
    name: String,
    value: String,
    inline: bool,
}

/// Normalize a raw input into `Some(trimmed)` or `None` when blank.
fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Read a non-empty string out of a JSON object by key.
fn jstr(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

impl EmbedData {
    /// The default starter embed (title "Embed Creator", a hint description, current timestamp).
    fn starter() -> Self {
        Self {
            title: Some("Embed Creator".to_string()),
            description: Some("Create an embed with this view!".to_string()),
            color: Some(colors::BLURPLE.0),
            timestamp: Some(Timestamp::now().unix_timestamp()),
            ..Default::default()
        }
    }

    /// Build the live preview embed. Clamps every value to Discord's limits so
    /// the preview is always renderable, and injects a placeholder description
    /// when the embed would otherwise be empty (Discord rejects empty embeds).
    fn to_create_embed(&self) -> CreateEmbed {
        let mut e = CreateEmbed::new();
        let mut empty = true;

        if let Some(t) = self.title.as_deref().filter(|s| !s.is_empty()) {
            e = e.title(truncate(t, 256));
            empty = false;
        }
        if let Some(d) = self.description.as_deref().filter(|s| !s.is_empty()) {
            e = e.description(truncate(d, 4096));
            empty = false;
        }
        if let Some(u) = self.url.as_deref().filter(|s| !s.is_empty()) {
            e = e.url(u);
        }
        if let Some(c) = self.color {
            e = e.color(Colour(c));
        }
        if let Some(name) = self.author_name.as_deref().filter(|s| !s.is_empty()) {
            let mut a = CreateEmbedAuthor::new(truncate(name, 256));
            if let Some(u) = self.author_url.as_deref().filter(|s| !s.is_empty()) {
                a = a.url(u);
            }
            if let Some(i) = self.author_icon.as_deref().filter(|s| !s.is_empty()) {
                a = a.icon_url(i);
            }
            e = e.author(a);
            empty = false;
        }
        if let Some(text) = self.footer_text.as_deref().filter(|s| !s.is_empty()) {
            let mut f = CreateEmbedFooter::new(truncate(text, 2048));
            if let Some(i) = self.footer_icon.as_deref().filter(|s| !s.is_empty()) {
                f = f.icon_url(i);
            }
            e = e.footer(f);
            empty = false;
        }
        if let Some(i) = self.image_url.as_deref().filter(|s| !s.is_empty()) {
            e = e.image(i);
            empty = false;
        }
        if let Some(t) = self.thumbnail_url.as_deref().filter(|s| !s.is_empty()) {
            e = e.thumbnail(t);
            empty = false;
        }
        for f in &self.fields {
            // Discord rejects fields with an empty name or value; substitute a
            // zero-width space so imported/edited embeds always render.
            let name = if f.name.trim().is_empty() {
                "\u{200b}"
            } else {
                truncate(&f.name, 256)
            };
            let value = if f.value.trim().is_empty() {
                "\u{200b}"
            } else {
                truncate(&f.value, 1024)
            };
            e = e.field(name, value, f.inline);
            empty = false;
        }
        if let Some(t) = self
            .timestamp
            .and_then(|ts| Timestamp::from_unix_timestamp(ts).ok())
        {
            e = e.timestamp(t);
        }
        if empty {
            e = e.description("*This embed is empty. Use the buttons below to add content.*");
        }
        e
    }

    /// Serialize to Discord's embed-object JSON shape.
    fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        if let Some(t) = &self.title {
            map.insert("title".into(), json!(t));
        }
        if let Some(d) = &self.description {
            map.insert("description".into(), json!(d));
        }
        if let Some(u) = &self.url {
            map.insert("url".into(), json!(u));
        }
        if let Some(c) = self.color {
            map.insert("color".into(), json!(c));
        }
        if let Some(t) = self
            .timestamp
            .and_then(|ts| Timestamp::from_unix_timestamp(ts).ok())
        {
            map.insert("timestamp".into(), json!(t.to_string()));
        }
        if let Some(name) = &self.author_name {
            let mut a = serde_json::Map::new();
            a.insert("name".into(), json!(name));
            if let Some(u) = &self.author_url {
                a.insert("url".into(), json!(u));
            }
            if let Some(i) = &self.author_icon {
                a.insert("icon_url".into(), json!(i));
            }
            map.insert("author".into(), Value::Object(a));
        }
        if let Some(text) = &self.footer_text {
            let mut f = serde_json::Map::new();
            f.insert("text".into(), json!(text));
            if let Some(i) = &self.footer_icon {
                f.insert("icon_url".into(), json!(i));
            }
            map.insert("footer".into(), Value::Object(f));
        }
        if let Some(i) = &self.image_url {
            map.insert("image".into(), json!({ "url": i }));
        }
        if let Some(t) = &self.thumbnail_url {
            map.insert("thumbnail".into(), json!({ "url": t }));
        }
        if !self.fields.is_empty() {
            let fields: Vec<Value> = self
                .fields
                .iter()
                .map(|f| json!({ "name": f.name, "value": f.value, "inline": f.inline }))
                .collect();
            map.insert("fields".into(), Value::Array(fields));
        }
        Value::Object(map)
    }

    /// Parse Discord's embed-object JSON shape into an `EmbedData`.
    fn from_json(v: &Value) -> Self {
        let mut d = EmbedData {
            title: jstr(v, "title"),
            description: jstr(v, "description"),
            url: jstr(v, "url"),
            color: v.get("color").and_then(|c| c.as_u64()).map(|c| c as u32),
            ..Default::default()
        };
        d.timestamp = v.get("timestamp").and_then(|t| {
            if let Some(s) = t.as_str() {
                Timestamp::parse(s).ok().map(|ts| ts.unix_timestamp())
            } else {
                t.as_i64()
            }
        });
        if let Some(a) = v.get("author") {
            d.author_name = jstr(a, "name");
            d.author_url = jstr(a, "url");
            d.author_icon = jstr(a, "icon_url");
        }
        if let Some(f) = v.get("footer") {
            d.footer_text = jstr(f, "text");
            d.footer_icon = jstr(f, "icon_url");
        }
        d.image_url = v.get("image").and_then(|i| jstr(i, "url"));
        d.thumbnail_url = v.get("thumbnail").and_then(|t| jstr(t, "url"));
        if let Some(arr) = v.get("fields").and_then(|x| x.as_array()) {
            for f in arr.iter().take(MAX_FIELDS) {
                let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
                let value = f.get("value").and_then(|x| x.as_str()).unwrap_or("").trim();
                if name.is_empty() && value.is_empty() {
                    continue;
                }
                d.fields.push(EmbedField {
                    name: truncate(name, 256).to_string(),
                    value: truncate(value, 1024).to_string(),
                    inline: f.get("inline").and_then(|x| x.as_bool()).unwrap_or(false),
                });
            }
        }
        d
    }
}

/// An interactive builder session, keyed by the builder message id. `owner_id`
/// enforces that only the invoker may drive it.
struct Builder {
    data: EmbedData,
    owner_id: u64,
}

// ---- module-level session stores ------------------------------------------
//
// Command fns are free functions and cannot see cog struct fields, so both
// session maps live here as module statics shared by command fns and the
// `on_component`/`on_modal` hooks.

static BUILDERS: LazyLock<DashMap<u64, Builder>> = LazyLock::new(DashMap::new);
static TEXT_SESSIONS: LazyLock<DashMap<u64, EmbedData>> = LazyLock::new(DashMap::new);

pub struct EmbedCog {
    state: Arc<AppState>,
}

impl EmbedCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for EmbedCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let msg_id = interaction.message.id.get();

        // Ownership / session check.
        let owner = match BUILDERS.get(&msg_id) {
            Some(b) => b.owner_id,
            None => {
                ephemeral(
                    ctx,
                    interaction,
                    embeds::error_embed("This embed builder has expired."),
                )
                .await;
                return;
            }
        };
        if interaction.user.id.get() != owner {
            ephemeral(
                ctx,
                interaction,
                embeds::error_embed("This embed builder isn't yours to control."),
            )
            .await;
            return;
        }

        match cid {
            // Buttons that open a pre-filled modal.
            BTN_AUTHOR | BTN_BASE | BTN_IMAGES | BTN_FOOTER => {
                let data = BUILDERS
                    .get(&msg_id)
                    .map(|b| b.data.clone())
                    .unwrap_or_default();
                let modal = match cid {
                    BTN_AUTHOR => build_author_modal(&data),
                    BTN_BASE => build_base_modal(&data),
                    BTN_IMAGES => build_images_modal(&data),
                    _ => build_footer_modal(&data),
                };
                let _ = interaction
                    .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
                    .await;
            }
            BTN_ADDFIELD => {
                let full = BUILDERS
                    .get(&msg_id)
                    .map(|b| b.data.fields.len() >= MAX_FIELDS)
                    .unwrap_or(false);
                if full {
                    ephemeral(
                        ctx,
                        interaction,
                        embeds::warning_embed("This embed already has the maximum of 25 fields."),
                    )
                    .await;
                } else {
                    let _ = interaction
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Modal(build_addfield_modal()),
                        )
                        .await;
                }
            }
            BTN_IMPORT => {
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Modal(build_import_modal()),
                    )
                    .await;
            }
            // Switch to the "remove a field" sub-view.
            BTN_REMOVEFIELD => {
                let (embed, components) = {
                    let b = BUILDERS.get(&msg_id);
                    let Some(b) = b else { return };
                    if b.data.fields.is_empty() {
                        drop(b);
                        ephemeral(
                            ctx,
                            interaction,
                            embeds::warning_embed("There are no fields to remove."),
                        )
                        .await;
                        return;
                    }
                    (b.data.to_create_embed(), build_remove_components(&b.data))
                };
                update(ctx, interaction, embed, components).await;
            }
            // Switch to the "send to a channel" sub-view.
            BTN_SEND => {
                let embed = BUILDERS
                    .get(&msg_id)
                    .map(|b| b.data.to_create_embed())
                    .unwrap_or_default();
                update(ctx, interaction, embed, build_send_components()).await;
            }
            // Back to the main builder view.
            BTN_BACK => {
                let (embed, components) = {
                    let b = BUILDERS.get(&msg_id);
                    let Some(b) = b else { return };
                    (b.data.to_create_embed(), build_main_components(&b.data))
                };
                update(ctx, interaction, embed, components).await;
            }
            // A field was chosen for removal.
            SEL_REMOVE => {
                let idx = match &interaction.data.kind {
                    ComponentInteractionDataKind::StringSelect { values } => {
                        values.first().and_then(|v| v.parse::<usize>().ok())
                    }
                    _ => None,
                };
                let (embed, components) = {
                    let mut b = match BUILDERS.get_mut(&msg_id) {
                        Some(b) => b,
                        None => return,
                    };
                    if let Some(i) = idx.filter(|&i| i < b.data.fields.len()) {
                        b.data.fields.remove(i);
                    }
                    (b.data.to_create_embed(), build_main_components(&b.data))
                };
                update(ctx, interaction, embed, components).await;
            }
            // A channel was chosen to send to.
            SEL_SEND => {
                let channel = match &interaction.data.kind {
                    ComponentInteractionDataKind::ChannelSelect { values } => {
                        values.first().copied()
                    }
                    _ => None,
                };
                let (embed, components, preview) = {
                    let b = match BUILDERS.get(&msg_id) {
                        Some(b) => b,
                        None => return,
                    };
                    (
                        b.data.to_create_embed(),
                        build_main_components(&b.data),
                        b.data.to_create_embed(),
                    )
                };
                // Restore the main view first, then report the outcome as a followup.
                update(ctx, interaction, embed, components).await;
                if let Some(channel) = channel {
                    // The channel select is guild-scoped, but only shows that the
                    // user can *view* a channel — confirm they can post there too.
                    if let Some(gid) = interaction.guild_id
                        && !user_can_send_in(ctx, gid, interaction.user.id, channel).await
                    {
                        let followup = CreateInteractionResponseFollowup::new()
                            .embed(embeds::error_embed(
                                "You don't have permission to send messages in that channel.",
                            ))
                            .ephemeral(true);
                        let _ = interaction.create_followup(&ctx.http, followup).await;
                        return;
                    }
                    let sent = channel
                        .send_message(&ctx.http, CreateMessage::new().embed(preview))
                        .await;
                    let followup = match sent {
                        Ok(_) => CreateInteractionResponseFollowup::new()
                            .embed(embeds::success_embed(
                                "Embed Sent",
                                &format!("Sent your embed to <#{}>.", channel.get()),
                            ))
                            .ephemeral(true),
                        Err(_) => CreateInteractionResponseFollowup::new()
                            .embed(embeds::error_embed(
                                "I couldn't send to that channel. Check my permissions there.",
                            ))
                            .ephemeral(true),
                    };
                    let _ = interaction.create_followup(&ctx.http, followup).await;
                }
            }
            // Export the embed JSON as a file attachment.
            BTN_EXPORT_JSON => {
                let pretty = BUILDERS
                    .get(&msg_id)
                    .map(|b| serde_json::to_string_pretty(&b.data.to_json()).unwrap_or_default())
                    .unwrap_or_default();
                let file = CreateAttachment::bytes(pretty.into_bytes(), "custom_embed.json");
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .embed(embeds::success_embed(
                                    "Exported JSON",
                                    "Your embed's JSON is attached below.",
                                ))
                                .add_file(file)
                                .ephemeral(true),
                        ),
                    )
                    .await;
            }
            // Export the embed JSON to a Mystbin paste.
            BTN_EXPORT_MYST => {
                let _ = interaction.defer_ephemeral(&ctx.http).await;
                let pretty = BUILDERS
                    .get(&msg_id)
                    .map(|b| serde_json::to_string_pretty(&b.data.to_json()).unwrap_or_default())
                    .unwrap_or_default();
                let followup = match self.upload_to_mystbin(&pretty).await {
                    Some(link) => CreateInteractionResponseFollowup::new()
                        .embed(embeds::success_embed("Exported to Mystbin", &link))
                        .ephemeral(true),
                    None => CreateInteractionResponseFollowup::new()
                        .embed(embeds::error_embed(
                            "Failed to upload to Mystbin. Try again later.",
                        ))
                        .ephemeral(true),
                };
                let _ = interaction.create_followup(&ctx.http, followup).await;
            }
            // Discard the session and strip the controls.
            BTN_CANCEL => {
                BUILDERS.remove(&msg_id);
                update_no_components(
                    ctx,
                    interaction,
                    CreateEmbed::new()
                        .title("Embed Creator Cancelled")
                        .description("Discarded this embed.")
                        .color(colors::RED)
                        .timestamp(Timestamp::now()),
                )
                .await;
            }
            // Finalize: keep the embed, remove the controls.
            BTN_COMPLETE => {
                let embed = BUILDERS
                    .get(&msg_id)
                    .map(|b| b.data.to_create_embed())
                    .unwrap_or_default();
                BUILDERS.remove(&msg_id);
                update_no_components(ctx, interaction, embed).await;
            }
            _ => {}
        }
    }

    async fn on_modal(&self, ctx: &serenity::all::Context, interaction: &ModalInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let Some(msg) = interaction.message.as_ref() else {
            return;
        };
        let msg_id = msg.id.get();

        // Ownership / session check.
        let owner = match BUILDERS.get(&msg_id) {
            Some(b) => b.owner_id,
            None => return,
        };
        if interaction.user.id.get() != owner {
            return;
        }

        let inputs = collect_inputs(interaction);

        // The import modal can fail (bad JSON / unreachable paste): handle it
        // separately so we can surface an ephemeral error without an update.
        if cid == MODAL_IMPORT {
            let link = inputs.get("import_link").map(|s| s.trim()).unwrap_or("");
            if link.is_empty() {
                return;
            }
            match self.parse_import(link).await {
                Ok(data) => {
                    let (embed, components) = {
                        let mut b = match BUILDERS.get_mut(&msg_id) {
                            Some(b) => b,
                            None => return,
                        };
                        b.data = data;
                        (b.data.to_create_embed(), build_main_components(&b.data))
                    };
                    let _ = interaction
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::UpdateMessage(
                                CreateInteractionResponseMessage::new()
                                    .embed(embed)
                                    .components(components),
                            ),
                        )
                        .await;
                }
                Err(e) => {
                    let _ = interaction
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .embed(embeds::error_embed(&e))
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                }
            }
            return;
        }

        // Mutate the session for the property modals, then refresh the preview.
        let (embed, components) = {
            let mut b = match BUILDERS.get_mut(&msg_id) {
                Some(b) => b,
                None => return,
            };
            let d = &mut b.data;
            match cid {
                MODAL_AUTHOR => {
                    d.author_name = inputs.get("name").map(|s| s.as_str()).and_then(opt);
                    d.author_url = inputs.get("author_url").map(|s| s.as_str()).and_then(opt);
                    d.author_icon = inputs.get("author_icon").map(|s| s.as_str()).and_then(opt);
                }
                MODAL_BASE => {
                    d.title = inputs.get("title").map(|s| s.as_str()).and_then(opt);
                    d.description = inputs.get("description").map(|s| s.as_str()).and_then(opt);
                    d.url = inputs.get("url").map(|s| s.as_str()).and_then(opt);
                    match inputs.get("color").map(|s| s.trim()).unwrap_or("") {
                        "" => d.color = None,
                        c => {
                            if let Some(v) = parse_hex_color(c) {
                                d.color = Some(v);
                            }
                        }
                    }
                }
                MODAL_IMAGES => {
                    d.image_url = inputs.get("image_url").map(|s| s.as_str()).and_then(opt);
                    d.thumbnail_url = inputs
                        .get("thumbnail_url")
                        .map(|s| s.as_str())
                        .and_then(opt);
                }
                MODAL_FOOTER => {
                    d.footer_text = inputs.get("text").map(|s| s.as_str()).and_then(opt);
                    d.footer_icon = inputs.get("footer_icon").map(|s| s.as_str()).and_then(opt);
                }
                MODAL_ADDFIELD => {
                    let name = inputs.get("field_name").map(|s| s.trim()).unwrap_or("");
                    let value = inputs.get("field_value").map(|s| s.trim()).unwrap_or("");
                    let inline = inputs
                        .get("field_inline")
                        .map(|s| {
                            matches!(s.trim().to_lowercase().as_str(), "true" | "yes" | "1" | "y")
                        })
                        .unwrap_or(false);
                    if (!name.is_empty() || !value.is_empty()) && d.fields.len() < MAX_FIELDS {
                        d.fields.push(EmbedField {
                            name: if name.is_empty() {
                                "\u{200b}".to_string()
                            } else {
                                truncate(name, 256).to_string()
                            },
                            value: if value.is_empty() {
                                "\u{200b}".to_string()
                            } else {
                                truncate(value, 1024).to_string()
                            },
                            inline,
                        });
                    }
                }
                _ => return,
            }
            (d.to_create_embed(), build_main_components(d))
        };

        let _ = interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::UpdateMessage(
                    CreateInteractionResponseMessage::new()
                        .embed(embed)
                        .components(components),
                ),
            )
            .await;
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![embed()]
}

// ---- commands --------------------------------------------------------------

/// Build or edit a custom embed interactively or step by step.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    subcommand_required,
    subcommands(
        "embed_new",
        "embed_title",
        "embed_description",
        "embed_color",
        "embed_author",
        "embed_footer",
        "embed_field",
        "embed_preview",
        "embed_send",
        "embed_clear"
    )
)]
async fn embed(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Open the interactive embed builder.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "new",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_new(ctx: Context<'_>) -> Result<(), Error> {
    let data = EmbedData::starter();
    let embed = data.to_create_embed();
    let components = build_main_components(&data);
    let handle = ctx
        .send(poise::CreateReply::default().embed(embed).components(components))
        .await?;
    let sent = handle.message().await?;
    BUILDERS.insert(
        sent.id.get(),
        Builder {
            data,
            owner_id: ctx.author().id.get(),
        },
    );
    Ok(())
}

/// Set the title of your embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "title",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_title(
    ctx: Context<'_>,
    #[description = "Title text"]
    #[rest]
    title: String,
) -> Result<(), Error> {
    TEXT_SESSIONS
        .entry(ctx.author().id.get())
        .or_default()
        .title = opt(&title);
    send_embed(
        ctx,
        embeds::success_embed("Embed Creator", "Title set."),
    )
    .await
}

/// Set the description of your embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "description",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_description(
    ctx: Context<'_>,
    #[description = "Description text"]
    #[rest]
    description: String,
) -> Result<(), Error> {
    TEXT_SESSIONS
        .entry(ctx.author().id.get())
        .or_default()
        .description = opt(&description);
    send_embed(
        ctx,
        embeds::success_embed("Embed Creator", "Description set."),
    )
    .await
}

/// Set the color of your embed draft (hex, e.g. ff5733).
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "color",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_color(
    ctx: Context<'_>,
    #[description = "Hex color (e.g. ff5733)"] color: String,
) -> Result<(), Error> {
    match parse_hex_color(&color) {
        Some(c) => {
            TEXT_SESSIONS
                .entry(ctx.author().id.get())
                .or_default()
                .color = Some(c);
            send_embed(
                ctx,
                embeds::success_embed("Embed Creator", &format!("Color set to #{c:06X}.")),
            )
            .await
        }
        None => send_error(ctx, "Invalid hex color. Example: `embed color ff5733`").await,
    }
}

/// Set the author name of your embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "author",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_author(
    ctx: Context<'_>,
    #[description = "Author text"]
    #[rest]
    author: String,
) -> Result<(), Error> {
    TEXT_SESSIONS
        .entry(ctx.author().id.get())
        .or_default()
        .author_name = opt(&author);
    send_embed(
        ctx,
        embeds::success_embed("Embed Creator", "Author set."),
    )
    .await
}

/// Set the footer text of your embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "footer",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_footer(
    ctx: Context<'_>,
    #[description = "Footer text"]
    #[rest]
    footer: String,
) -> Result<(), Error> {
    TEXT_SESSIONS
        .entry(ctx.author().id.get())
        .or_default()
        .footer_text = opt(&footer);
    send_embed(
        ctx,
        embeds::success_embed("Embed Creator", "Footer set."),
    )
    .await
}

/// Add a field to your embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "field",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_field(
    ctx: Context<'_>,
    #[description = "Field name"] name: String,
    #[description = "Field value"] value: String,
    #[description = "Inline (true/false)"] inline: Option<bool>,
) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    let mut session = TEXT_SESSIONS.entry(user_id).or_default();
    if session.fields.len() >= MAX_FIELDS {
        drop(session);
        return send_error(ctx, "This embed already has 25 fields.").await;
    }
    session.fields.push(EmbedField {
        name: truncate(name.trim(), 256).to_string(),
        value: truncate(value.trim(), 1024).to_string(),
        inline: inline.unwrap_or(false),
    });
    drop(session);
    send_embed(
        ctx,
        embeds::success_embed("Embed Creator", "Field added."),
    )
    .await
}

/// Preview your current embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "preview",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_preview(ctx: Context<'_>) -> Result<(), Error> {
    let user_id = ctx.author().id.get();
    match TEXT_SESSIONS.get(&user_id) {
        Some(session) => {
            let embed = session.to_create_embed();
            drop(session);
            send_embed(ctx, embed).await
        }
        None => {
            send_error(
                ctx,
                "No active embed. Start with `embed title <text>` or `embed new`.",
            )
            .await
        }
    }
}

/// Send your embed draft to a channel.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "send",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_send(
    ctx: Context<'_>,
    #[description = "Channel to send to"] channel: ChannelId,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();
    let user_id = ctx.author().id.get();

    if !user_can_send_in(sctx, guild_id, ctx.author().id, channel).await {
        return send_error(
            ctx,
            "That channel isn't in this server, or you can't send messages there.",
        )
        .await;
    }

    let Some(session) = TEXT_SESSIONS.get(&user_id) else {
        return send_error(ctx, "No active embed to send.").await;
    };
    let embed = session.to_create_embed();
    drop(session);

    match channel
        .send_message(&sctx.http, CreateMessage::new().embed(embed))
        .await
    {
        Ok(_) => {
            TEXT_SESSIONS.remove(&user_id);
            send_embed(
                ctx,
                embeds::success_embed(
                    "Embed Creator",
                    &format!("Embed sent to <#{}>!", channel.get()),
                ),
            )
            .await
        }
        Err(_) => {
            send_error(ctx, "I couldn't send to that channel. Check my permissions.").await
        }
    }
}

/// Clear your embed draft.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "clear",
    required_permissions = "MANAGE_MESSAGES"
)]
async fn embed_clear(ctx: Context<'_>) -> Result<(), Error> {
    TEXT_SESSIONS.remove(&ctx.author().id.get());
    send_embed(
        ctx,
        embeds::success_embed("Embed Creator", "Embed session cleared."),
    )
    .await
}

// ---- EmbedCog helpers that require self.state -----------------------------

impl EmbedCog {
    /// Upload text to mystb.in, returning the paste link.
    async fn upload_to_mystbin(&self, text: &str) -> Option<String> {
        let body = json!({ "files": [{ "content": text, "filename": "custom_embed.json" }] });
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
        let json: Value = resp.json().await.ok()?;
        let id = json.get("id").and_then(|v| v.as_str())?;
        Some(format!("https://mystb.in/{id}"))
    }

    /// Fetch the first file's content from a mystb.in paste by id.
    async fn fetch_mystbin(&self, id: &str) -> Option<String> {
        let resp = self
            .state
            .http
            .get(format!("https://mystb.in/api/paste/{id}"))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: Value = resp.json().await.ok()?;
        json.get("files")
            .and_then(|f| f.as_array())
            .and_then(|a| a.first())
            .and_then(|f| f.get("content"))
            .and_then(|c| c.as_str())
            .map(str::to_string)
    }

    /// Parse an import payload: a raw JSON string, or a `https://mystb.in/<id>`
    /// link whose paste content is JSON.
    async fn parse_import(&self, input: &str) -> Result<EmbedData, String> {
        let input = input.trim();
        let raw = if let Some(rest) = input
            .strip_prefix("https://mystb.in/")
            .or_else(|| input.strip_prefix("http://mystb.in/"))
        {
            let id = rest.split(['/', '?', '#']).next().unwrap_or("");
            if id.is_empty() {
                return Err("That doesn't look like a valid Mystbin link.".to_string());
            }
            match self.fetch_mystbin(id).await {
                Some(c) => c,
                None => return Err("Couldn't fetch that Mystbin paste.".to_string()),
            }
        } else {
            input.to_string()
        };
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| "This doesn't seem to be valid JSON or a Mystbin link.".to_string())?;
        Ok(EmbedData::from_json(&value))
    }
}

// ---- free helpers: interaction responses ----------------------------------

/// Whether `user_id` may have the bot post into `channel`: the channel must
/// belong to `guild_id` and the user must hold Send Messages there. Denies
/// on a cold cache rather than guessing.
async fn user_can_send_in(
    ctx: &serenity::all::Context,
    guild_id: GuildId,
    user_id: UserId,
    channel: ChannelId,
) -> bool {
    let gc = match channel.to_channel(&ctx.http).await {
        Ok(Channel::Guild(gc)) if gc.guild_id == guild_id => gc,
        _ => return false,
    };
    let Ok(member) = guild_id.member(&ctx.http, user_id).await else {
        return false;
    };
    match ctx.cache.guild(guild_id) {
        Some(g) => {
            let p = g.user_permissions_in(&gc, &member);
            p.contains(Permissions::ADMINISTRATOR) || p.contains(Permissions::SEND_MESSAGES)
        }
        None => false,
    }
}

/// Respond to a component with an `UpdateMessage` (embed + components).
async fn update(
    ctx: &serenity::all::Context,
    interaction: &ComponentInteraction,
    embed: CreateEmbed,
    components: Vec<CreateActionRow>,
) {
    let _ = interaction
        .create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(embed)
                    .components(components),
            ),
        )
        .await;
}

/// Respond to a component with an `UpdateMessage` that strips all controls.
async fn update_no_components(
    ctx: &serenity::all::Context,
    interaction: &ComponentInteraction,
    embed: CreateEmbed,
) {
    let _ = interaction
        .create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(embed)
                    .components(vec![]),
            ),
        )
        .await;
}

/// Send a private (ephemeral) embed in response to a component.
async fn ephemeral(
    ctx: &serenity::all::Context,
    interaction: &ComponentInteraction,
    embed: CreateEmbed,
) {
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

// ---- free helpers: components ---------------------------------------------

/// The main builder control layout (4 rows of buttons).
fn build_main_components(data: &EmbedData) -> Vec<CreateActionRow> {
    let edit_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_AUTHOR)
            .label("Author")
            .style(ButtonStyle::Primary)
            .emoji('📝'),
        CreateButton::new(BTN_BASE)
            .label("Base")
            .style(ButtonStyle::Primary)
            .emoji('🗒'),
        CreateButton::new(BTN_IMAGES)
            .label("Images")
            .style(ButtonStyle::Primary)
            .emoji('🖼'),
        CreateButton::new(BTN_FOOTER)
            .label("Footer")
            .style(ButtonStyle::Primary)
            .emoji('📜'),
    ]);
    let field_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_ADDFIELD)
            .label("Add Field")
            .style(ButtonStyle::Success)
            .emoji('➕'),
        CreateButton::new(BTN_REMOVEFIELD)
            .label("Remove Field")
            .style(ButtonStyle::Danger)
            .emoji('➖')
            .disabled(data.fields.is_empty()),
        CreateButton::new(BTN_IMPORT)
            .label("Import")
            .style(ButtonStyle::Secondary)
            .emoji('📥'),
    ]);
    let io_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_SEND)
            .label("Send")
            .style(ButtonStyle::Success)
            .emoji('💬'),
        CreateButton::new(BTN_EXPORT_JSON)
            .label("Export JSON")
            .style(ButtonStyle::Secondary)
            .emoji('📤'),
        CreateButton::new(BTN_EXPORT_MYST)
            .label("Export to Mystbin")
            .style(ButtonStyle::Secondary)
            .emoji('🗄'),
    ]);
    let finish_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_COMPLETE)
            .label("Complete")
            .style(ButtonStyle::Success)
            .emoji('✅'),
        CreateButton::new(BTN_CANCEL)
            .label("Cancel")
            .style(ButtonStyle::Danger)
            .emoji('❌'),
    ]);
    vec![edit_row, field_row, io_row, finish_row]
}

/// The "remove a field" sub-view: a select of the existing fields + Back.
fn build_remove_components(data: &EmbedData) -> Vec<CreateActionRow> {
    let options: Vec<CreateSelectMenuOption> = data
        .fields
        .iter()
        .take(MAX_FIELDS)
        .enumerate()
        .map(|(i, f)| {
            let name = if f.name.trim().is_empty() || f.name == "\u{200b}" {
                "(no name)".to_string()
            } else {
                truncate(&f.name, 90).to_string()
            };
            CreateSelectMenuOption::new(format!("Field {}: {}", i + 1, name), i.to_string())
                .emoji('🗑')
        })
        .collect();
    vec![
        CreateActionRow::SelectMenu(
            CreateSelectMenu::new(SEL_REMOVE, CreateSelectMenuKind::String { options })
                .placeholder("Select a field to remove")
                .min_values(1)
                .max_values(1),
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(BTN_BACK)
                .label("Back")
                .style(ButtonStyle::Secondary)
                .emoji('↩'),
        ]),
    ]
}

/// The "send to a channel" sub-view: a channel select + Back.
fn build_send_components() -> Vec<CreateActionRow> {
    vec![
        CreateActionRow::SelectMenu(
            CreateSelectMenu::new(
                SEL_SEND,
                CreateSelectMenuKind::Channel {
                    channel_types: Some(vec![ChannelType::Text, ChannelType::News]),
                    default_channels: None,
                },
            )
            .placeholder("Select a channel to send to")
            .min_values(1)
            .max_values(1),
        ),
        CreateActionRow::Buttons(vec![
            CreateButton::new(BTN_BACK)
                .label("Back")
                .style(ButtonStyle::Secondary)
                .emoji('↩'),
        ]),
    ]
}

// ---- free helpers: modals -------------------------------------------------

fn short(custom_id: &str, label: &str, max: u16) -> CreateInputText {
    CreateInputText::new(InputTextStyle::Short, label, custom_id)
        .required(false)
        .max_length(max)
}

fn paragraph(custom_id: &str, label: &str, max: u16) -> CreateInputText {
    CreateInputText::new(InputTextStyle::Paragraph, label, custom_id)
        .required(false)
        .max_length(max)
}

/// Apply a pre-fill value when present.
fn prefill(input: CreateInputText, value: &Option<String>) -> CreateInputText {
    match value {
        Some(v) if !v.is_empty() => input.value(v.clone()),
        _ => input,
    }
}

fn build_author_modal(data: &EmbedData) -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(prefill(
            short("name", "Name", 256).placeholder("Author name"),
            &data.author_name,
        )),
        CreateActionRow::InputText(prefill(
            short("author_url", "URL", 1024).placeholder("Author URL (optional)"),
            &data.author_url,
        )),
        CreateActionRow::InputText(prefill(
            short("author_icon", "Icon URL", 1024).placeholder("Author icon URL (optional)"),
            &data.author_icon,
        )),
    ];
    CreateModal::new(MODAL_AUTHOR, "Edit Author").components(rows)
}

fn build_base_modal(data: &EmbedData) -> CreateModal {
    let color_value = data.color.map(|c| format!("#{c:06X}"));
    let rows = vec![
        CreateActionRow::InputText(prefill(
            short("title", "Title", 256).placeholder("Title"),
            &data.title,
        )),
        CreateActionRow::InputText(prefill(
            paragraph("description", "Description", 4000).placeholder("Description (optional)"),
            &data.description,
        )),
        CreateActionRow::InputText(prefill(
            short("color", "Color", 7).placeholder("#5865F2 (optional)"),
            &color_value,
        )),
        CreateActionRow::InputText(prefill(
            short("url", "Title URL", 1024).placeholder("Title URL (optional)"),
            &data.url,
        )),
    ];
    CreateModal::new(MODAL_BASE, "Edit Base").components(rows)
}

fn build_images_modal(data: &EmbedData) -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(prefill(
            short("image_url", "Image URL", 1024).placeholder("Image URL (optional)"),
            &data.image_url,
        )),
        CreateActionRow::InputText(prefill(
            short("thumbnail_url", "Thumbnail URL", 1024).placeholder("Thumbnail URL (optional)"),
            &data.thumbnail_url,
        )),
    ];
    CreateModal::new(MODAL_IMAGES, "Edit Images").components(rows)
}

fn build_footer_modal(data: &EmbedData) -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(prefill(
            paragraph("text", "Text", 2048).placeholder("Footer text"),
            &data.footer_text,
        )),
        CreateActionRow::InputText(prefill(
            short("footer_icon", "Icon URL", 1024).placeholder("Footer icon URL (optional)"),
            &data.footer_icon,
        )),
    ];
    CreateModal::new(MODAL_FOOTER, "Edit Footer").components(rows)
}

fn build_addfield_modal() -> CreateModal {
    let rows = vec![
        CreateActionRow::InputText(short("field_name", "Name", 256).placeholder("Field name")),
        CreateActionRow::InputText(
            paragraph("field_value", "Value", 1024).placeholder("Field value"),
        ),
        CreateActionRow::InputText(short("field_inline", "Inline", 5).placeholder("true / false")),
    ];
    CreateModal::new(MODAL_ADDFIELD, "Add Field").components(rows)
}

fn build_import_modal() -> CreateModal {
    let rows = vec![CreateActionRow::InputText(
        paragraph("import_link", "JSON or Mystbin link", 4000)
            .placeholder("https://mystb.in/SomeID or raw embed JSON"),
    )];
    CreateModal::new(MODAL_IMPORT, "Import Embed").components(rows)
}

// ---- free helpers: misc ---------------------------------------------------

/// Collect a modal's submitted text inputs into a `{custom_id: value}` map.
fn collect_inputs(interaction: &ModalInteraction) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for row in &interaction.data.components {
        for comp in &row.components {
            if let ActionRowComponent::InputText(it) = comp {
                map.insert(it.custom_id.clone(), it.value.clone().unwrap_or_default());
            }
        }
    }
    map
}

/// Parse a hex color like `#ff5733`, `ff5733`, or `0xff5733` into 0xRRGGBB.
fn parse_hex_color(s: &str) -> Option<u32> {
    let h = s
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if h.is_empty() || h.len() > 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}
