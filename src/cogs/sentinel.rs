use super::Cog;
use crate::entities::{sentinel_config, sentinels_decancer};
use crate::state::{AppState, SentinelConfig};
use crate::utils::{colors, embeds, parse, perms};
use async_trait::async_trait;
use dashmap::DashMap;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use serenity::all::{
    ActionRowComponent, ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow,
    CreateButton, CreateEmbed, CreateEmbedFooter, CreateInputText, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateModal, EditMember, GetMessages, GuildId,
    InputTextStyle, Member, Message, ModalInteraction, Permissions, Timestamp, UserId,
};
use std::sync::Arc;

/// custom_id namespace for this cog's interactive components. `on_component` and
/// `on_modal` are fanned out to every cog, so we early-return unless the id
/// belongs to us.
const ID_PREFIX: &str = "sent:";
const BTN_THRESH_A: &str = "sent:thresh_a";
const BTN_THRESH_B: &str = "sent:thresh_b";
const MODAL_THRESH_A: &str = "sent:modal_a";
const MODAL_THRESH_B: &str = "sent:modal_b";

/// The seven toxicity categories: (db column / input id, display label).
const CATEGORIES: [(&str, &str); 7] = [
    ("toxicity", "Toxicity"),
    ("severe_toxicity", "Severe Toxicity"),
    ("obscene", "Obscene"),
    ("identity_attack", "Identity Attack"),
    ("insult", "Insult"),
    ("threat", "Threat"),
    ("sexual_explicit", "Sexual Explicit"),
];

/// Per-guild decancer settings. Kept in an internal cache because `AppState` has
/// no `decancer_cache` field.
#[derive(Debug, Clone, Copy)]
struct DecancerConfig {
    enabled: bool,
    log_channel_id: Option<i64>,
}

pub struct SentinelCog {
    state: Arc<AppState>,
    /// Per-guild "delete flagged messages" toggle. Not part of `SentinelConfig`
    /// (state.rs), so kept here, hydrated from a `delete_flagged` column added
    /// idempotently in `on_ready`.
    delete_flags: DashMap<u64, bool>,
    /// Per-guild decancer config.
    decancer_cache: DashMap<u64, DecancerConfig>,
}

impl SentinelCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self {
            state,
            delete_flags: DashMap::new(),
            decancer_cache: DashMap::new(),
        })
    }
}

#[async_trait]
impl Cog for SentinelCog {
    async fn on_ready(&self, _ctx: &Context) {
        // Load sentinel configs.
        let rows = sentinel_config::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();

        for m in rows {
            self.state.sentinel_cache.insert(
                m.guild_id as u64,
                SentinelConfig {
                    enabled: m.enabled,
                    log_channel_id: m.log_channel_id,
                    toxicity: m.toxicity,
                    severe_toxicity: m.severe_toxicity,
                    obscene: m.obscene,
                    threat: m.threat,
                    insult: m.insult,
                    identity_attack: m.identity_attack,
                    sexual_explicit: m.sexual_explicit,
                },
            );
            self.delete_flags
                .insert(m.guild_id as u64, m.delete_flagged);
        }

        // Load decancer configs.
        let drows = sentinels_decancer::Entity::find()
            .all(self.state.servers_orm())
            .await
            .unwrap_or_default();
        for m in drows {
            self.decancer_cache.insert(
                m.guild_id as u64,
                DecancerConfig {
                    enabled: m.enabled,
                    log_channel_id: m.log_channel_id,
                },
            );
        }

        tracing::info!("Sentinel + decancer configs loaded");
    }

