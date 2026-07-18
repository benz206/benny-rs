use super::embeds;
use serenity::all::{
    ComponentInteraction, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseMessage, ModalInteraction,
};

/// Send a private (ephemeral) embed in response to a component interaction.
pub async fn respond_ephemeral(
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

/// Send a private (ephemeral) error embed in response to a component interaction.
pub async fn respond_ephemeral_error(
    ctx: &serenity::all::Context,
    interaction: &ComponentInteraction,
    text: &str,
) {
    respond_ephemeral(ctx, interaction, embeds::error_embed(text)).await;
}

/// Send a private (ephemeral) plain-text message in response to a component interaction.
pub async fn respond_ephemeral_text(
    ctx: &serenity::all::Context,
    interaction: &ComponentInteraction,
    text: &str,
) {
    let _ = interaction
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(text)
                    .ephemeral(true),
            ),
        )
        .await;
}

/// Send a private (ephemeral) embed in response to a modal interaction.
pub async fn respond_ephemeral_modal(
    ctx: &serenity::all::Context,
    interaction: &ModalInteraction,
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
