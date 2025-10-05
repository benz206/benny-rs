use async_trait::async_trait;
use serenity::all::{Context, Message};
use std::sync::Arc;

#[async_trait]
pub trait Cog: Send + Sync {
    async fn on_ready(&self, _ctx: &Context) {}
    async fn on_message(&self, _ctx: &Context, _msg: &Message) {}
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
}

pub mod base;
pub mod prefixes;


