use super::Cog;
use crate::framework::{Context, Data, Error, send_embed, send_error};
use crate::state::AppState;
use crate::utils::{colors, format};
use async_trait::async_trait;
use serenity::all::{
    ButtonStyle, ChannelType, Colour, ComponentInteraction, CreateActionRow, CreateButton,
    CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, Guild, GuildId, Permissions, PremiumTier, Role, RoleId,
    Timestamp, UserPublicFlags,
};
use std::collections::HashMap;
use std::sync::Arc;

/// custom_id prefix for the avatar card's delete button. All of this cog's
/// component ids share the `info:` prefix so `on_component` can early-return for
/// ids it does not own; the invoking user's id is appended here so only they can
/// delete the card.
const AVATAR_DELETE_PREFIX: &str = "info:avatar:delete:";

/// How many role mentions to list in a member card before truncating.
const MAX_ROLES_SHOWN: usize = 20;

pub struct InfoCog {
    state: Arc<AppState>,
}

impl InfoCog {
    pub fn new(state: Arc<AppState>) -> Arc<Self> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl Cog for InfoCog {
    async fn on_component(&self, ctx: &serenity::all::Context, interaction: &ComponentInteraction) {
        if !interaction.data.custom_id.starts_with("info:") {
            return;
        }
        if let Some(owner) = interaction
            .data
            .custom_id
            .strip_prefix(AVATAR_DELETE_PREFIX)
            .and_then(|id| id.parse::<u64>().ok())
        {
            // Only the user who requested the card may delete it.
            if owner != interaction.user.id.get() {
                let _ = interaction
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .ephemeral(true)
                                .content("This isn't your avatar card."),
                        ),
                    )
                    .await;
                return;
            }
            // Acknowledge so Discord does not show "interaction failed", then
            // remove the avatar card.
            let _ = interaction
                .create_response(&ctx.http, CreateInteractionResponse::Acknowledge)
                .await;
            let _ = interaction.message.delete(&ctx.http).await;
        }
    }
}

pub fn commands() -> Vec<poise::Command<Data, Error>> {
    vec![info(), serverinfo(), roleinfo(), avatar()]
}

// ---- commands ---------------------------------------------------------------

/// Show detailed information about a server member.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Info & Utility",
    aliases("userinfo", "ui", "whois", "i")
)]
async fn info(
    ctx: Context<'_>,
    #[description = "Member"] member: Option<serenity::all::Member>,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let member_opt = match member {
        Some(m) => Some(m),
        None => ctx.author_member().await.map(|cow| cow.into_owned()),
    };

    let roles = fetch_roles(sctx, guild_id).await;

    match member_opt {
        Some(member) => {
            let embed = build_member_card(&member, &roles);
            send_embed(ctx, embed).await
        }
        None => {
            // Invoker somehow not a member — show a basic user card.
            let user = ctx.author();
            let avatar = user.face();
            let badges = badge_names(user.public_flags.unwrap_or_else(UserPublicFlags::empty));
            let embed = CreateEmbed::new()
                .author(CreateEmbedAuthor::new(user.tag()).icon_url(&avatar))
                .title(user.name.clone())
                .thumbnail(&avatar)
                .color(colors::BLURPLE)
                .field("Username", user.name.clone(), true)
                .field("ID", format!("`{}`", user.id.get()), true)
                .field("Bot", yes_no(user.bot), true)
                .field("Account Created", fmt_ts(user.id.created_at()), false)
                .field(
                    "Badges",
                    if badges.is_empty() {
                        "None".to_string()
                    } else {
                        badges.join(", ")
                    },
                    false,
                )
                .footer(CreateEmbedFooter::new(
                    "This user is not a member of this server.",
                ))
                .timestamp(Timestamp::now());
            send_embed(ctx, embed).await
        }
    }
}

/// Show information about this server.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Info & Utility",
    aliases("si", "guildinfo")
)]
async fn serverinfo(ctx: Context<'_>) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let cached = sctx.cache.guild(guild_id).map(|g| g.clone());
    let embed = if let Some(guild) = cached {
        build_server_card_cached(&guild)
    } else {
        match build_server_card_http(sctx, guild_id).await {
            Some(e) => e,
            None => return send_error(ctx, "Failed to get server info.").await,
        }
    };
    send_embed(ctx, embed).await
}

