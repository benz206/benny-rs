use serenity::all::{Context, CreateCommand, Interaction};

pub async fn register_global(ctx: &Context) {
    let _ = serenity::all::Command::create_global_command(&ctx.http, CreateCommand::new("ping").description("Latency check")).await;
}

pub async fn handle_interaction(ctx: &Context, interaction: &Interaction) {
    if let Some(app) = interaction.as_command() {
        if app.data.name == "ping" {
            let _ = app.create_response(&ctx.http, serenity::all::CreateInteractionResponse::Message(
                serenity::all::CreateInteractionResponseMessage::new().content("Pong!")
            )).await;
        }
    }
}


