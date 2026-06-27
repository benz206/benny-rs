use super::Cog;
use crate::state::AppState;
use crate::utils::format::truncate;
use crate::utils::{colors, embeds, parse};
use async_trait::async_trait;
use dashmap::DashMap;
use serde_json::{json, Value};
use serenity::all::{
    ActionRowComponent, ButtonStyle, ChannelId, ChannelType, Colour, ComponentInteraction,
    ComponentInteractionDataKind, Context, CreateActionRow, CreateAttachment, CreateButton,
    CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInputText, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage, CreateModal,
    CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, InputTextStyle, Message,
    ModalInteraction, Timestamp,
};
use std::collections::HashMap;
use std::sync::Arc;

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
    /// The default starter embed, mirroring the Python `custom_embed` command
    /// (title "Embed Creator", a hint description, current timestamp).
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
            let name = if f.name.trim().is_empty() { "\u{200b}" } else { truncate(&f.name, 256) };
            let value = if f.value.trim().is_empty() { "\u{200b}" } else { truncate(&f.value, 1024) };
            e = e.field(name, value, f.inline);
            empty = false;
        }
        if let Some(t) = self.timestamp.and_then(|ts| Timestamp::from_unix_timestamp(ts).ok()) {
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
        if let Some(t) = self.timestamp.and_then(|ts| Timestamp::from_unix_timestamp(ts).ok()) {
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
/// enforces the Python `interaction_check` (only the invoker may drive it).
struct Builder {
    data: EmbedData,
    owner_id: u64,
}

pub struct EmbedCog {
    state: Arc<AppState>,
    /// Interactive builder sessions, keyed by builder message id.
    builders: DashMap<u64, Builder>,
    /// Legacy text-command sessions, keyed by user id.
    text_sessions: DashMap<u64, EmbedData>,
}

impl EmbedCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            builders: DashMap::new(),
            text_sessions: DashMap::new(),
        })
    }
}

#[async_trait]
impl Cog for EmbedCog {
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
        let mut it = body.splitn(3, ' ');
        let Some(cmd) = it.next() else { return };
        if !matches!(cmd, "embed" | "customembed" | "cembed" | "ce") {
            return;
        }
        let subcmd = it.next().unwrap_or("").trim();
        let arg = it.next().unwrap_or("").trim();
        let user_id = msg.author.id.get();