/// Show information about a role.
#[poise::command(
    slash_command,
    prefix_command,
    guild_only,
    category = "Info & Utility",
    aliases("ri")
)]
async fn roleinfo(
    ctx: Context<'_>,
    #[description = "Role"] role: serenity::all::Role,
) -> Result<(), Error> {
    let guild_id = ctx.guild_id().unwrap();
    let sctx = ctx.serenity_context();

    let member_count = sctx.cache.guild(guild_id).map(|guild| {
        guild
            .members
            .values()
            .filter(|m| m.roles.contains(&role.id))
            .count()
    });

    let color = if role.colour.0 == 0 {
        colors::BLURPLE
    } else {
        role.colour
    };
    let perms = permission_summary(role.permissions);

    let mut embed = CreateEmbed::new()
        .title(format!("Role: {}", role.name))
        .color(color)
        .field("Name", role.name.clone(), true)
        .field("Role ID", format!("`{}`", role.id.get()), true)
        .field("Color", format!("`#{:06X}`", role.colour.0), true)
        .field("Position", role.position.to_string(), true)
        .field("Mentionable", yes_no(role.mentionable), true)
        .field("Hoisted", yes_no(role.hoist), true)
        .field("Managed", yes_no(role.managed), true)
        .field("Created", fmt_ts(role.id.created_at()), false)
        .field("Permissions", perms, false)
        .timestamp(Timestamp::now());
    if let Some(count) = member_count {
        embed = embed.field("Members", count.to_string(), true);
    }
    send_embed(ctx, embed).await
}

/// Show a user's avatar.
#[poise::command(
    slash_command,
    prefix_command,
    category = "Info & Utility",
    aliases("av", "pfp")
)]
async fn avatar(
    ctx: Context<'_>,
    #[description = "Member"] member: Option<serenity::all::User>,
) -> Result<(), Error> {
    let sctx = ctx.serenity_context();
    let target_user = member.as_ref().unwrap_or_else(|| ctx.author());

    let (title, avatar_url, color) = if let Some(guild_id) = ctx.guild_id() {
        let roles = fetch_roles(sctx, guild_id).await;
        match guild_id.member(sctx, target_user.id).await {
            Ok(m) => (
                m.display_name().to_string(),
                m.face(),
                top_color(&roles, &m.roles),
            ),
            Err(_) => (target_user.name.clone(), target_user.face(), colors::BLURPLE),
        }
    } else {
        (target_user.name.clone(), target_user.face(), colors::BLURPLE)
    };

    let embed = CreateEmbed::new()
        .title(title)
        .image(avatar_url)
        .color(color)
        .timestamp(Timestamp::now());
    let button = CreateButton::new(format!("{AVATAR_DELETE_PREFIX}{}", ctx.author().id.get()))
        .label("Delete")
        .style(ButtonStyle::Danger)
        .emoji('🗑');
    ctx.send(
        poise::CreateReply::default()
            .embed(embed)
            .components(vec![CreateActionRow::Buttons(vec![button])]),
    )
    .await?;
    Ok(())
}

// ---- free helpers -----------------------------------------------------------

/// Owned snapshot of the guild's roles, preferring the gateway cache and
/// falling back to a fresh HTTP fetch on a cold cache.
async fn fetch_roles(ctx: &serenity::all::Context, guild_id: GuildId) -> HashMap<RoleId, Role> {
    if let Some(guild) = ctx.cache.guild(guild_id) {
        return guild.roles.clone();
    }
    guild_id.roles(&ctx.http).await.unwrap_or_default()
}

