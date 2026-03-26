use serenity::all::{CreateEmbed, Timestamp};
use super::colors;

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

pub fn info_embed(title: &str, description: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(title)
        .description(description)
        .color(colors::BLURPLE)
        .timestamp(Timestamp::now())
}
