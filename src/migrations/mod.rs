//! SeaORM migration system. Replaces the old inline `CREATE TABLE` calls in
//! `db.rs`. Two migrators, one per database (`servers.db` / `users.db`), each
//! run at startup against the shared SeaORM connection.
//!
//! The table DDL is kept verbatim (with `IF NOT EXISTS` and the original
//! `DEFAULT` clauses) because several upserts insert only a subset of columns
//! and rely on the column defaults — so the schema must match byte-for-byte.
//! The SeaORM migration framework still tracks applied versions in
//! `seaql_migrations`, giving us ordered, versioned, reversible migrations.
use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

pub struct ServersMigrator;
pub struct UsersMigrator;

impl MigratorTrait for ServersMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(InitServers),
            Box::new(AddModCases),
            Box::new(AddWarnPolicy),
            Box::new(AddAutomod),
            Box::new(AddEngagement),
        ]
    }
}

impl MigratorTrait for UsersMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(InitUsers)]
    }
}

async fn run_all(manager: &SchemaManager<'_>, statements: &[&str]) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    for sql in statements {
        conn.execute_unprepared(sql).await?;
    }
    Ok(())
}

async fn drop_all(manager: &SchemaManager<'_>, tables: &[&str]) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    for table in tables {
        conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// servers.db
// ---------------------------------------------------------------------------
struct InitServers;

impl MigrationName for InitServers {
    fn name(&self) -> &str {
        "m20260627_000001_init_servers"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for InitServers {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(manager, SERVERS_TABLES).await?;
        // Upgrade path for a pre-existing servers.db whose `sentinels_config`
        // was created (by the old db.rs) without `delete_flagged`. Fresh DBs
        // already have the column from the CREATE above, so the duplicate-column
        // error here is expected and ignored.
        let _ = manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE sentinels_config ADD COLUMN delete_flagged INTEGER NOT NULL DEFAULT 0",
            )
            .await;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_all(manager, SERVERS_TABLE_NAMES).await
    }
}

const SERVERS_TABLE_NAMES: &[&str] = &[
    "tags_tags",
    "settings_prefixes",
    "sentinels_config",
    "sentinels_decancer",
    "base_afk",
    "welcome_config",
    "goodbye_config",
    "logging_webhooks",
    "welcome_autoroles",
    "sticky_roles",
    "sticky_roles_config",
    "mod_config",
    "mod_timed",
];

const SERVERS_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS tags_tags (
        guild_id INTEGER NOT NULL,
        name TEXT NOT NULL,
        content TEXT NOT NULL,
        owner_id INTEGER NOT NULL DEFAULT 0,
        uses INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (guild_id, name)
    )",
    "CREATE TABLE IF NOT EXISTS settings_prefixes (
        guild_id INTEGER NOT NULL,
        prefix TEXT NOT NULL,
        PRIMARY KEY (guild_id, prefix)
    )",
    "CREATE TABLE IF NOT EXISTS sentinels_config (
        guild_id INTEGER PRIMARY KEY,
        enabled INTEGER NOT NULL DEFAULT 0,
        log_channel_id INTEGER,
        toxicity REAL NOT NULL DEFAULT 0.85,
        severe_toxicity REAL NOT NULL DEFAULT 0.85,
        obscene REAL NOT NULL DEFAULT 0.85,
        threat REAL NOT NULL DEFAULT 0.85,
        insult REAL NOT NULL DEFAULT 0.85,
        identity_attack REAL NOT NULL DEFAULT 0.85,
        sexual_explicit REAL NOT NULL DEFAULT 0.85,
        delete_flagged INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS sentinels_decancer (
        guild_id INTEGER PRIMARY KEY,
        enabled INTEGER NOT NULL DEFAULT 0,
        log_channel_id INTEGER
    )",
    "CREATE TABLE IF NOT EXISTS base_afk (
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        message TEXT NOT NULL DEFAULT '',
        set_at INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (guild_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS welcome_config (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER,
        message TEXT NOT NULL DEFAULT 'Welcome {member.mention} to {server}!',
        embed_json TEXT,
        enabled INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS goodbye_config (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER,
        message TEXT NOT NULL DEFAULT 'Goodbye {member.name}!',
        embed_json TEXT,
        enabled INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS logging_webhooks (
        guild_id INTEGER PRIMARY KEY,
        webhook_url TEXT NOT NULL DEFAULT '',
        enabled INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS welcome_autoroles (
        guild_id INTEGER NOT NULL,
        role_id INTEGER NOT NULL,
        PRIMARY KEY (guild_id, role_id)
    )",
    "CREATE TABLE IF NOT EXISTS sticky_roles (
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        role_ids TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (guild_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS sticky_roles_config (
        guild_id INTEGER PRIMARY KEY,
        enabled INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS mod_config (
        guild_id INTEGER PRIMARY KEY,
        mute_role_id INTEGER
    )",
    "CREATE TABLE IF NOT EXISTS mod_timed (
        guild_id INTEGER NOT NULL,
        case_number INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        action TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (guild_id, case_number)
    )",
];

// ---------------------------------------------------------------------------
// servers.db — moderation cases (added after the initial schema; replaces the
// old MongoDB `mod_cases` / `mod_counts` collections so the bot is SQL-only).
// ---------------------------------------------------------------------------
struct AddModCases;

impl MigrationName for AddModCases {
    fn name(&self) -> &str {
        "m20260628_000002_add_mod_cases"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddModCases {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(
            manager,
            &["CREATE TABLE IF NOT EXISTS mod_cases (
                guild_id INTEGER NOT NULL,
                case_number INTEGER NOT NULL,
                action_type TEXT NOT NULL,
                target_id INTEGER NOT NULL,
                moderator_id INTEGER NOT NULL,
                reason TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL DEFAULT 0,
                active INTEGER NOT NULL DEFAULT 1,
                expires_at INTEGER,
                PRIMARY KEY (guild_id, case_number)
            )"],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_all(manager, &["mod_cases"]).await
    }
}

// ---------------------------------------------------------------------------
// servers.db — warn-escalation policy on mod_config (auto-punish once a member
// accumulates `warn_threshold` active warns; 0 = disabled).
// ---------------------------------------------------------------------------
struct AddWarnPolicy;

impl MigrationName for AddWarnPolicy {
    fn name(&self) -> &str {
        "m20260702_000003_add_warn_policy"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddWarnPolicy {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(
            manager,
            &[
                "ALTER TABLE mod_config ADD COLUMN warn_threshold INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE mod_config ADD COLUMN warn_action TEXT NOT NULL DEFAULT 'timeout'",
                "ALTER TABLE mod_config ADD COLUMN warn_timeout_secs INTEGER NOT NULL DEFAULT 3600",
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(
            manager,
            &[
                "ALTER TABLE mod_config DROP COLUMN warn_threshold",
                "ALTER TABLE mod_config DROP COLUMN warn_action",
                "ALTER TABLE mod_config DROP COLUMN warn_timeout_secs",
            ],
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// servers.db — bot-side automod filters + anti-raid settings.
// ---------------------------------------------------------------------------
struct AddAutomod;

impl MigrationName for AddAutomod {
    fn name(&self) -> &str {
        "m20260702_000004_add_automod"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddAutomod {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(
            manager,
            &["CREATE TABLE IF NOT EXISTS automod_config (
                guild_id INTEGER PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 0,
                log_channel_id INTEGER,
                anti_invite INTEGER NOT NULL DEFAULT 1,
                anti_link INTEGER NOT NULL DEFAULT 0,
                mention_limit INTEGER NOT NULL DEFAULT 8,
                spam_msgs INTEGER NOT NULL DEFAULT 8,
                spam_secs INTEGER NOT NULL DEFAULT 5,
                punishment TEXT NOT NULL DEFAULT 'delete',
                timeout_secs INTEGER NOT NULL DEFAULT 600,
                raid_enabled INTEGER NOT NULL DEFAULT 0,
                raid_joins INTEGER NOT NULL DEFAULT 10,
                raid_secs INTEGER NOT NULL DEFAULT 30,
                min_account_age_days INTEGER NOT NULL DEFAULT 0,
                raid_action TEXT NOT NULL DEFAULT 'alert'
            )"],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(manager, &["DROP TABLE IF EXISTS automod_config"]).await
    }
}

// ---------------------------------------------------------------------------
// servers.db — engagement features: leveling/XP, starboard, giveaways.
// ---------------------------------------------------------------------------
struct AddEngagement;

impl MigrationName for AddEngagement {
    fn name(&self) -> &str {
        "m20260702_000005_add_engagement"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddEngagement {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(
            manager,
            &[
                "CREATE TABLE IF NOT EXISTS levels_config (
                    guild_id INTEGER PRIMARY KEY,
                    enabled INTEGER NOT NULL DEFAULT 0,
                    announce INTEGER NOT NULL DEFAULT 1,
                    levelup_channel_id INTEGER,
                    xp_min INTEGER NOT NULL DEFAULT 15,
                    xp_max INTEGER NOT NULL DEFAULT 25,
                    cooldown_secs INTEGER NOT NULL DEFAULT 60
                )",
                "CREATE TABLE IF NOT EXISTS levels_users (
                    guild_id INTEGER NOT NULL,
                    user_id INTEGER NOT NULL,
                    xp INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (guild_id, user_id)
                )",
                "CREATE TABLE IF NOT EXISTS levels_rewards (
                    guild_id INTEGER NOT NULL,
                    level INTEGER NOT NULL,
                    role_id INTEGER NOT NULL,
                    PRIMARY KEY (guild_id, level)
                )",
                "CREATE TABLE IF NOT EXISTS starboard_config (
                    guild_id INTEGER PRIMARY KEY,
                    enabled INTEGER NOT NULL DEFAULT 0,
                    channel_id INTEGER,
                    threshold INTEGER NOT NULL DEFAULT 3,
                    emoji TEXT NOT NULL DEFAULT '⭐',
                    self_star INTEGER NOT NULL DEFAULT 0
                )",
                "CREATE TABLE IF NOT EXISTS starboard_posts (
                    guild_id INTEGER NOT NULL,
                    message_id INTEGER NOT NULL,
                    starboard_message_id INTEGER NOT NULL,
                    star_count INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (guild_id, message_id)
                )",
                "CREATE TABLE IF NOT EXISTS giveaways (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    guild_id INTEGER NOT NULL,
                    channel_id INTEGER NOT NULL,
                    message_id INTEGER NOT NULL DEFAULT 0,
                    prize TEXT NOT NULL,
                    winners INTEGER NOT NULL DEFAULT 1,
                    host_id INTEGER NOT NULL,
                    ends_at INTEGER NOT NULL,
                    ended INTEGER NOT NULL DEFAULT 0
                )",
                "CREATE TABLE IF NOT EXISTS giveaway_entries (
                    giveaway_id INTEGER NOT NULL,
                    user_id INTEGER NOT NULL,
                    PRIMARY KEY (giveaway_id, user_id)
                )",
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(
            manager,
            &[
                "DROP TABLE IF EXISTS levels_config",
                "DROP TABLE IF EXISTS levels_users",
                "DROP TABLE IF EXISTS levels_rewards",
                "DROP TABLE IF EXISTS starboard_config",
                "DROP TABLE IF EXISTS starboard_posts",
                "DROP TABLE IF EXISTS giveaways",
                "DROP TABLE IF EXISTS giveaway_entries",
            ],
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// users.db
// ---------------------------------------------------------------------------
struct InitUsers;

impl MigrationName for InitUsers {
    fn name(&self) -> &str {
        "m20260627_000001_init_users"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for InitUsers {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_all(manager, USERS_TABLES).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_all(manager, USERS_TABLE_NAMES).await
    }
}

const USERS_TABLE_NAMES: &[&str] = &[
    "settings_users",
    "reminders_reminders",
    "reminders_users",
    "premium_tokens",
];

const USERS_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS settings_users (
        user_id INTEGER PRIMARY KEY,
        timezone TEXT,
        patron_level INTEGER NOT NULL DEFAULT 0,
        is_blacklisted INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS reminders_reminders (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id INTEGER NOT NULL,
        content TEXT NOT NULL,
        fire_at INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS reminders_users (
        user_id INTEGER PRIMARY KEY,
        reminder_count INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS premium_tokens (
        token TEXT PRIMARY KEY,
        level INTEGER NOT NULL DEFAULT 0,
        redeemed INTEGER NOT NULL DEFAULT 0,
        owner_id INTEGER
    )",
];