        match subcmd {
            // Interactive builder.
            "" | "create" | "new" | "builder" | "edit" | "make" => {
                self.open_builder(ctx, msg, EmbedData::starter()).await;
            }
            // Import then open the interactive builder pre-filled.
            "import" | "load" => {
                if arg.is_empty() {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: `embed import <json | https://mystb.in/...>`")
                        .await;
                    return;
                }
                match self.parse_import(arg).await {
                    Ok(data) => self.open_builder(ctx, msg, data).await,
                    Err(e) => {
                        let _ = msg
                            .channel_id
                            .send_message(&ctx.http, CreateMessage::new().embed(embeds::error_embed(&e)))
                            .await;
                    }
                }
            }
            // Legacy text subcommands (operate on a per-user session).
            "title" | "description" | "desc" | "color" | "colour" | "author" | "footer"
            | "field" | "preview" | "show" | "send" | "clear" | "reset" => {
                self.handle_text(ctx, msg, user_id, subcmd, arg).await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::info_embed(
                            "Embed Creator",
                            "`embed create` — open the interactive builder\n\
                             `embed import <json|mystbin>` — import then edit\n\n\
                             Quick text commands:\n\
                             `embed title <text>` · `embed description <text>` · `embed color <hex>`\n\
                             `embed author <text>` · `embed footer <text>` · `embed field <name> | <value>`\n\
                             `embed preview` · `embed send <#channel>` · `embed clear`",
                        )),
                    )
                    .await;
            }
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let msg_id = interaction.message.id.get();

        // Ownership / session check.
        let owner = match self.builders.get(&msg_id) {
            Some(b) => b.owner_id,
            None => {
                self.ephemeral(ctx, interaction, embeds::error_embed("This embed builder has expired."))
                    .await;
                return;
            }
        };
        if interaction.user.id.get() != owner {
            self.ephemeral(
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
                let data = self.builders.get(&msg_id).map(|b| b.data.clone()).unwrap_or_default();
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
                let full = self
                    .builders
                    .get(&msg_id)
                    .map(|b| b.data.fields.len() >= MAX_FIELDS)
                    .unwrap_or(false);
                if full {
                    self.ephemeral(
                        ctx,
                        interaction,
                        embeds::warning_embed("This embed already has the maximum of 25 fields."),
                    )
                    .await;
                } else {
                    let _ = interaction
                        .create_response(&ctx.http, CreateInteractionResponse::Modal(build_addfield_modal()))
                        .await;
                }
            }
            BTN_IMPORT => {
                let _ = interaction
                    .create_response(&ctx.http, CreateInteractionResponse::Modal(build_import_modal()))
                    .await;
            }
            // Switch to the "remove a field" sub-view.
            BTN_REMOVEFIELD => {
                let (embed, components) = {
                    let b = self.builders.get(&msg_id);
                    let Some(b) = b else { return };
                    if b.data.fields.is_empty() {
                        drop(b);
                        self.ephemeral(
                            ctx,
                            interaction,
                            embeds::warning_embed("There are no fields to remove."),
                        )
                        .await;
                        return;
                    }
                    (b.data.to_create_embed(), build_remove_components(&b.data))
                };
                self.update(ctx, interaction, embed, components).await;
            }
            // Switch to the "send to a channel" sub-view.
            BTN_SEND => {
                let embed = self
                    .builders
                    .get(&msg_id)
                    .map(|b| b.data.to_create_embed())
                    .unwrap_or_default();
                self.update(ctx, interaction, embed, build_send_components()).await;
            }
            // Back to the main builder view.
            BTN_BACK => {
                let (embed, components) = {
                    let b = self.builders.get(&msg_id);
                    let Some(b) = b else { return };
                    (b.data.to_create_embed(), build_main_components(&b.data))
                };
                self.update(ctx, interaction, embed, components).await;
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
                    let mut b = match self.builders.get_mut(&msg_id) {
                        Some(b) => b,
                        None => return,
                    };
                    if let Some(i) = idx.filter(|&i| i < b.data.fields.len()) {
                        b.data.fields.remove(i);
                    }
                    (b.data.to_create_embed(), build_main_components(&b.data))
                };
                self.update(ctx, interaction, embed, components).await;
            }
            // A channel was chosen to send to.
            SEL_SEND => {
                let channel = match &interaction.data.kind {
                    ComponentInteractionDataKind::ChannelSelect { values } => values.first().copied(),
                    _ => None,
                };
                let (embed, components, preview) = {
                    let b = match self.builders.get(&msg_id) {
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
                self.update(ctx, interaction, embed, components).await;
                if let Some(channel) = channel {
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
                let pretty = self
                    .builders
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
                let pretty = self
                    .builders
                    .get(&msg_id)
                    .map(|b| serde_json::to_string_pretty(&b.data.to_json()).unwrap_or_default())
                    .unwrap_or_default();
                let followup = match self.upload_to_mystbin(&pretty).await {
                    Some(link) => CreateInteractionResponseFollowup::new()
                        .embed(embeds::success_embed("Exported to Mystbin", &link))
                        .ephemeral(true),
                    None => CreateInteractionResponseFollowup::new()
                        .embed(embeds::error_embed("Failed to upload to Mystbin. Try again later."))
                        .ephemeral(true),
                };
                let _ = interaction.create_followup(&ctx.http, followup).await;
            }
            // Discard the session and strip the controls.
            BTN_CANCEL => {
                self.builders.remove(&msg_id);
                self.update_no_components(
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
                let embed = self
                    .builders
                    .get(&msg_id)
                    .map(|b| b.data.to_create_embed())
                    .unwrap_or_default();
                self.builders.remove(&msg_id);
                self.update_no_components(ctx, interaction, embed).await;
            }
            _ => {}
        }
    }

    async fn on_modal(&self, ctx: &Context, interaction: &ModalInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let Some(msg) = interaction.message.as_ref() else {
            return;
        };
        let msg_id = msg.id.get();

        // Ownership / session check.
        let owner = match self.builders.get(&msg_id) {
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
                        let mut b = match self.builders.get_mut(&msg_id) {
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
            let mut b = match self.builders.get_mut(&msg_id) {
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
                    d.thumbnail_url = inputs.get("thumbnail_url").map(|s| s.as_str()).and_then(opt);
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
                        .map(|s| matches!(s.trim().to_lowercase().as_str(), "true" | "yes" | "1" | "y"))
                        .unwrap_or(false);
                    if (!name.is_empty() || !value.is_empty()) && d.fields.len() < MAX_FIELDS {
                        d.fields.push(EmbedField {
                            name: if name.is_empty() { "\u{200b}".to_string() } else { truncate(name, 256).to_string() },
                            value: if value.is_empty() { "\u{200b}".to_string() } else { truncate(value, 1024).to_string() },
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

impl EmbedCog {
    // ---- interactive builder helpers --------------------------------------

    /// Post a fresh builder message and register its session.
    async fn open_builder(&self, ctx: &Context, msg: &Message, data: EmbedData) {
        let builder = CreateMessage::new()
            .reference_message(msg)
            .embed(data.to_create_embed())
            .components(build_main_components(&data));
        match msg.channel_id.send_message(&ctx.http, builder).await {
            Ok(sent) => {
                self.builders.insert(
                    sent.id.get(),
                    Builder {
                        data,
                        owner_id: msg.author.id.get(),
                    },
                );
            }
            Err(e) => tracing::error!(error = ?e, "failed to open embed builder"),
        }
    }

    /// Respond to a component with an `UpdateMessage` (embed + components).
    async fn update(
        &self,
        ctx: &Context,
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
        &self,
        ctx: &Context,
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
    async fn ephemeral(&self, ctx: &Context, interaction: &ComponentInteraction, embed: CreateEmbed) {
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

    // ---- import / export over Mystbin -------------------------------------

    /// Parse an import payload: a raw JSON string, or a `https://mystb.in/<id>`
    /// link whose paste content is JSON. Mirrors the Python import modal.
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

    /// Upload text to mystb.in, returning the paste link (mirrors ocr.rs).
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

    // ---- legacy text subcommands ------------------------------------------

    async fn handle_text(&self, ctx: &Context, msg: &Message, user_id: u64, subcmd: &str, arg: &str) {
        match subcmd {
            "title" => {
                self.text_sessions.entry(user_id).or_default().title = opt(arg);
                self.text_ack(ctx, msg, "Title set.").await;
            }
            "description" | "desc" => {
                self.text_sessions.entry(user_id).or_default().description = opt(arg);
                self.text_ack(ctx, msg, "Description set.").await;
            }
            "color" | "colour" => match parse_hex_color(arg) {
                Some(c) => {
                    self.text_sessions.entry(user_id).or_default().color = Some(c);
                    self.text_ack(ctx, msg, &format!("Color set to #{c:06X}.")).await;
                }
                None => {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Invalid hex color. Example: `embed color ff5733`")
                        .await;
                }
            },
            "author" => {
                self.text_sessions.entry(user_id).or_default().author_name = opt(arg);
                self.text_ack(ctx, msg, "Author set.").await;
            }
            "footer" => {
                self.text_sessions.entry(user_id).or_default().footer_text = opt(arg);
                self.text_ack(ctx, msg, "Footer set.").await;
            }
            "field" => {
                let Some((name, value)) = arg.split_once('|') else {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: `embed field <name> | <value>`")
                        .await;
                    return;
                };
                let mut session = self.text_sessions.entry(user_id).or_default();
                if session.fields.len() >= MAX_FIELDS {
                    drop(session);
                    let _ = msg.channel_id.say(&ctx.http, "This embed already has 25 fields.").await;
                    return;
                }
                session.fields.push(EmbedField {
                    name: truncate(name.trim(), 256).to_string(),
                    value: truncate(value.trim(), 1024).to_string(),
                    inline: false,
                });
                drop(session);
                self.text_ack(ctx, msg, "Field added.").await;
            }
            "preview" | "show" => match self.text_sessions.get(&user_id) {
                Some(session) => {
                    let embed = session.to_create_embed();
                    drop(session);
                    let _ = msg
                        .channel_id
                        .send_message(&ctx.http, CreateMessage::new().embed(embed))
                        .await;
                }
                None => {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "No active embed. Start with `embed title <text>` or `embed create`.")
                        .await;
                }
            },
            "send" => {
                let Some(channel_id) = parse::parse_channel_id(arg) else {
                    let _ = msg.channel_id.say(&ctx.http, "Usage: `embed send <#channel>`").await;
                    return;
                };
                match self.text_sessions.get(&user_id) {
                    Some(session) => {
                        let embed = session.to_create_embed();
                        drop(session);
                        match ChannelId::new(channel_id)
                            .send_message(&ctx.http, CreateMessage::new().embed(embed))
                            .await
                        {
                            Ok(_) => {
                                self.text_sessions.remove(&user_id);
                                self.text_ack(ctx, msg, &format!("Embed sent to <#{channel_id}>!")).await;
                            }
                            Err(_) => {
                                let _ = msg
                                    .channel_id
                                    .say(&ctx.http, "I couldn't send to that channel. Check my permissions.")
                                    .await;
                            }
                        }
                    }
                    None => {
                        let _ = msg.channel_id.say(&ctx.http, "No active embed to send.").await;
                    }
                }
            }
            "clear" | "reset" => {
                self.text_sessions.remove(&user_id);
                self.text_ack(ctx, msg, "Embed session cleared.").await;
            }
            _ => {}
        }
    }

    async fn text_ack(&self, ctx: &Context, msg: &Message, text: &str) {
        let _ = msg
            .channel_id
            .send_message(&ctx.http, CreateMessage::new().embed(embeds::success_embed("Embed Creator", text)))
            .await;
    }
}

// ---- free helpers: components ---------------------------------------------

/// The main builder control layout (4 rows of buttons).
fn build_main_components(data: &EmbedData) -> Vec<CreateActionRow> {
    let edit_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_AUTHOR).label("Author").style(ButtonStyle::Primary).emoji('📝'),
        CreateButton::new(BTN_BASE).label("Base").style(ButtonStyle::Primary).emoji('🗒'),
        CreateButton::new(BTN_IMAGES).label("Images").style(ButtonStyle::Primary).emoji('🖼'),
        CreateButton::new(BTN_FOOTER).label("Footer").style(ButtonStyle::Primary).emoji('📜'),
    ]);
    let field_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_ADDFIELD).label("Add Field").style(ButtonStyle::Success).emoji('➕'),
        CreateButton::new(BTN_REMOVEFIELD)
            .label("Remove Field")
            .style(ButtonStyle::Danger)
            .emoji('➖')
            .disabled(data.fields.is_empty()),
        CreateButton::new(BTN_IMPORT).label("Import").style(ButtonStyle::Secondary).emoji('📥'),
    ]);
    let io_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_SEND).label("Send").style(ButtonStyle::Success).emoji('💬'),
        CreateButton::new(BTN_EXPORT_JSON).label("Export JSON").style(ButtonStyle::Secondary).emoji('📤'),
        CreateButton::new(BTN_EXPORT_MYST).label("Export to Mystbin").style(ButtonStyle::Secondary).emoji('🗄'),
    ]);
    let finish_row = CreateActionRow::Buttons(vec![
        CreateButton::new(BTN_COMPLETE).label("Complete").style(ButtonStyle::Success).emoji('✅'),
        CreateButton::new(BTN_CANCEL).label("Cancel").style(ButtonStyle::Danger).emoji('❌'),
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
            CreateSelectMenuOption::new(format!("Field {}: {}", i + 1, name), i.to_string()).emoji('🗑')
        })
        .collect();
    vec![
        CreateActionRow::SelectMenu(
            CreateSelectMenu::new(SEL_REMOVE, CreateSelectMenuKind::String { options })
                .placeholder("Select a field to remove")
                .min_values(1)
                .max_values(1),
        ),
        CreateActionRow::Buttons(vec![CreateButton::new(BTN_BACK)
            .label("Back")
            .style(ButtonStyle::Secondary)
            .emoji('↩')]),
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
        CreateActionRow::Buttons(vec![CreateButton::new(BTN_BACK)
            .label("Back")
            .style(ButtonStyle::Secondary)
            .emoji('↩')]),
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
        CreateActionRow::InputText(
            prefill(short("name", "Name", 256).placeholder("Author name"), &data.author_name),
        ),
        CreateActionRow::InputText(
            prefill(short("author_url", "URL", 1024).placeholder("Author URL (optional)"), &data.author_url),
        ),
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
        CreateActionRow::InputText(prefill(short("title", "Title", 256).placeholder("Title"), &data.title)),
        CreateActionRow::InputText(prefill(
            paragraph("description", "Description", 4000).placeholder("Description (optional)"),
            &data.description,
        )),
        CreateActionRow::InputText(prefill(
            short("color", "Color", 7).placeholder("#5865F2 (optional)"),
            &color_value,
        )),
        CreateActionRow::InputText(prefill(short("url", "Title URL", 1024).placeholder("Title URL (optional)"), &data.url)),
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
        CreateActionRow::InputText(prefill(paragraph("text", "Text", 2048).placeholder("Footer text"), &data.footer_text)),
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
        CreateActionRow::InputText(paragraph("field_value", "Value", 1024).placeholder("Field value")),
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
    let h = s.trim().trim_start_matches('#').trim_start_matches("0x").trim_start_matches("0X");
    if h.is_empty() || h.len() > 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}