async fn build_server_card_http(
    ctx: &serenity::all::Context,
    guild_id: GuildId,
) -> Option<CreateEmbed> {
    let guild = guild_id
        .to_partial_guild_with_counts(&ctx.http)
        .await
        .ok()?;
    let channels = guild_id.channels(&ctx.http).await.unwrap_or_default();
    let (text, voice, categories) = count_channels(channels.values().map(|c| c.kind));

    let total = guild.approximate_member_count.unwrap_or(0);
    let mut embed = CreateEmbed::new()
        .title(guild.name.clone())
        .color(colors::BLURPLE)
        .field("Owner", format!("<@{}>", guild.owner_id.get()), true)
        .field("Server ID", format!("`{}`", guild.id.get()), true)
        .field("Created", fmt_ts(guild.id.created_at()), false)
        .field("Members", format!("**Total:** {total}"), true)
        .field(
            "Channels",
            format!("**Text:** {text}\n**Voice:** {voice}\n**Categories:** {categories}"),
            true,
        )
        .field("Roles", guild.roles.len().to_string(), true)
        .field(
            "Boost Status",
            boost_status(guild.premium_tier, guild.premium_subscription_count),
            true,
        )
        .field(
            "Verification",
            format!("{:?}", guild.verification_level),
            true,
        )
        .timestamp(Timestamp::now());
    if let Some(icon) = guild.icon_url() {
        embed = embed.thumbnail(icon);
    }
    Some(embed)
}

/// Format a snowflake/instant as an absolute + relative Discord timestamp.
fn fmt_ts(t: Timestamp) -> String {
    let u = t.unix_timestamp();
    format!("<t:{u}:F> (<t:{u}:R>)")
}