    async fn on_message(&self, ctx: &Context, msg: &Message) {
        if msg.author.bot {
            return;
        }
        let guild_id = match msg.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let content = msg.content.trim();
        let prefix = self.state.prefix().to_string();

        // Management commands. A prefixed message that isn't ours is some other
        // cog's command, so it should never be scanned for toxicity either.
        if content.starts_with(&prefix) {
            let body = content[prefix.len()..].trim();
            let mut it = body.splitn(3, ' ');
            match it.next() {
                Some("sentinel") => {
                    let sub = it.next().unwrap_or("");
                    let arg = it.next().unwrap_or("").trim();
                    self.handle_sentinel_cmd(ctx, msg, guild_id, sub, arg).await;
                }
                Some("decancer") => {
                    let sub = it.next().unwrap_or("");
                    let arg = it.next().unwrap_or("").trim();
                    self.handle_decancer_cmd(ctx, msg, guild_id, sub, arg).await;
                }
                _ => {}
            }
            return;
        }

        // Toxicity scan: only for sufficiently long, non-command messages in a
        // sentinel-enabled guild (minimum 25 characters).
        if msg.content.chars().count() <= 25 {
            return;
        }
        let config = match self.state.sentinel_cache.get(&guild_id) {
            Some(c) if c.enabled => c.clone(),
            _ => return,
        };
        let Some(api_url) = self.state.config.sentiment_api_url.clone() else {
            return;
        };
        self.check_toxicity(ctx, msg, guild_id, &config, &api_url)
            .await;
    }

    async fn on_member_join(&self, ctx: &Context, member: &Member) {
        let guild_id = member.guild_id.get();
        let cfg = match self.decancer_cache.get(&guild_id) {
            Some(c) if c.enabled => *c,
            _ => return,
        };

        let original = member.display_name().to_string();
        let cleaned = decancer_name(&original);

        if cleaned != original && !cleaned.trim().is_empty() {
            let _ = member
                .guild_id
                .edit_member(
                    &ctx.http,
                    member.user.id,
                    EditMember::new().nickname(cleaned.clone()),
                )
                .await;
        }

        if let Some(log_id) = cfg.log_channel_id {
            let icon = member
                .user
                .avatar_url()
                .unwrap_or_else(|| member.user.default_avatar_url());
            let embed = decancer_embed(
                "Decancer Automatic Action",
                &original,
                &cleaned,
                member.user.id.get(),
                icon,
            );
            let _ = ChannelId::new(log_id as u64)
                .send_message(&ctx.http, CreateMessage::new().embed(embed))
                .await;
        }
    }

    async fn on_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let guild_id = match interaction.guild_id {
            Some(g) => g.get(),
            None => return,
        };

        let config = self
            .state
            .sentinel_cache
            .get(&guild_id)
            .map(|c| c.clone())
            .unwrap_or_else(default_config);

        // The config buttons are visible to anyone in the channel; only let a
        // member with Manage Server actually open the threshold editor.
        if !perms::has_perm(
            ctx,
            GuildId::new(guild_id),
            interaction.user.id.get(),
            Permissions::MANAGE_GUILD,
        )
        .await
        {
            let _ = interaction
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content(
                                "You need the **Manage Server** permission to configure Sentinel.",
                            ),
                    ),
                )
                .await;
            return;
        }

        let modal = match cid {
            BTN_THRESH_A => build_modal_a(&config),
            BTN_THRESH_B => build_modal_b(&config),
            _ => return,
        };

        let _ = interaction
            .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
            .await;
    }

    async fn on_modal(&self, ctx: &Context, interaction: &ModalInteraction) {
        let cid = interaction.data.custom_id.as_str();
        if !cid.starts_with(ID_PREFIX) {
            return;
        }
        let guild_id = match interaction.guild_id {
            Some(g) => g.get(),
            None => return,
        };

        // Re-check on submit — a forged modal submission must not bypass the
        // button-level Manage Server gate.
        if !perms::has_perm(
            ctx,
            GuildId::new(guild_id),
            interaction.user.id.get(),
            Permissions::MANAGE_GUILD,
        )
        .await
        {
            let _ = interaction
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .ephemeral(true)
                            .content(
                                "You need the **Manage Server** permission to configure Sentinel.",
                            ),
                    ),
                )
                .await;
            return;
        }

        // Flatten the submitted input rows into (custom_id, value) pairs.
        let mut inputs: Vec<(String, String)> = Vec::new();
        for row in &interaction.data.components {
            for comp in &row.components {
                if let ActionRowComponent::InputText(it) = comp {
                    if let Some(val) = &it.value {
                        inputs.push((it.custom_id.clone(), val.clone()));
                    }
                }
            }
        }

        let mut updated: Vec<String> = Vec::new();
        for (key, val) in inputs {
            if val.trim().is_empty() {
                continue;
            }
            if key == "log_channel" {
                if let Some(c) = parse::parse_channel_id(&val) {
                    self.set_log_channel(guild_id, c as i64).await;
                    updated.push("log channel".to_string());
                }
            } else if let Some((col, _)) = CATEGORIES.iter().find(|(k, _)| *k == key) {
                if let Some(v) = parse_threshold(&val) {
                    self.set_threshold(guild_id, col, v).await;
                    updated.push(format!("{col} = {v:.2}"));
                }
            }
        }

        let embed = if updated.is_empty() {
            embeds::warning_embed("No valid changes were submitted.")
        } else {
            embeds::success_embed(
                "Sentinel Config Saved",
                &format!("Updated: {}", updated.join(", ")),
            )
        };
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
}

