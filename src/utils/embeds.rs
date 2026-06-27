use super::colors;
use serenity::all::{CreateEmbed, Timestamp};

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

pub fn warning_embed(description: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title("Warning")
        .description(description)
        .color(colors::YELLOW)
        .timestamp(Timestamp::now())
}
