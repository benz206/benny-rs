use sqlx::{Executor, SqlitePool};

pub async fn ensure_servers_schema(pool: &SqlitePool) -> sqlx::Result<()> {
    let tags = r#"
        CREATE TABLE IF NOT EXISTS tags_tags (
            guild_id TEXT NOT NULL,
            name TEXT NOT NULL,
            content TEXT NOT NULL,
            uses INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (guild_id, name)
        )
    "#;

    let settings_prefixes = r#"
        CREATE TABLE IF NOT EXISTS settings_prefixes (
            guild_id TEXT PRIMARY KEY,
            prefixes TEXT NOT NULL
        )
    "#;

    let sentinels_config = r#"
        CREATE TABLE IF NOT EXISTS sentinels_config (
            guild_id TEXT PRIMARY KEY,
            toxicity REAL DEFAULT 0.85
        )
    "#;

    let sentinels_decancer = r#"
        CREATE TABLE IF NOT EXISTS sentinels_decancer (
            guild_id TEXT PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 0
        )
    "#;

    pool.execute(tags).await?;
    pool.execute(settings_prefixes).await?;
    pool.execute(sentinels_config).await?;
    pool.execute(sentinels_decancer).await?;
    Ok(())
}

pub async fn ensure_users_schema(pool: &SqlitePool) -> sqlx::Result<()> {
    let users = r#"
        CREATE TABLE IF NOT EXISTS settings_users (
            user_id TEXT PRIMARY KEY,
            timezone TEXT,
            patron_level INTEGER DEFAULT 0
        )
    "#;

    let reminders = r#"
        CREATE TABLE IF NOT EXISTS reminders_reminders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL,
            message TEXT NOT NULL,
            when_utc INTEGER NOT NULL
        )
    "#;

    pool.execute(users).await?;
    pool.execute(reminders).await?;
    Ok(())
}