impl SentinelCog {
    // ---- Toxicity scanning ------------------------------------------------

    async fn check_toxicity(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        config: &SentinelConfig,
        api_url: &str,
    ) {
        let payload = serde_json::json!({ "text": &msg.content });
        let resp = match self.state.http.post(api_url).json(&payload).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = ?e, "sentinel API call failed");
                return;
            }
        };
        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::debug!(error = ?e, "sentinel API returned bad JSON");
                return;
            }
        };

        let get = |k: &str| json.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let scores: [f64; 7] = [
            get("toxicity"),
            get("severe_toxicity"),
            get("obscene"),
            get("identity_attack"),
            get("insult"),
            get("threat"),
            get("sexual_explicit"),
        ];
        let thresholds: [f64; 7] = [
            config.toxicity,
            config.severe_toxicity,
            config.obscene,
            config.identity_attack,
            config.insult,
            config.threat,
            config.sexual_explicit,
        ];
        let avg_score = scores.iter().sum::<f64>() / 7.0;
        let avg_threshold = thresholds.iter().sum::<f64>() / 7.0;

        let triggered =
            scores.iter().zip(thresholds.iter()).any(|(s, t)| s > t) || avg_score > avg_threshold;
        if !triggered {
            return;
        }

        let log_channel = match config.log_channel_id {
            Some(id) => ChannelId::new(id as u64),
            None => return,
        };

        // Color-coded ANSI bar block.
        let mut body = String::from("```ansi\n");
        for (i, (_, label)) in CATEGORIES.iter().enumerate() {
            body.push_str(&bar_row(label, scores[i] * 100.0, thresholds[i] * 100.0));
            body.push('\n');
        }
        body.push_str(&bar_row(
            "Average",
            avg_score * 100.0,
            avg_threshold * 100.0,
        ));
        body.push_str("\n```");

        let mut embed = CreateEmbed::new()
            .title("Sentinel Alert")
            .description(body)
            .color(colors::RED)
            .timestamp(Timestamp::now());

        // Recent message history for context (last 5, oldest first).
        if let Ok(mut history) = msg
            .channel_id
            .messages(&ctx.http, GetMessages::new().limit(5))
            .await
        {
            history.reverse();
            for m in history {
                let preview = if m.content.trim().is_empty() {
                    "No message content.".to_string()
                } else if m.content.chars().count() > 500 {
                    format!("{}...", m.content.chars().take(497).collect::<String>())
                } else {
                    m.content.clone()
                };
                embed = embed.field(
                    format!("{} - {}", m.author.name, m.author.id.get()),
                    preview,
                    false,
                );
            }
        }

        let _ = log_channel
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;

        // Optionally delete the offending message.
        if self
            .delete_flags
            .get(&guild_id)
            .map(|b| *b)
            .unwrap_or(false)
        {
            let _ = msg.delete(&ctx.http).await;
        }
    }

    // ---- Sentinel management commands -------------------------------------

    async fn handle_sentinel_cmd(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        subcmd: &str,
        arg: &str,
    ) {
        // Every Sentinel subcommand configures the automod (the `config` panel
        // itself opens threshold editors), so all require Manage Server.
        if !perms::require_perm(
            ctx,
            msg,
            GuildId::new(guild_id),
            Permissions::MANAGE_GUILD,
            "Manage Server",
        )
        .await
        {
            return;
        }
        match subcmd {
            "enable" => {
                let _ = sentinel_config::Entity::insert(sentinel_config::ActiveModel {
                    guild_id: Set(guild_id as i64),
                    enabled: Set(true),
                    ..Default::default()
                })
                .on_conflict(
                    OnConflict::column(sentinel_config::Column::GuildId)
                        .update_column(sentinel_config::Column::Enabled)
                        .to_owned(),
                )
                .exec(self.state.servers_orm())
                .await;
                {
                    let mut e = self
                        .state
                        .sentinel_cache
                        .entry(guild_id)
                        .or_insert_with(default_config);
                    e.enabled = true;
                }
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Sentinel Enabled",
                            "Messages will now be scanned for toxicity.",
                        )),
                    )
                    .await;
            }
            "disable" => {
                let _ = sentinel_config::Entity::update_many()
                    .col_expr(sentinel_config::Column::Enabled, Expr::value(false))
                    .filter(sentinel_config::Column::GuildId.eq(guild_id as i64))
                    .exec(self.state.servers_orm())
                    .await;
                if let Some(mut e) = self.state.sentinel_cache.get_mut(&guild_id) {
                    e.enabled = false;
                }
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Sentinel Disabled",
                            "Toxicity scanning is off.",
                        )),
                    )
                    .await;
            }
            "channel" => {
                let cid = match parse::parse_channel_id(arg) {
                    Some(c) => c,
                    None => {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "Usage: sentinel channel <#channel>")
                            .await;
                        return;
                    }
                };
                self.set_log_channel(guild_id, cid as i64).await;
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Sentinel Log Channel Set",
                            &format!("Alerts will be sent to <#{cid}>."),
                        )),
                    )
                    .await;
            }
            "threshold" => {
                let mut parts = arg.splitn(2, ' ');
                let category = parts.next().unwrap_or("").trim();
                let value: f64 = match parts.next().and_then(parse_threshold) {
                    Some(v) => v,
                    None => {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "Usage: sentinel threshold <category> <0.0-1.0>")
                            .await;
                        return;
                    }
                };
                let col = match category {
                    "toxicity" => "toxicity",
                    "severe_toxicity" | "severe" => "severe_toxicity",
                    "obscene" => "obscene",
                    "threat" => "threat",
                    "insult" => "insult",
                    "identity_attack" | "identity" => "identity_attack",
                    "sexual_explicit" | "sexual" => "sexual_explicit",
                    _ => {
                        let _ = msg
                            .channel_id
                            .say(
                                &ctx.http,
                                "Invalid category. Use: toxicity, severe_toxicity, obscene, threat, insult, identity_attack, sexual_explicit",
                            )
                            .await;
                        return;
                    }
                };
                self.set_threshold(guild_id, col, value).await;
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Threshold Updated",
                            &format!("`{col}` threshold set to {value:.2}."),
                        )),
                    )
                    .await;
            }
            "delete" => {
                let on = matches!(arg, "on" | "true" | "enable" | "1" | "yes");
                let off = matches!(arg, "off" | "false" | "disable" | "0" | "no");
                if !on && !off {
                    let _ = msg
                        .channel_id
                        .say(&ctx.http, "Usage: sentinel delete <on|off>")
                        .await;
                    return;
                }
                let _ = sentinel_config::Entity::insert(sentinel_config::ActiveModel {
                    guild_id: Set(guild_id as i64),
                    delete_flagged: Set(on),
                    ..Default::default()
                })
                .on_conflict(
                    OnConflict::column(sentinel_config::Column::GuildId)
                        .update_column(sentinel_config::Column::DeleteFlagged)
                        .to_owned(),
                )
                .exec(self.state.servers_orm())
                .await;
                self.delete_flags.insert(guild_id, on);
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Sentinel Delete Updated",
                            &format!(
                                "Flagged messages will {} be deleted.",
                                if on { "now" } else { "no longer" }
                            ),
                        )),
                    )
                    .await;
            }
            "config" => {
                self.cmd_config(ctx, msg, guild_id).await;
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `sentinel enable` | `disable` | `channel <#channel>` | `threshold <category> <0.0-1.0>` | `delete <on|off>` | `config`",
                    )
                    .await;
            }
        }
    }

    /// Send the config overview embed with the `SentinelConfigView` buttons.
    async fn cmd_config(&self, ctx: &Context, msg: &Message, guild_id: u64) {
        let config = self
            .state
            .sentinel_cache
            .get(&guild_id)
            .map(|c| c.clone())
            .unwrap_or_else(default_config);

        let channel = config
            .log_channel_id
            .map(|c| format!("<#{c}>"))
            .unwrap_or_else(|| "Not set".to_string());
        let delete_flagged = self
            .delete_flags
            .get(&guild_id)
            .map(|b| *b)
            .unwrap_or(false);

        let desc = format!(
            "**Enabled:** {}\n**Log Channel:** {}\n**Delete Flagged:** {}\n\n\
             **Toxicity:** {:.2}\n**Severe Toxicity:** {:.2}\n**Obscene:** {:.2}\n\
             **Identity Attack:** {:.2}\n**Insult:** {:.2}\n**Threat:** {:.2}\n**Sexual Explicit:** {:.2}",
            config.enabled,
            channel,
            delete_flagged,
            config.toxicity,
            config.severe_toxicity,
            config.obscene,
            config.identity_attack,
            config.insult,
            config.threat,
            config.sexual_explicit,
        );
        let embed = CreateEmbed::new()
            .title("Sentinel Config")
            .description(desc)
            .color(colors::RED)
            .timestamp(Timestamp::now());

        let buttons = CreateActionRow::Buttons(vec![
            CreateButton::new(BTN_THRESH_A)
                .label("Edit Thresholds (1/2)")
                .style(ButtonStyle::Primary),
            CreateButton::new(BTN_THRESH_B)
                .label("Edit Thresholds (2/2) + Channel")
                .style(ButtonStyle::Primary),
        ]);

        let _ = msg
            .channel_id
            .send_message(
                &ctx.http,
                CreateMessage::new()
                    .embed(embed)
                    .components(vec![buttons])
                    .reference_message(msg),
            )
            .await;
    }

    /// Upsert one threshold column and mirror it into the cache. `col` must be a
    /// validated `CATEGORIES` key (column names cannot be bound parameters).
    async fn set_threshold(&self, guild_id: u64, col: &str, value: f64) {
        // The column name can't be a bound parameter, so map the validated
        // `col` to its typed entity column and upsert just that threshold.
        use sentinel_config::Column as C;
        let gid = guild_id as i64;
        let upsert = match col {
            "toxicity" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    toxicity: Set(value),
                    ..Default::default()
                },
                C::Toxicity,
            )),
            "severe_toxicity" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    severe_toxicity: Set(value),
                    ..Default::default()
                },
                C::SevereToxicity,
            )),
            "obscene" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    obscene: Set(value),
                    ..Default::default()
                },
                C::Obscene,
            )),
            "threat" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    threat: Set(value),
                    ..Default::default()
                },
                C::Threat,
            )),
            "insult" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    insult: Set(value),
                    ..Default::default()
                },
                C::Insult,
            )),
            "identity_attack" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    identity_attack: Set(value),
                    ..Default::default()
                },
                C::IdentityAttack,
            )),
            "sexual_explicit" => Some((
                sentinel_config::ActiveModel {
                    guild_id: Set(gid),
                    sexual_explicit: Set(value),
                    ..Default::default()
                },
                C::SexualExplicit,
            )),
            _ => None,
        };
        if let Some((am, update_col)) = upsert {
            let _ = sentinel_config::Entity::insert(am)
                .on_conflict(
                    OnConflict::column(C::GuildId)
                        .update_column(update_col)
                        .to_owned(),
                )
                .exec(self.state.servers_orm())
                .await;
        }

        let mut e = self
            .state
            .sentinel_cache
            .entry(guild_id)
            .or_insert_with(default_config);
        match col {
            "toxicity" => e.toxicity = value,
            "severe_toxicity" => e.severe_toxicity = value,
            "obscene" => e.obscene = value,
            "threat" => e.threat = value,
            "insult" => e.insult = value,
            "identity_attack" => e.identity_attack = value,
            "sexual_explicit" => e.sexual_explicit = value,
            _ => {}
        }
    }

    async fn set_log_channel(&self, guild_id: u64, channel_id: i64) {
        let _ = sentinel_config::Entity::insert(sentinel_config::ActiveModel {
            guild_id: Set(guild_id as i64),
            log_channel_id: Set(Some(channel_id)),
            ..Default::default()
        })
        .on_conflict(
            OnConflict::column(sentinel_config::Column::GuildId)
                .update_column(sentinel_config::Column::LogChannelId)
                .to_owned(),
        )
        .exec(self.state.servers_orm())
        .await;
        let mut e = self
            .state
            .sentinel_cache
            .entry(guild_id)
            .or_insert_with(default_config);
        e.log_channel_id = Some(channel_id);
    }

    // ---- Decancer management commands -------------------------------------

    async fn handle_decancer_cmd(
        &self,
        ctx: &Context,
        msg: &Message,
        guild_id: u64,
        subcmd: &str,
        arg: &str,
    ) {
        // `decancer user` force-rewrites a member's nickname (Manage Nicknames);
        // the enable/disable/logs toggles are guild config (Manage Server).
        let required = match subcmd {
            "user" => Some((Permissions::MANAGE_NICKNAMES, "Manage Nicknames")),
            "enable" | "disable" | "logs" => Some((Permissions::MANAGE_GUILD, "Manage Server")),
            _ => None,
        };
        if let Some((perm, label)) = required
            && !perms::require_perm(ctx, msg, GuildId::new(guild_id), perm, label).await
        {
            return;
        }
        match subcmd {
            "enable" => {
                self.set_decancer_enabled(guild_id, true).await;
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(embeds::success_embed(
                            "Enabled Decancer",
                            "New members' nicknames will be automatically cleaned. Consider setting `decancer logs <#channel>`.",
                        )),
                    )
                    .await;
            }
            "disable" => {
                self.set_decancer_enabled(guild_id, false).await;
                let _ = msg
                    .channel_id
                    .send_message(
                        &ctx.http,
                        CreateMessage::new().embed(
                            CreateEmbed::new()
                                .title("Disabled Decancer")
                                .description("Automatic nickname cleaning is off. Consider re-enabling this feature!")
                                .color(colors::RED)
                                .timestamp(Timestamp::now()),
                        ),
                    )
                    .await;
            }
            "logs" => {
                let cid = parse::parse_channel_id(arg).unwrap_or_else(|| msg.channel_id.get());
                self.ensure_decancer_row(guild_id).await;
                let _ = sentinels_decancer::Entity::update_many()
                    .col_expr(
                        sentinels_decancer::Column::LogChannelId,
                        Expr::value(cid as i64),
                    )
                    .filter(sentinels_decancer::Column::GuildId.eq(guild_id as i64))
                    .exec(self.state.servers_orm())
                    .await;
                {
                    let mut e = self
                        .decancer_cache
                        .entry(guild_id)
                        .or_insert(DecancerConfig {
                            enabled: false,
                            log_channel_id: None,
                        });
                    e.log_channel_id = Some(cid as i64);
                }
                let enabled = self
                    .decancer_cache
                    .get(&guild_id)
                    .map(|c| c.enabled)
                    .unwrap_or(false);
                let mut embed = embeds::success_embed(
                    "Decancer Logs Channel Updated",
                    &format!("Set Decancer logs to <#{cid}>."),
                );
                if !enabled {
                    embed = embed.footer(CreateEmbedFooter::new(
                        "Reminder: You need to enable the decancer feature!",
                    ));
                }
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }
            "user" => {
                let uid = match parse::parse_user_id(arg) {
                    Some(u) => u,
                    None => {
                        let _ = msg
                            .channel_id
                            .say(&ctx.http, "Usage: decancer user <@member>")
                            .await;
                        return;
                    }
                };
                let gid = GuildId::new(guild_id);
                let member = match gid.member(&ctx.http, UserId::new(uid)).await {
                    Ok(m) => m,
                    Err(_) => {
                        let _ = msg.channel_id.say(&ctx.http, "Member not found.").await;
                        return;
                    }
                };

                let original = member.display_name().to_string();
                let cleaned = decancer_name(&original);
                if cleaned != original && !cleaned.trim().is_empty() {
                    let _ = gid
                        .edit_member(
                            &ctx.http,
                            UserId::new(uid),
                            EditMember::new().nickname(cleaned.clone()),
                        )
                        .await;
                }

                let icon = member
                    .user
                    .avatar_url()
                    .unwrap_or_else(|| member.user.default_avatar_url());
                let embed = decancer_embed("Decancer Action", &original, &cleaned, uid, icon);
                let _ = msg
                    .channel_id
                    .send_message(&ctx.http, CreateMessage::new().embed(embed.clone()))
                    .await;

                if let Some(log_id) = self
                    .decancer_cache
                    .get(&guild_id)
                    .and_then(|c| c.log_channel_id)
                {
                    let _ = ChannelId::new(log_id as u64)
                        .send_message(&ctx.http, CreateMessage::new().embed(embed))
                        .await;
                }
            }
            _ => {
                let _ = msg
                    .channel_id
                    .say(
                        &ctx.http,
                        "Usage: `decancer enable` | `disable` | `logs <#channel>` | `user <@member>`",
                    )
                    .await;
            }
        }
    }

    async fn ensure_decancer_row(&self, guild_id: u64) {
        let _ = sentinels_decancer::Entity::insert(sentinels_decancer::ActiveModel {
            guild_id: Set(guild_id as i64),
            enabled: Set(false),
            log_channel_id: Set(None),
        })
        .on_conflict(
            OnConflict::column(sentinels_decancer::Column::GuildId)
                .do_nothing()
                .to_owned(),
        )
        .exec(self.state.servers_orm())
        .await;
    }

    async fn set_decancer_enabled(&self, guild_id: u64, enabled: bool) {
        self.ensure_decancer_row(guild_id).await;
        let _ = sentinels_decancer::Entity::update_many()
            .col_expr(sentinels_decancer::Column::Enabled, Expr::value(enabled))
            .filter(sentinels_decancer::Column::GuildId.eq(guild_id as i64))
            .exec(self.state.servers_orm())
            .await;
        let mut e = self
            .decancer_cache
            .entry(guild_id)
            .or_insert(DecancerConfig {
                enabled: false,
                log_channel_id: None,
            });
        e.enabled = enabled;
    }
}

