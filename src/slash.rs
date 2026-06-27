use crate::cogs::translate::TranslateCog;
use serenity::all::{CommandType, Context, CreateCommand, Interaction};

pub async fn register_global(ctx: &Context) {
    let _ = serenity::all::Command::create_global_command(
        &ctx.http,
        CreateCommand::new("ping").description("Latency check"),
    )
    .await;
    // Message context menu: right-click a message → Apps → Translate
    let _ = serenity::all::Command::create_global_command(
        &ctx.http,
        CreateCommand::new("Translate").kind(CommandType::Message),
    )
    .await;
}

pub async fn handle_interaction(ctx: &Context, interaction: &Interaction, translate: &TranslateCog) {
    if let Some(app) = interaction.as_command() {
        match app.data.name.as_str() {
            "ping" => {
                let _ = app
                    .create_response(
                        &ctx.http,
                        serenity::all::CreateInteractionResponse::Message(
                            serenity::all::CreateInteractionResponseMessage::new().content("Pong!"),
                        ),
                    )
                    .await;
            }
            "Translate" if app.data.kind == CommandType::Message => {
                translate.handle_context_menu(ctx, app).await;
            }
            _ => {}
        }
    }
}
