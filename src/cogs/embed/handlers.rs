use super::components::{
    build_addfield_modal, build_author_modal, build_base_modal, build_footer_modal,
    build_images_modal, build_import_modal, build_main_components, build_remove_components,
    build_send_components,
};
use super::model::{EmbedData, EmbedField, opt, parse_hex_color};
use super::{
    BTN_ADDFIELD, BTN_AUTHOR, BTN_BACK, BTN_BASE, BTN_CANCEL, BTN_COMPLETE, BTN_EXPORT_JSON,
    BTN_EXPORT_MYST, BTN_FOOTER, BTN_IMAGES, BTN_IMPORT, BTN_REMOVEFIELD, BTN_SEND, BUILDERS,
    EmbedCog, ID_PREFIX, MAX_FIELDS, MODAL_ADDFIELD, MODAL_AUTHOR, MODAL_BASE, MODAL_FOOTER,
    MODAL_IMAGES, MODAL_IMPORT, SEL_REMOVE, SEL_SEND,
};
use crate::cogs::Cog;
use crate::utils::format::truncate;
use crate::utils::{colors, embeds, interactions};
use async_trait::async_trait;
use serde_json::{Value, json};
use serenity::all::{
    ActionRowComponent, Channel, ChannelId, ComponentInteraction, ComponentInteractionDataKind,
    CreateActionRow, CreateAttachment, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateMessage, GuildId,
    ModalInteraction, Permissions, Timestamp, UserId,
};
use std::collections::HashMap;

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
                interactions::respond_ephemeral(
                    ctx,
                    interaction,
                    embeds::error_embed("This embed builder has expired."),
                )
                .await;
                return;
            }
        };
        if interaction.user.id.get() != owner {
            interactions::respond_ephemeral(
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
                    interactions::respond_ephemeral(
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
                        interactions::respond_ephemeral(
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
                    interactions::respond_ephemeral_modal(ctx, interaction, embeds::error_embed(&e))
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
pub(super) async fn user_can_send_in(
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
