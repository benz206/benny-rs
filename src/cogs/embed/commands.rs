use super::components::build_main_components;
use super::handlers::user_can_send_in;
use super::model::{EmbedData, EmbedField, opt, parse_hex_color};
use super::{BUILDERS, Builder, MAX_BUILDERS, MAX_FIELDS, TEXT_SESSIONS};
use crate::framework::{Context, Error, send_embed, send_error};
use crate::utils::embeds;
use crate::utils::format::truncate;
use serenity::all::{ChannelId, CreateMessage};

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
pub(super) async fn embed(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Open the interactive embed builder.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Embed",
    rename = "new",
    aliases("custom_embed", "cembed", "ce"),
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
    // Cap the live-session map so abandoned builders can't leak memory over a
    // long uptime (every other interactive map here is bounded the same way).
    crate::utils::cache::bounded_insert(
        &BUILDERS,
        sent.id.get(),
        Builder {
            data,
            owner_id: ctx.author().id.get(),
        },
        MAX_BUILDERS,
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