fn yes_no(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

/// Highest-positioned role color the member has, ignoring uncolored roles.
fn top_color(roles: &HashMap<RoleId, Role>, member_roles: &[RoleId]) -> Colour {
    member_roles
        .iter()
        .filter_map(|rid| roles.get(rid))
        .filter(|r| r.colour.0 != 0)
        .max_by_key(|r| r.position)
        .map(|r| r.colour)
        .unwrap_or(colors::BLURPLE)
}

/// Human-readable public flag (badge) names a user carries.
fn badge_names(flags: UserPublicFlags) -> Vec<&'static str> {
    use UserPublicFlags as F;
    [
        (F::DISCORD_EMPLOYEE, "Discord Staff"),
        (F::PARTNERED_SERVER_OWNER, "Partnered Server Owner"),
        (F::HYPESQUAD_EVENTS, "HypeSquad Events"),
        (F::BUG_HUNTER_LEVEL_1, "Bug Hunter"),
        (F::HOUSE_BRAVERY, "HypeSquad Bravery"),
        (F::HOUSE_BRILLIANCE, "HypeSquad Brilliance"),
        (F::HOUSE_BALANCE, "HypeSquad Balance"),
        (F::EARLY_SUPPORTER, "Early Supporter"),
        (F::TEAM_USER, "Team User"),
        (F::SYSTEM, "System"),
        (F::BUG_HUNTER_LEVEL_2, "Bug Hunter Level 2"),
        (F::VERIFIED_BOT, "Verified Bot"),
        (
            F::EARLY_VERIFIED_BOT_DEVELOPER,
            "Early Verified Bot Developer",
        ),
        (F::DISCORD_CERTIFIED_MODERATOR, "Certified Moderator"),
        (F::ACTIVE_DEVELOPER, "Active Developer"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .map(|(_, name)| name)
    .collect()
}

/// Build the member card embed (info command, member present).
fn build_member_card(member: &serenity::all::Member, roles: &HashMap<RoleId, Role>) -> CreateEmbed {
    let user = &member.user;
    let avatar = member.face();
    let color = top_color(roles, &member.roles);

    // Role mentions, highest first, capped at MAX_ROLES_SHOWN.
    let mut sorted: Vec<&RoleId> = member.roles.iter().collect();
    sorted.sort_by_key(|rid| std::cmp::Reverse(roles.get(rid).map(|r| r.position).unwrap_or(0)));
    let total_roles = sorted.len();
    let shown: Vec<String> = sorted
        .iter()
        .take(MAX_ROLES_SHOWN)
        .map(|rid| format!("<@&{}>", rid.get()))
        .collect();
    let mut roles_value = if shown.is_empty() {
        "None".to_string()
    } else {
        shown.join(" ")
    };
    if total_roles > MAX_ROLES_SHOWN {
        roles_value.push_str(&format!(" *and {} more*", total_roles - MAX_ROLES_SHOWN));
    }

    let nick = member.nick.as_deref().unwrap_or("None").to_string();
    let badges = badge_names(user.public_flags.unwrap_or_else(UserPublicFlags::empty));
    let joined = member
        .joined_at
        .map(fmt_ts)
        .unwrap_or_else(|| "Unknown".to_string());

    CreateEmbed::new()
        .author(CreateEmbedAuthor::new(user.tag()).icon_url(&avatar))
        .title(member.display_name().to_string())
        .thumbnail(&avatar)
        .color(color)
        .field("Username", user.name.clone(), true)
        .field("Nickname", nick, true)
        .field("Bot", yes_no(user.bot), true)
        .field("ID", format!("`{}`", user.id.get()), false)
        .field("Account Created", fmt_ts(user.id.created_at()), false)
        .field("Joined Server", joined, false)
        .field(
            format!("Roles [{total_roles}]"),
            format::truncate(&roles_value, 1024).to_string(),
            false,
        )
        .field(
            "Badges",
            if badges.is_empty() {
                "None".to_string()
            } else {
                badges.join(", ")
            },
            false,
        )
        .timestamp(Timestamp::now())
}

/// Build the server card from a fully cached guild.
fn build_server_card_cached(guild: &Guild) -> CreateEmbed {
    let total = guild.member_count;
    let (humans, bots) = if guild.members.len() as u64 == total && total > 0 {
        let bots = guild.members.values().filter(|m| m.user.bot).count() as u64;
        (Some(total - bots), Some(bots))
    } else {
        (None, None)
    };
    let mut members_value = format!("**Total:** {total}");
    if let (Some(h), Some(b)) = (humans, bots) {
        members_value.push_str(&format!("\n**Humans:** {h}\n**Bots:** {b}"));
    }

    let (text, voice, categories) = count_channels(guild.channels.values().map(|c| c.kind));

    let mut embed = CreateEmbed::new()
        .title(guild.name.clone())
        .color(colors::BLURPLE)
        .field("Owner", format!("<@{}>", guild.owner_id.get()), true)
        .field("Server ID", format!("`{}`", guild.id.get()), true)
        .field("Created", fmt_ts(guild.id.created_at()), false)
        .field("Members", members_value, true)
        .field(
            "Channels",
            format!("**Text:** {text}\n**Voice:** {voice}\n**Categories:** {categories}"),
            true,
        )
        .field("Roles", guild.roles.len().to_string(), true)
        .field(
            "Boost Status",
            boost_status(guild.premium_tier, guild.premium_subscription_count),
            true,
        )
        .field(
            "Verification",
            format!("{:?}", guild.verification_level),
            true,
        )
        .timestamp(Timestamp::now());
    if let Some(icon) = guild.icon_url() {
        embed = embed.thumbnail(icon);
    }
    embed
}

/// Count text/voice/category channels from a stream of channel kinds.
fn count_channels(kinds: impl Iterator<Item = ChannelType>) -> (usize, usize, usize) {
    let (mut text, mut voice, mut categories) = (0usize, 0usize, 0usize);
    for kind in kinds {
        match kind {
            ChannelType::Text | ChannelType::News | ChannelType::Forum => text += 1,
            ChannelType::Voice | ChannelType::Stage => voice += 1,
            ChannelType::Category => categories += 1,
            _ => {}
        }
    }
    (text, voice, categories)
}

fn boost_status(tier: PremiumTier, count: Option<u64>) -> String {
    let level = match tier {
        PremiumTier::Tier1 => 1,
        PremiumTier::Tier2 => 2,
        PremiumTier::Tier3 => 3,
        _ => 0,
    };
    format!("**Level {level}** \u{2022} {} boosts", count.unwrap_or(0))
}

/// Compact permission summary for a role's permission bitset.
fn permission_summary(perms: Permissions) -> String {
    if perms.contains(Permissions::ADMINISTRATOR) {
        return "Administrator (all permissions)".to_string();
    }
    let names = perms.get_permission_names();
    if names.is_empty() {
        "None".to_string()
    } else {
        format::truncate(&names.join(", "), 1024).to_string()
    }
}
