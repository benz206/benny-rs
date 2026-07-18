use async_trait::async_trait;
use serenity::all::{
    ChannelId, ComponentInteraction, Context, Guild, GuildChannel, GuildId, Member, Message,
    MessageId, ModalInteraction, Reaction, Role, RoleId, UnavailableGuild, User, VoiceState,
};
use serenity::model::event::{GuildMemberUpdateEvent, MessageUpdateEvent};
use std::sync::Arc;

/// Event hooks dispatched to every registered cog. All default to no-ops so a
/// cog only implements the events it cares about.
///
/// Component/modal interactions are fanned out to ALL cogs, so every cog that
/// uses interactive components MUST namespace its `custom_id`s with a unique
/// per-cog prefix (e.g. `tr:`, `dict:`, `help:`) and early-return from
/// `on_component`/`on_modal` when the id does not match its prefix.
#[async_trait]
pub trait Cog: Send + Sync {
    async fn on_ready(&self, _ctx: &Context) {}
    /// Every non-bot message. Commands are dispatched by poise; use this only
    /// for behaviour that must run on all messages (e.g. AFK auto-clear,
    /// toxicity scanning, dynamic tag invocation).
    async fn on_message(&self, _ctx: &Context, _msg: &Message) {}
    async fn on_member_join(&self, _ctx: &Context, _member: &Member) {}
    async fn on_member_leave(&self, _ctx: &Context, _guild_id: GuildId, _user: &User) {}
    async fn on_member_update(
        &self,
        _ctx: &Context,
        _old: Option<Member>,
        _new: Option<Member>,
        _event: &GuildMemberUpdateEvent,
    ) {
    }
    async fn on_member_ban(&self, _ctx: &Context, _guild_id: GuildId, _banned_user: &User) {}
    async fn on_member_unban(&self, _ctx: &Context, _guild_id: GuildId, _unbanned_user: &User) {}
    async fn on_message_update(
        &self,
        _ctx: &Context,
        _old: Option<Message>,
        _new: Option<Message>,
        _event: &MessageUpdateEvent,
    ) {
    }
    async fn on_message_delete(
        &self,
        _ctx: &Context,
        _channel_id: ChannelId,
        _msg_id: MessageId,
        _guild_id: Option<GuildId>,
    ) {
    }
    async fn on_reaction_add(&self, _ctx: &Context, _reaction: Reaction) {}
    async fn on_reaction_remove(&self, _ctx: &Context, _reaction: Reaction) {}
    async fn on_guild_create(&self, _ctx: &Context, _guild: &Guild) {}
    async fn on_guild_delete(
        &self,
        _ctx: &Context,
        _incomplete: UnavailableGuild,
        _full: Option<Guild>,
    ) {
    }
    async fn on_channel_create(&self, _ctx: &Context, _channel: &GuildChannel) {}
    async fn on_channel_delete(&self, _ctx: &Context, _channel: &GuildChannel) {}
    async fn on_role_create(&self, _ctx: &Context, _role: &Role) {}
    async fn on_role_delete(
        &self,
        _ctx: &Context,
        _guild_id: GuildId,
        _role_id: RoleId,
        _role: Option<Role>,
    ) {
    }
    async fn on_thread_create(&self, _ctx: &Context, _thread: &GuildChannel) {}
    async fn on_voice_state_update(
        &self,
        _ctx: &Context,
        _old: Option<VoiceState>,
        _new: &VoiceState,
    ) {
    }
    async fn on_component(&self, _ctx: &Context, _interaction: &ComponentInteraction) {}
    async fn on_modal(&self, _ctx: &Context, _interaction: &ModalInteraction) {}
}

#[derive(Default)]
pub struct CogManager {
    cogs: Vec<Arc<dyn Cog>>,
}