// ---- Free helpers ---------------------------------------------------------

fn default_config() -> SentinelConfig {
    SentinelConfig {
        enabled: false,
        log_channel_id: None,
        toxicity: 0.85,
        severe_toxicity: 0.85,
        obscene: 0.85,
        threat: 0.85,
        insult: 0.85,
        identity_attack: 0.85,
        sexual_explicit: 0.85,
    }
}

/// Clean a display name with the `decancer` crate, retaining capitalization.
/// Falls back to the original on error.
fn decancer_name(name: &str) -> String {
    match decancer::cure(name, decancer::Options::default().retain_capitalization()) {
        Ok(cured) => cured.to_string(),
        Err(_) => name.to_string(),
    }
}

/// Accept either a 0.0-1.0 float or a 0-100 number.
fn parse_threshold(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    let v = if v > 1.0 { v / 100.0 } else { v };
    if (0.0..=1.0).contains(&v) {
        Some(v)
    } else {
        None
    }
}

/// One row of the ANSI toxicity bar:
/// label + percent on one line, a 50-cell colored bar on the next. The bar is
/// red when the score exceeds its threshold, yellow above half the threshold,
/// green otherwise.
fn bar_row(label: &str, value_pct: f64, threshold_pct: f64) -> String {
    const RED: &str = "\u{1b}[31m";
    const YELLOW: &str = "\u{1b}[33m";
    const GREEN: &str = "\u{1b}[32m";
    const WHITE: &str = "\u{1b}[37m";

    let color = if value_pct > threshold_pct {
        RED
    } else if value_pct > threshold_pct / 2.0 {
        YELLOW
    } else {
        GREEN
    };
    let filled = ((value_pct / 2.0).round() as i64).clamp(0, 50) as usize;
    let empty = 50usize.saturating_sub(filled);
    format!(
        "{WHITE}{label:<40}{color}{value_pct:.2}%\n{color}{}{WHITE}{}",
        "█".repeat(filled),
        "█".repeat(empty),
    )
}

