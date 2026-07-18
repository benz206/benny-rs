use super::MAX_FIELDS;
use crate::utils::colors;
use crate::utils::format::truncate;
use serde_json::{Value, json};
use serenity::all::{Colour, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, Timestamp};

// ---- the embed model ------------------------------------------------------

/// A full, editable representation of a Discord embed. Renders to a `CreateEmbed`
/// preview and round-trips through Discord's embed-object JSON format.
#[derive(Debug, Clone, Default)]
pub(super) struct EmbedData {
    pub(super) title: Option<String>,
    pub(super) description: Option<String>,
    pub(super) url: Option<String>,
    pub(super) color: Option<u32>,
    /// Unix seconds.
    pub(super) timestamp: Option<i64>,
    pub(super) author_name: Option<String>,
    pub(super) author_url: Option<String>,
    pub(super) author_icon: Option<String>,
    pub(super) footer_text: Option<String>,
    pub(super) footer_icon: Option<String>,
    pub(super) image_url: Option<String>,
    pub(super) thumbnail_url: Option<String>,
    pub(super) fields: Vec<EmbedField>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct EmbedField {
    pub(super) name: String,
    pub(super) value: String,
    pub(super) inline: bool,
}

/// Normalize a raw input into `Some(trimmed)` or `None` when blank.
pub(super) fn opt(s: &str) -> Option<String> {
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
    pub(super) fn starter() -> Self {
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
    pub(super) fn to_create_embed(&self) -> CreateEmbed {
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
    pub(super) fn to_json(&self) -> Value {
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
    pub(super) fn from_json(v: &Value) -> Self {
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

/// Parse a hex color like `#ff5733`, `ff5733`, or `0xff5733` into 0xRRGGBB.
pub(super) fn parse_hex_color(s: &str) -> Option<u32> {
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