impl CogManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, cog: Arc<dyn Cog>) {
        self.cogs.push(cog);
    }

    /// Fan every message out to the `on_message` observer hook. Prefix-command
    /// parsing and dispatch is handled by poise; this drives the message-level
    /// features (AFK, toxicity scanning, dynamic tags).
    pub async fn dispatch_message(&self, ctx: &Context, msg: &Message) {
        for cog in &self.cogs {
            cog.on_message(ctx, msg).await;
        }
    }

    pub async fn dispatch_ready(&self, ctx: &Context) {
        for cog in &self.cogs {
            cog.on_ready(ctx).await;
        }
    }

    pub async fn dispatch_member_join(&self, ctx: &Context, member: &Member) {
        for cog in &self.cogs {
            cog.on_member_join(ctx, member).await;
        }
    }

    pub async fn dispatch_member_leave(&self, ctx: &Context, guild_id: GuildId, user: &User) {
        for cog in &self.cogs {
            cog.on_member_leave(ctx, guild_id, user).await;
        }
    }

    pub async fn dispatch_message_update(
        &self,
        ctx: &Context,
        old: Option<Message>,
        new: Option<Message>,
        event: &MessageUpdateEvent,
    ) {
        for cog in &self.cogs {
            cog.on_message_update(ctx, old.clone(), new.clone(), event)
                .await;
        }
    }

    pub async fn dispatch_message_delete(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
        msg_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        for cog in &self.cogs {
            cog.on_message_delete(ctx, channel_id, msg_id, guild_id)
                .await;
        }
    }

    pub async fn dispatch_reaction_add(&self, ctx: &Context, reaction: Reaction) {
        for cog in &self.cogs {
            cog.on_reaction_add(ctx, reaction.clone()).await;
        }
    }

    pub async fn dispatch_reaction_remove(&self, ctx: &Context, reaction: Reaction) {
        for cog in &self.cogs {
            cog.on_reaction_remove(ctx, reaction.clone()).await;
        }
    }

    pub async fn dispatch_guild_create(&self, ctx: &Context, guild: &Guild) {
        for cog in &self.cogs {
            cog.on_guild_create(ctx, guild).await;
        }
    }

    pub async fn dispatch_guild_delete(
        &self,
        ctx: &Context,
        incomplete: UnavailableGuild,
        full: Option<Guild>,
    ) {
        for cog in &self.cogs {
            cog.on_guild_delete(ctx, incomplete, full.clone())
                .await;
        }
    }

    pub async fn dispatch_member_update(
        &self,
        ctx: &Context,
        old: Option<Member>,
        new: Option<Member>,
        event: &GuildMemberUpdateEvent,
    ) {
        for cog in &self.cogs {
            cog.on_member_update(ctx, old.clone(), new.clone(), event)
                .await;
        }
    }

    pub async fn dispatch_member_ban(&self, ctx: &Context, guild_id: GuildId, banned_user: &User) {
        for cog in &self.cogs {
            cog.on_member_ban(ctx, guild_id, banned_user).await;
        }
    }

    pub async fn dispatch_member_unban(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        unbanned_user: &User,
    ) {
        for cog in &self.cogs {
            cog.on_member_unban(ctx, guild_id, unbanned_user).await;
        }
    }

    pub async fn dispatch_channel_create(&self, ctx: &Context, channel: &GuildChannel) {
        for cog in &self.cogs {
            cog.on_channel_create(ctx, channel).await;
        }
    }

    pub async fn dispatch_channel_delete(&self, ctx: &Context, channel: &GuildChannel) {
        for cog in &self.cogs {
            cog.on_channel_delete(ctx, channel).await;
        }
    }

    pub async fn dispatch_role_create(&self, ctx: &Context, role: &Role) {
        for cog in &self.cogs {
            cog.on_role_create(ctx, role).await;
        }
    }

    pub async fn dispatch_role_delete(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        role_id: RoleId,
        role: Option<Role>,
    ) {
        for cog in &self.cogs {
            cog.on_role_delete(ctx, guild_id, role_id, role.clone())
                .await;
        }
    }

    pub async fn dispatch_thread_create(&self, ctx: &Context, thread: &GuildChannel) {
        for cog in &self.cogs {
            cog.on_thread_create(ctx, thread).await;
        }
    }

    pub async fn dispatch_voice_state_update(
        &self,
        ctx: &Context,
        old: Option<VoiceState>,
        new: &VoiceState,
    ) {
        for cog in &self.cogs {
            cog.on_voice_state_update(ctx, old.clone(), new).await;
        }
    }

    pub async fn dispatch_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        for cog in &self.cogs {
            cog.on_component(ctx, interaction).await;
        }
    }

    pub async fn dispatch_modal(&self, ctx: &Context, interaction: &ModalInteraction) {
        for cog in &self.cogs {
            cog.on_modal(ctx, interaction).await;
        }
    }
}

