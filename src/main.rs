use anyhow::Result;
use serenity::all::{
    ChannelId, Client, Context, EventHandler, GatewayIntents,
    Guild, GuildChannel, GuildId, Interaction, Member, Message, MessageId,
    Reaction, Ready, Role, RoleId, UnavailableGuild, User, VoiceState,
};
use serenity::model::event::{GuildMemberUpdateEvent, MessageUpdateEvent};
use std::sync::Arc;
use tracing::{error, info};

mod config;
mod db;
mod db_mongo;
mod error;
mod state;
mod http;
mod cogs;
mod slash;
mod tagscript;
mod tasks;
mod utils;

use config::load_config;
use state::{start_latency_task, AppState};

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_level(true)
        .compact()
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            error!(error = ?e, "failed to load config.json");
            return Err(e);
        }
    };

    info!("starting benny-rs");

    let http_client = reqwest::Client::builder().build()?;
    tokio::fs::create_dir_all("databases").await.ok();
    let servers_db = sqlx::SqlitePool::connect("sqlite://databases/servers.db?mode=rwc").await?;
    let users_db = sqlx::SqlitePool::connect("sqlite://databases/users.db?mode=rwc").await?;

    db::ensure_servers_schema(&servers_db).await?;
    db::ensure_users_schema(&users_db).await?;

    // Connect MongoDB (optional - warn if unavailable)
    let mongo = match mongodb::Client::with_uri_str(&config.mongodb_uri).await {
        Ok(client) => {
            info!("MongoDB connected");
            Some(client)
        }
        Err(e) => {
            tracing::warn!(error = ?e, "MongoDB unavailable, moderation features disabled");
            None
        }
    };

    // Connect Redis (optional - warn if unavailable)
    let redis = match redis::Client::open(config.redis_uri.as_str()) {
        Ok(client) => match redis::aio::ConnectionManager::new(client).await {
            Ok(mgr) => {
                info!("Redis connected");
                Some(Arc::new(tokio::sync::Mutex::new(mgr)))
            }
            Err(e) => {
                tracing::warn!(error = ?e, "Redis unavailable, caching features disabled");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = ?e, "Redis unavailable, caching features disabled");
            None
        }
    };

    let app_state = Arc::new(AppState::new(
        config.clone(),
        http_client,
        servers_db,
        users_db,
        mongo,
        redis,
    ));
    start_latency_task(app_state.clone());

    let token = if cfg!(debug_assertions) {
        config.dev_token.as_deref().unwrap_or(&config.token)
    } else {
        &config.token
    };

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MODERATION
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::GUILD_MESSAGE_REACTIONS
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::DIRECT_MESSAGES;

    use cogs::{
        afk::AfkCog,
        base::BaseCog,
        dev::DevCog,
        dictionary::DictionaryCog,
        embed::EmbedCog,
        help::HelpCog,
        info::InfoCog,
        logging::LoggingCog,
        moderation::ModerationCog,
        music::MusicCog,
        ocr::OcrCog,
        prefixes::PrefixesCog,
        premium::PremiumCog,
        reminders::RemindersCog,
        roles::RolesCog,
        sentinel::SentinelCog,
        settings::SettingsCog,
        tags::TagsCog,
        translate::TranslateCog,
        welcome::WelcomeCog,
        CogManager,
    };

    struct Handler {
        cogs: Arc<CogManager>,
        state: Arc<AppState>,
        translate: Arc<TranslateCog>,
        reminder_task_started: std::sync::atomic::AtomicBool,
    }

    #[serenity::async_trait]
    impl EventHandler for Handler {
        async fn ready(&self, ctx: Context, ready: Ready) {
            info!("connected as {} ({})", ready.user.name, ready.user.id);
            info!("===========================================");
            info!("  benny-rs v{}", env!("CARGO_PKG_VERSION"));
            info!("  Guilds: {}", ctx.cache.guilds().len());
            info!("===========================================");
            self.cogs.dispatch_ready(&ctx).await;
            slash::register_global(&ctx).await;

            // Spawn reminder background task exactly once
            if !self.reminder_task_started.swap(true, std::sync::atomic::Ordering::SeqCst) {
                tasks::reminders::spawn_reminder_task(self.state.clone(), ctx.http.clone());
            }
        }

        async fn message(&self, ctx: Context, msg: Message) {
            if msg.author.bot { return; }
            self.cogs.dispatch_message(&ctx, &msg).await;
        }

        async fn guild_member_addition(&self, ctx: Context, member: Member) {
            self.cogs.dispatch_member_join(&ctx, &member).await;
        }

        async fn guild_member_removal(&self, ctx: Context, guild_id: GuildId, user: User, _member: Option<Member>) {
            self.cogs.dispatch_member_leave(&ctx, guild_id, &user).await;
        }

        async fn message_update(&self, ctx: Context, old: Option<Message>, new: Option<Message>, event: MessageUpdateEvent) {
            self.cogs.dispatch_message_update(&ctx, old, new, &event).await;
        }

        async fn message_delete(&self, ctx: Context, channel_id: ChannelId, msg_id: MessageId, guild_id: Option<GuildId>) {
            self.cogs.dispatch_message_delete(&ctx, channel_id, msg_id, guild_id).await;
        }

        async fn reaction_add(&self, ctx: Context, reaction: Reaction) {
            self.cogs.dispatch_reaction_add(&ctx, reaction).await;
        }

        async fn guild_create(&self, ctx: Context, guild: Guild, _is_new: Option<bool>) {
            self.cogs.dispatch_guild_create(&ctx, &guild).await;
        }

        async fn guild_delete(&self, ctx: Context, incomplete: UnavailableGuild, full: Option<Guild>) {
            self.cogs.dispatch_guild_delete(&ctx, incomplete, full).await;
        }

        async fn guild_member_update(&self, ctx: Context, old: Option<Member>, new: Option<Member>, event: GuildMemberUpdateEvent) {
            self.cogs.dispatch_member_update(&ctx, old, new, &event).await;
        }

        async fn guild_ban_addition(&self, ctx: Context, guild_id: GuildId, banned_user: User) {
            self.cogs.dispatch_member_ban(&ctx, guild_id, &banned_user).await;
        }

        async fn guild_ban_removal(&self, ctx: Context, guild_id: GuildId, unbanned_user: User) {
            self.cogs.dispatch_member_unban(&ctx, guild_id, &unbanned_user).await;
        }

        async fn channel_create(&self, ctx: Context, channel: GuildChannel) {
            self.cogs.dispatch_channel_create(&ctx, &channel).await;
        }

        async fn channel_delete(&self, ctx: Context, channel: GuildChannel, _messages: Option<Vec<Message>>) {
            self.cogs.dispatch_channel_delete(&ctx, &channel).await;
        }

        async fn guild_role_create(&self, ctx: Context, new: Role) {
            self.cogs.dispatch_role_create(&ctx, &new).await;
        }

        async fn guild_role_delete(&self, ctx: Context, guild_id: GuildId, removed_role_id: RoleId, removed_role_data: Option<Role>) {
            self.cogs.dispatch_role_delete(&ctx, guild_id, removed_role_id, removed_role_data).await;
        }

        async fn thread_create(&self, ctx: Context, thread: GuildChannel) {
            self.cogs.dispatch_thread_create(&ctx, &thread).await;
        }

        async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
            self.cogs.dispatch_voice_state_update(&ctx, old, &new).await;
        }

        async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
            match &interaction {
                Interaction::Command(_) | Interaction::Autocomplete(_) => {
                    slash::handle_interaction(&ctx, &interaction, &self.translate).await;
                }
                Interaction::Component(c) => {
                    self.cogs.dispatch_component(&ctx, c).await;
                }
                Interaction::Modal(m) => {
                    self.cogs.dispatch_modal(&ctx, m).await;
                }
                _ => {}
            }
        }
    }

    let mut manager = CogManager::new(config.prefix.clone());
    manager.register(BaseCog::new(app_state.clone()));
    manager.register(PrefixesCog::new(app_state.clone()));
    manager.register(AfkCog::new(app_state.clone()));
    manager.register(RemindersCog::new(app_state.clone()));
    manager.register(TagsCog::new(app_state.clone()));
    manager.register(WelcomeCog::new(app_state.clone()));
    manager.register(LoggingCog::new(app_state.clone()));
    manager.register(SettingsCog::new(app_state.clone()));
    manager.register(ModerationCog::new(app_state.clone()));
    manager.register(InfoCog::new(app_state.clone()));
    manager.register(HelpCog::new(app_state.clone()));
    manager.register(RolesCog::new(app_state.clone()));
    let translate_cog = TranslateCog::new(app_state.clone());
    manager.register(translate_cog.clone());
    manager.register(DictionaryCog::new(app_state.clone()));
    manager.register(OcrCog::new(app_state.clone()));
    manager.register(EmbedCog::new(app_state.clone()));
    manager.register(SentinelCog::new(app_state.clone()));
    manager.register(DevCog::new(app_state.clone()));
    manager.register(PremiumCog::new(app_state.clone()));
    manager.register(MusicCog::new(app_state.clone()));
    let manager = Arc::new(manager);

    // Keep a bounded message cache so logging can show edited/deleted content.
    let mut cache_settings = serenity::cache::Settings::default();
    cache_settings.max_messages = 1000;

    let mut client = Client::builder(token, intents)
        .cache_settings(cache_settings)
        .event_handler(Handler {
            cogs: manager.clone(),
            translate: translate_cog.clone(),
            state: app_state.clone(),
            reminder_task_started: std::sync::atomic::AtomicBool::new(false),
        })
        .await
        .map_err(|e| {
            error!(error = ?e, "failed to create serenity client");
            anyhow::anyhow!(e)
        })?;

    let api = http::router(app_state.clone());
    tokio::spawn(http::serve(api, "127.0.0.1:8080".parse().unwrap()));

    if let Err(e) = client.start().await {
        error!(error = ?e, "client exited with error");
    }

    Ok(())
}
