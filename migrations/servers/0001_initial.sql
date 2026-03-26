CREATE TABLE IF NOT EXISTS tags_tags (
    guild_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    content TEXT NOT NULL,
    owner_id INTEGER NOT NULL DEFAULT 0,
    uses INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, name)
);

CREATE TABLE IF NOT EXISTS settings_prefixes (
    guild_id INTEGER NOT NULL,
    prefix TEXT NOT NULL,
    PRIMARY KEY (guild_id, prefix)
);

CREATE TABLE IF NOT EXISTS sentinels_config (
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
);

CREATE TABLE IF NOT EXISTS sentinels_decancer (
    guild_id INTEGER PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS base_afk (
    guild_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    message TEXT NOT NULL DEFAULT '',
    set_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE IF NOT EXISTS welcome_config (
    guild_id INTEGER PRIMARY KEY,
    channel_id INTEGER,
    message TEXT NOT NULL DEFAULT 'Welcome {member.mention} to {server}!',
    embed_json TEXT,
    enabled INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS goodbye_config (
    guild_id INTEGER PRIMARY KEY,
    channel_id INTEGER,
    message TEXT NOT NULL DEFAULT 'Goodbye {member.name}!',
    embed_json TEXT,
    enabled INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS logging_webhooks (
    guild_id INTEGER PRIMARY KEY,
    webhook_url TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 0
);
