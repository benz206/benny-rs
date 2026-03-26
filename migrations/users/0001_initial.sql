CREATE TABLE IF NOT EXISTS settings_users (
    user_id INTEGER PRIMARY KEY,
    timezone TEXT,
    patron_level INTEGER NOT NULL DEFAULT 0,
    is_blacklisted INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS reminders_reminders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL,
    content TEXT NOT NULL,
    fire_at INTEGER NOT NULL
);
