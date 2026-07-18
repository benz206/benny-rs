use super::colors;
use serenity::all::{Colour, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, Timestamp};

pub fn success_embed(title: &str, description: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(title)
        .description(description)
        .color(colors::GREEN)
        .timestamp(Timestamp::now())
}

pub fn error_embed(description: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title("Error")
        .description(description)
        .color(colors::RED)
        .timestamp(Timestamp::now())
}

pub fn warning_embed(description: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title("Warning")
        .description(description)
        .color(colors::YELLOW)
        .timestamp(Timestamp::now())
}

/// Deserialize a TagScript embed JSON object into a serenity `CreateEmbed`.
pub fn json_to_embed(v: &serde_json::Value) -> CreateEmbed {
    let mut embed = CreateEmbed::new();

    if let Some(title) = v.get("title").and_then(|x| x.as_str()) {
        embed = embed.title(title);
    }
    if let Some(desc) = v.get("description").and_then(|x| x.as_str()) {
        embed = embed.description(desc);
    }
    if let Some(color) = v.get("color").and_then(|x| x.as_u64()) {
        embed = embed.color(Colour(color as u32));
    }
    if let Some(url) = v.get("url").and_then(|x| x.as_str()) {
        embed = embed.url(url);
    }
    if let Some(fields) = v.get("fields").and_then(|x| x.as_array()) {
        for f in fields {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("\u{200B}");
            let value = f
                .get("value")
                .and_then(|x| x.as_str())
                .unwrap_or("\u{200B}");
            let inline = f.get("inline").and_then(|x| x.as_bool()).unwrap_or(false);
            embed = embed.field(name, value, inline);
        }
    }
    if let Some(url) = v
        .get("thumbnail")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.thumbnail(url);
    }
    if let Some(url) = v
        .get("image")
        .and_then(|t| t.get("url"))
        .and_then(|x| x.as_str())
    {
        embed = embed.image(url);
    }
    if let Some(text) = v
        .get("footer")
        .and_then(|t| t.get("text"))
        .and_then(|x| x.as_str())
    {
        embed = embed.footer(CreateEmbedFooter::new(text));
    }
    if let Some(author) = v.get("author")
        && let Some(name) = author.get("name").and_then(|x| x.as_str()) {
            let mut a = CreateEmbedAuthor::new(name);
            if let Some(icon) = author.get("icon_url").and_then(|x| x.as_str()) {
                a = a.icon_url(icon);
            }
            embed = embed.author(a);
        }

    embed
}