fn decancer_embed(
    title: &str,
    original: &str,
    cleaned: &str,
    user_id: u64,
    icon: String,
) -> CreateEmbed {
    CreateEmbed::new()
        .title(title)
        .description(format!("{original} >> **{cleaned}**"))
        .color(colors::BLUE)
        .timestamp(Timestamp::now())
        .footer(CreateEmbedFooter::new(user_id.to_string()).icon_url(icon))
}

/// Build the first threshold modal (5 inputs: Discord caps a modal at 5 rows,
/// so the seven categories + channel are split across two modals).
fn build_modal_a(config: &SentinelConfig) -> CreateModal {
    let fields = [
        ("toxicity", "Toxicity", config.toxicity),
        ("severe_toxicity", "Severe Toxicity", config.severe_toxicity),
        ("obscene", "Obscene", config.obscene),
        ("identity_attack", "Identity Attack", config.identity_attack),
        ("insult", "Insult", config.insult),
    ];
    let rows = fields
        .iter()
        .map(|(key, label, val)| threshold_input(key, label, *val))
        .collect();
    CreateModal::new(MODAL_THRESH_A, "Sentinel Thresholds (1/2)").components(rows)
}

/// Build the second threshold modal (threat, sexual_explicit + log channel).
fn build_modal_b(config: &SentinelConfig) -> CreateModal {
    let mut rows = vec![
        threshold_input("threat", "Threat", config.threat),
        threshold_input("sexual_explicit", "Sexual Explicit", config.sexual_explicit),
    ];
    let mut chan_input = CreateInputText::new(
        InputTextStyle::Short,
        "Log Channel (ID or #mention)",
        "log_channel",
    )
    .placeholder("Channel ID or #mention")
    .required(false)
    .max_length(40);
    if let Some(c) = config.log_channel_id {
        chan_input = chan_input.value(c.to_string());
    }
    rows.push(CreateActionRow::InputText(chan_input));
    CreateModal::new(MODAL_THRESH_B, "Sentinel Thresholds (2/2)").components(rows)
}

fn threshold_input(key: &str, label: &str, value: f64) -> CreateActionRow {
    CreateActionRow::InputText(
        CreateInputText::new(InputTextStyle::Short, label, key)
            .placeholder(format!("{value:.2}"))
            .value(format!("{value:.2}"))
            .required(false)
            .max_length(5),
    )
}
