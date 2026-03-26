use async_trait::async_trait;
use serenity::all::{
    ChannelId, ComponentInteraction, Context, Guild, GuildId, Member, Message,
    MessageId, ModalInteraction, Reaction, UnavailableGuild, User,
};
use serenity::model::event::MessageUpdateEvent;
use std::sync::Arc;

#[async_trait]
pub trait Cog: Send + Sync {
    async fn on_ready(&self, _ctx: &Context) {}
    async fn on_message(&self, _ctx: &Context, _msg: &Message) {}
    async fn on_member_join(&self, _ctx: &Context, _member: &Member) {}
    async fn on_member_leave(&self, _ctx: &Context, _guild_id: GuildId, _user: &User) {}
    async fn on_message_update(&self, _ctx: &Context, _old: Option<Message>, _new: Option<Message>, _event: &MessageUpdateEvent) {}
    async fn on_message_delete(&self, _ctx: &Context, _channel_id: ChannelId, _msg_id: MessageId, _guild_id: Option<GuildId>) {}
    async fn on_reaction_add(&self, _ctx: &Context, _reaction: Reaction) {}
    async fn on_guild_create(&self, _ctx: &Context, _guild: &Guild) {}
    async fn on_guild_delete(&self, _ctx: &Context, _incomplete: UnavailableGuild, _full: Option<Guild>) {}
    async fn on_component(&self, _ctx: &Context, _interaction: &ComponentInteraction) {}
    async fn on_modal(&self, _ctx: &Context, _interaction: &ModalInteraction) {}
}

pub struct CogManager {
    prefix: String,
    cogs: Vec<Arc<dyn Cog>>,
}

impl CogManager {
    pub fn new(prefix: String) -> Self {
        Self { prefix, cogs: Vec::new() }
    }

    pub fn register(&mut self, cog: Arc<dyn Cog>) {
        self.cogs.push(cog);
    }

    pub fn prefix(&self) -> &str { &self.prefix }

    pub async fn dispatch_message(&self, ctx: &Context, msg: &Message) {
        for cog in &self.cogs { cog.on_message(ctx, msg).await; }
    }

    pub async fn dispatch_ready(&self, ctx: &Context) {
        for cog in &self.cogs { cog.on_ready(ctx).await; }
    }

    pub async fn dispatch_member_join(&self, ctx: &Context, member: &Member) {
        for cog in &self.cogs { cog.on_member_join(ctx, member).await; }
    }

    pub async fn dispatch_member_leave(&self, ctx: &Context, guild_id: GuildId, user: &User) {
        for cog in &self.cogs { cog.on_member_leave(ctx, guild_id, user).await; }
    }

    pub async fn dispatch_message_update(&self, ctx: &Context, old: Option<Message>, new: Option<Message>, event: &MessageUpdateEvent) {
        for cog in &self.cogs { cog.on_message_update(ctx, old.clone(), new.clone(), event).await; }
    }

    pub async fn dispatch_message_delete(&self, ctx: &Context, channel_id: ChannelId, msg_id: MessageId, guild_id: Option<GuildId>) {
        for cog in &self.cogs { cog.on_message_delete(ctx, channel_id, msg_id, guild_id).await; }
    }

    pub async fn dispatch_reaction_add(&self, ctx: &Context, reaction: Reaction) {
        for cog in &self.cogs { cog.on_reaction_add(ctx, reaction.clone()).await; }
    }

    pub async fn dispatch_guild_create(&self, ctx: &Context, guild: &Guild) {
        for cog in &self.cogs { cog.on_guild_create(ctx, guild).await; }
    }

    pub async fn dispatch_guild_delete(&self, ctx: &Context, incomplete: UnavailableGuild, full: Option<Guild>) {
        for cog in &self.cogs { cog.on_guild_delete(ctx, incomplete.clone(), full.clone()).await; }
    }

    pub async fn dispatch_component(&self, ctx: &Context, interaction: &ComponentInteraction) {
        for cog in &self.cogs { cog.on_component(ctx, interaction).await; }
    }

    pub async fn dispatch_modal(&self, ctx: &Context, interaction: &ModalInteraction) {
        for cog in &self.cogs { cog.on_modal(ctx, interaction).await; }
    }
}

pub mod afk;
pub mod base;
pub mod logging;
pub mod prefixes;
pub mod reminders;
pub mod settings;
pub mod tags;
pub mod welcome;
