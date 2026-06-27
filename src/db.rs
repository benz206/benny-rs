use sqlx::SqlitePool;

pub async fn ensure_servers_schema(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tags_tags (
            guild_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            content TEXT NOT NULL,
            owner_id INTEGER NOT NULL DEFAULT 0,
            uses INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (guild_id, name)
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS settings_prefixes (
            guild_id INTEGER NOT NULL,
            prefix TEXT NOT NULL,
            PRIMARY KEY (guild_id, prefix)
        )"
    ).execute(pool).await?;

    sqlx::query(
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
            sexual_explicit REAL NOT NULL DEFAULT 0.85
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sentinels_decancer (
            guild_id INTEGER PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 0,
            log_channel_id INTEGER
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS base_afk (
            guild_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            message TEXT NOT NULL DEFAULT '',
            set_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (guild_id, user_id)
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS welcome_config (
            guild_id INTEGER PRIMARY KEY,
            channel_id INTEGER,
            message TEXT NOT NULL DEFAULT 'Welcome {member.mention} to {server}!',
            embed_json TEXT,
            enabled INTEGER NOT NULL DEFAULT 0
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS goodbye_config (
            guild_id INTEGER PRIMARY KEY,
            channel_id INTEGER,
            message TEXT NOT NULL DEFAULT 'Goodbye {member.name}!',
            embed_json TEXT,
            enabled INTEGER NOT NULL DEFAULT 0
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS logging_webhooks (
            guild_id INTEGER PRIMARY KEY,
            webhook_url TEXT NOT NULL DEFAULT '',
            enabled INTEGER NOT NULL DEFAULT 0
        )"
    ).execute(pool).await?;

    // Welcome/goodbye auto-assigned roles (one row per role per guild).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS welcome_autoroles (
            guild_id INTEGER NOT NULL,
            role_id INTEGER NOT NULL,
            PRIMARY KEY (guild_id, role_id)
        )"
    ).execute(pool).await?;

    // Sticky roles: persisted role ids (comma-separated) to reapply on rejoin.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sticky_roles (
            guild_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            role_ids TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (guild_id, user_id)
        )"
    ).execute(pool).await?;

    // Whether sticky roles are enabled per guild.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sticky_roles_config (
            guild_id INTEGER PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 0
        )"
    ).execute(pool).await?;

    // Per-guild moderation config (e.g. mute role).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mod_config (
            guild_id INTEGER PRIMARY KEY,
            mute_role_id INTEGER
        )"
    ).execute(pool).await?;

    // Active timed infractions (mutes / temp-bans) polled by the expiry task.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mod_timed (
            guild_id INTEGER NOT NULL,
            case_number INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            action TEXT NOT NULL,
            expires_at INTEGER NOT NULL,
            PRIMARY KEY (guild_id, case_number)
        )"
    ).execute(pool).await?;

    Ok(())
}

pub async fn ensure_users_schema(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS settings_users (
            user_id INTEGER PRIMARY KEY,
            timezone TEXT,
            patron_level INTEGER NOT NULL DEFAULT 0,
            is_blacklisted INTEGER NOT NULL DEFAULT 0
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS reminders_reminders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            content TEXT NOT NULL,
            fire_at INTEGER NOT NULL
        )"
    ).execute(pool).await?;

    // Per-user reminder counter (mirrors Redis reminder:count:{user_id}).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS reminders_users (
            user_id INTEGER PRIMARY KEY,
            reminder_count INTEGER NOT NULL DEFAULT 0
        )"
    ).execute(pool).await?;

    // Premium redemption tokens.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS premium_tokens (
            token TEXT PRIMARY KEY,
            level INTEGER NOT NULL DEFAULT 0,
            redeemed INTEGER NOT NULL DEFAULT 0,
            owner_id INTEGER
        )"
    ).execute(pool).await?;

    Ok(())
}