pub mod afk;
pub mod automod;
pub mod base;
pub mod dev;
pub mod dictionary;
pub mod embed;
pub mod events;
pub mod giveaways;
pub mod help;
pub mod info;
pub mod levels;
pub mod logging;
pub mod moderation;
pub mod music;
pub mod ocr;
pub mod prefixes;
pub mod premium;
pub mod reminders;
pub mod roles;
pub mod sentinel;
pub mod settings;
pub mod starboard;
// Dormant while the TagScript engine is disabled (see tagscript/mod.rs); the
// cog is not registered in `build_manager`/`all_commands` below, so its items
// are unused by design.
#[allow(dead_code)]
pub mod tags;
pub mod translate;
pub mod welcome;

// ---- registration ----------------------------------------------------------
//
// The single place cogs are enumerated. Adding a cog means: `pub mod` above,
// one line in `build_manager` (gateway-event hooks), and one line in
// `all_commands` (poise commands). Nothing in main.rs or framework.rs changes.

/// Construct the manager with every active cog registered, in dispatch order.
pub fn build_manager(state: &Arc<crate::state::AppState>) -> CogManager {
    let mut m = CogManager::new();
    m.register(base::BaseCog::new(state.clone()));
    m.register(prefixes::PrefixesCog::new(state.clone()));
    m.register(afk::AfkCog::new(state.clone()));
    m.register(reminders::RemindersCog::new(state.clone()));
    // TEMPORARILY DISABLED: TagScript engine has known issues (see tagscript/mod.rs).
    // m.register(tags::TagsCog::new(state.clone()));
    m.register(welcome::WelcomeCog::new(state.clone()));
    m.register(logging::LoggingCog::new(state.clone()));
    m.register(settings::SettingsCog::new(state.clone()));
    m.register(moderation::ModerationCog::new(state.clone()));
    m.register(info::InfoCog::new(state.clone()));
    m.register(help::HelpCog::new(state.clone()));
    m.register(roles::RolesCog::new(state.clone()));
    m.register(translate::TranslateCog::new(state.clone()));
    m.register(dictionary::DictionaryCog::new(state.clone()));
    m.register(ocr::OcrCog::new(state.clone()));
    m.register(embed::EmbedCog::new(state.clone()));
    m.register(sentinel::SentinelCog::new(state.clone()));
    m.register(automod::AutomodCog::new(state.clone()));
    m.register(giveaways::GiveawaysCog::new(state.clone()));
    m.register(levels::LevelsCog::new(state.clone()));
    m.register(starboard::StarboardCog::new(state.clone()));
    m.register(dev::DevCog::new(state.clone()));
    m.register(premium::PremiumCog::new(state.clone()));
    m.register(music::MusicCog::new(state.clone()));
    m.register(events::EventsCog::new(state.clone()));
    m
}

/// Every command across every cog, concatenated into the list poise registers.
pub fn all_commands() -> Vec<poise::Command<crate::framework::Data, crate::framework::Error>> {
    let mut cmds = Vec::new();
    cmds.extend(base::commands());
    cmds.extend(info::commands());
    // TEMPORARILY DISABLED: TagScript engine has known issues (see tagscript/mod.rs).
    // cmds.extend(tags::commands());
    cmds.extend(moderation::commands());
    cmds.extend(prefixes::commands());
    cmds.extend(settings::commands());
    cmds.extend(welcome::commands());
    cmds.extend(logging::commands());
    cmds.extend(sentinel::commands());
    cmds.extend(automod::commands());
    cmds.extend(giveaways::commands());
    cmds.extend(levels::commands());
    cmds.extend(starboard::commands());
    cmds.extend(roles::commands());
    cmds.extend(reminders::commands());
    cmds.extend(premium::commands());
    cmds.extend(translate::commands());
    cmds.extend(dictionary::commands());
    cmds.extend(ocr::commands());
    cmds.extend(embed::commands());
    cmds.extend(afk::commands());
    cmds.extend(music::commands());
    cmds.extend(dev::commands());
    cmds.extend(help::commands());
    cmds
}
