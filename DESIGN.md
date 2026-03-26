# Benny Bot — High-Level Design Document

> Reference design for a Rust rewrite of the Benny Discord bot.
> The original implementation is Python 3.12 + discord.py. This document captures all features, data models, and integration points in enough detail to fully reproduce the bot.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Suggested Rust Dependencies](#2-suggested-rust-dependencies)
3. [Configuration](#3-configuration)
4. [Architecture](#4-architecture)
5. [Data Storage](#5-data-storage)
6. [Bot Core](#6-bot-core)
7. [Modules](#7-modules)
   - 7.1 [AFK](#71-afk)
   - 7.2 [Reminders](#72-reminders)
   - 7.3 [Tags (TagScript)](#73-tags-tagscript)
   - 7.4 [Music](#74-music)
   - 7.5 [Moderation](#75-moderation)
   - 7.6 [Sentinel (Toxicity Detection)](#76-sentinel-toxicity-detection)
   - 7.7 [Translation](#77-translation)
   - 7.8 [Dictionary](#78-dictionary)
   - 7.9 [Settings & Prefixes](#79-settings--prefixes)
   - 7.10 [Help System](#710-help-system)
   - 7.11 [Info & Permissions](#711-info--permissions)
   - 7.12 [Welcome / Goodbye](#712-welcome--goodbye)
   - 7.13 [Logging (clogging)](#713-logging-clogging)
   - 7.14 [Embed Creator](#714-embed-creator)
   - 7.15 [Image OCR](#715-image-ocr)
   - 7.16 [Bulk Role Management](#716-bulk-role-management)
   - 7.17 [Events & Internals](#717-events--internals)
   - 7.18 [Developer Commands](#718-developer-commands)
   - 7.19 [Premium (Stub)](#719-premium-stub)
8. [Shared Utilities](#8-shared-utilities)
9. [Error Handling](#9-error-handling)
10. [Startup Sequence](#10-startup-sequence)
11. [Security Considerations](#11-security-considerations)

---

## 1. Overview

Benny Bot is a general-purpose Discord bot targeting small-to-medium guilds. Its primary feature set is:

| Category | Features |
|---|---|
| Utility | AFK, reminders, translation, dictionary, OCR |
| Server Management | Prefix configuration, welcome/goodbye, logging, bulk roles |
| Moderation | Warn/kick/ban, infraction tracking, AI toxicity detection |
| Entertainment | Music playback (YouTube/SoundCloud/Spotify), tag scripting |
| Meta | Per-guild settings, custom help, embed builder, dev tools |

The bot uses a **prefix command** system (`>` default, per-guild overrides) and Discord **slash commands** (application commands) in parallel.

---

## 2. Suggested Rust Dependencies

```toml
[dependencies]
# Discord
serenity      = { version = "0.12", features = ["client", "gateway", "model", "cache", "http", "voice"] }
poise         = "0.6"          # Command framework on top of serenity (prefix + slash)

# Async runtime
tokio         = { version = "1", features = ["full"] }

# HTTP client
reqwest       = { version = "0.12", features = ["json", "stream"] }

# SQLite (async)
sqlx          = { version = "0.7", features = ["sqlite", "runtime-tokio", "migrate", "macros"] }

# MongoDB
mongodb       = "2"

# Redis
redis         = { version = "0.25", features = ["tokio-comp", "connection-manager"] }

# Serialization
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"

# Configuration
config        = "0.14"         # Reads config.json / config.toml / env vars
dotenvy       = "0.15"         # .env file support

# Time & scheduling
chrono        = { version = "0.4", features = ["serde"] }
tokio-cron-scheduler = "0.10"  # Reminder / ping loop scheduling

# Music (Lavalink client)
lavalink-rs   = "0.12"         # Rust Lavalink client

# Translation
# Call Google Translate REST endpoint directly via reqwest (no official Rust SDK)

# NLP / Toxicity
# Run a local ONNX model via ort crate, or call a self-hosted REST endpoint
ort           = "2"            # ONNX Runtime bindings for toxicity model

# Image processing / OCR
image         = "0.25"
tesseract     = "0.14"         # Bindings for libtesseract

# Logging & diagnostics
tracing       = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# System info (dev commands)
sysinfo       = "0.30"

# Git operations (dev commands)
git2          = "0.19"

# Utility
uuid          = { version = "1", features = ["v4"] }
rand          = "0.8"
regex         = "1"
humantime     = "2"            # Parse human-readable durations ("1h30m")
once_cell     = "1"
dashmap       = "5"            # Concurrent hashmap for in-memory caches
```

---

## 3. Configuration

The bot reads `bot_config.json` (or equivalent env vars) at startup.

### Schema

```json
{
  "token":           "<discord bot token>",
  "dev_token":       "<optional dev token>",
  "prefix":          ">",
  "mongodb": {
    "benny_uri":      "<connection string>",
    "dictionary_uri": "<connection string>"
  },
  "redis": {
    "url":            "<redis://...>"
  },
  "lavalink": {
    "host":           "127.0.0.1",
    "port":           2333,
    "password":       "<password>",
    "search_source":  "ytsearch"
  },
  "spotify": {
    "client_id":      "<id>",
    "client_secret":  "<secret>"
  },
  "unsplash": {
    "access_key":     "<key>"
  },
  "cogs": []
}
```

`cogs` is an allowlist of module names to load. An empty array means load all.

---

## 4. Architecture

```
┌──────────────────────────────────────────────────────────┐
│                        bot core                          │
│  BennyBot { config, db, redis, http_client, caches, …}  │
└────────────────────────┬─────────────────────────────────┘
                         │  shared state (Arc<BotState>)
          ┌──────────────┼───────────────┐
          │              │               │
   ┌──────▼──────┐  ┌────▼────┐  ┌──────▼──────┐
   │  SQLite DBs │  │ MongoDB │  │    Redis     │
   │  users.db   │  │  benny  │  │  reminders   │
   │  servers.db │  │  dict   │  │  caches      │
   └─────────────┘  └─────────┘  └─────────────┘

          ┌──────────────┬───────────────┐
          │              │               │
   ┌──────▼──────┐  ┌────▼────┐  ┌──────▼──────┐
   │  Lavalink   │  │ Detoxify│  │   REST APIs  │
   │  (music)    │  │  (NLP)  │  │  translate   │
   └─────────────┘  └─────────┘  │  dictionary  │
                                  │  unsplash    │
                                  └─────────────┘
```

### Command Dispatch

- **Prefix commands** — resolved per guild (from DB cache), fallback to `@mention` or default `>`
- **Slash commands** — registered globally or per guild for dev
- Both command types share the same handler functions; the framework wraps context differences

---

## 5. Data Storage

### 5.1 SQLite — `users.db`

```sql
CREATE TABLE settings_users (
    user_id       INTEGER PRIMARY KEY,
    premium_level INTEGER NOT NULL DEFAULT 0,
    is_blacklisted INTEGER NOT NULL DEFAULT 0,  -- boolean
    timezone      TEXT    NOT NULL DEFAULT 'UTC'
);

CREATE TABLE reminders_users (
    user_id       INTEGER PRIMARY KEY,
    reminder_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE reminders_reminders (
    rid       INTEGER PRIMARY KEY AUTOINCREMENT,
    uid       INTEGER NOT NULL,
    fire_at   INTEGER NOT NULL,  -- unix timestamp
    content   TEXT    NOT NULL
);
```

### 5.2 SQLite — `servers.db`

```sql
CREATE TABLE settings_prefixes (
    guild_id INTEGER NOT NULL,
    prefix   TEXT    NOT NULL,
    PRIMARY KEY (guild_id, prefix)
);

CREATE TABLE base_afk (
    guild_id  INTEGER NOT NULL,
    user_id   INTEGER NOT NULL,
    message   TEXT    NOT NULL DEFAULT '',
    set_at    INTEGER NOT NULL,  -- unix timestamp
    PRIMARY KEY (guild_id, user_id)
);

CREATE TABLE tags (
    guild_id  INTEGER NOT NULL,
    name      TEXT    NOT NULL,
    content   TEXT    NOT NULL,
    owner_id  INTEGER NOT NULL,
    uses      INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (guild_id, name)
);

CREATE TABLE welcome_config (
    guild_id        INTEGER PRIMARY KEY,
    channel_id      INTEGER,
    embed_json      TEXT,
    enabled         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE goodbye_config (
    guild_id        INTEGER PRIMARY KEY,
    channel_id      INTEGER,
    embed_json      TEXT,
    enabled         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE logging_webhooks (
    guild_id    INTEGER PRIMARY KEY,
    webhook_url TEXT,
    enabled     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sentinel_config (
    guild_id            INTEGER PRIMARY KEY,
    enabled             INTEGER NOT NULL DEFAULT 0,
    log_channel_id      INTEGER,
    toxicity_threshold  REAL    NOT NULL DEFAULT 0.8,
    severe_toxicity_threshold REAL NOT NULL DEFAULT 0.5,
    obscene_threshold   REAL    NOT NULL DEFAULT 0.8,
    identity_attack_threshold REAL NOT NULL DEFAULT 0.7,
    insult_threshold    REAL    NOT NULL DEFAULT 0.8,
    threat_threshold    REAL    NOT NULL DEFAULT 0.7,
    sexual_explicit_threshold REAL NOT NULL DEFAULT 0.8
);
```

### 5.3 MongoDB — `benny` database

**Collection: `mod_cases`**
```json
{
  "_id": ObjectId,
  "guild_id": NumberLong,
  "case_number": NumberInt,
  "type": "warn|kick|ban|unban",
  "target_id": NumberLong,
  "moderator_id": NumberLong,
  "reason": String,
  "timestamp": Date,
  "active": Boolean
}
```

**Collection: `mod_counts`**
```json
{
  "_id": ObjectId,
  "guild_id": NumberLong,
  "case_count": NumberInt
}
```

### 5.4 Redis

| Key pattern | Type | Value | TTL |
|---|---|---|---|
| `reminder:count:{user_id}` | string | integer count | none |
| `prefix_cache:{guild_id}` | string | JSON array of prefixes | 5 min |
| General caching as needed | — | — | varies |

---

## 6. Bot Core

### BotState

The central state struct passed as `Arc<BotState>` into every command context:

```rust
pub struct BotState {
    pub config:        BotConfig,
    pub sqlite_users:  sqlx::SqlitePool,
    pub sqlite_servers: sqlx::SqlitePool,
    pub mongo:         mongodb::Client,
    pub redis:         redis::aio::ConnectionManager,
    pub http_client:   reqwest::Client,
    pub prefix_cache:  DashMap<u64, Vec<String>>,  // guild_id -> prefixes
    pub user_cache:    DashMap<u64, User>,           // user_id -> User model
    pub afk_cache:     DashMap<(u64, u64), AfkEntry>, // (guild, user) -> AFK
    pub tag_cache:     DashMap<u64, HashMap<String, Tag>>, // guild -> tags
    pub sentinel_managers: DashMap<u64, SentinelConfig>,
    pub lavalink:      lavalink_rs::LavalinkClient,
    pub active_reminders: Mutex<Vec<ActiveReminder>>,
}
```

### Dynamic Prefix Resolution

On every message:
1. Check `prefix_cache` for the guild's prefixes
2. If cache miss, query `settings_prefixes` from SQLite and populate cache
3. Also accept `@BennyBot ` (mention) as a valid prefix
4. Strip matched prefix and dispatch to command router

### Intents Required

```
GUILDS | GUILD_MEMBERS | GUILD_MESSAGES | GUILD_MESSAGE_REACTIONS
| GUILD_VOICE_STATES | MESSAGE_CONTENT | DIRECT_MESSAGES
```

`MESSAGE_CONTENT` is a privileged intent and must be enabled in the Discord Developer Portal.

---

## 7. Modules

### 7.1 AFK

**Purpose**: Let users mark themselves as AFK. When an AFK user is mentioned, the bot notifies the mentioner. When the AFK user sends a message (more than 3 seconds after being set), the AFK status is cleared.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `afk [message]` | optional message | Set AFK status with optional message |

**Data**: `base_afk` SQLite table, mirrored to `afk_cache`.

**Event handlers**:
- `on_message`: Check if message author is AFK → clear their AFK if ≥3s elapsed
- `on_message`: Scan mentions in message → if any mentioned user is AFK, reply with their AFK message

**AfkEntry struct**:
```rust
pub struct AfkEntry {
    pub guild_id: u64,
    pub user_id:  u64,
    pub message:  String,
    pub set_at:   i64,   // unix timestamp
}
```

---

### 7.2 Reminders

**Purpose**: Schedule DM reminders for users at a future time.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `remind <time> <text>` | duration or datetime | Create a reminder |
| `reminders list` | — | List active reminders |
| `reminders delete <id>` | reminder ID | Delete a reminder |

**Limits**: 10 active reminders per user.

**Time parsing**: Accept human-readable strings like `1h30m`, `tomorrow`, `2026-04-01 09:00`, etc. (`humantime` crate for durations; `chrono` for absolute datetimes).

**Scheduling**:
- On startup, load all pending reminders from SQLite into `active_reminders`
- Background task (tokio spawn) loops every 30 seconds, checks `fire_at` timestamps, fires DMs, removes from DB
- Redis key `reminder:count:{user_id}` tracks per-user count (also validated against SQLite)

**ActiveReminder struct**:
```rust
pub struct ActiveReminder {
    pub rid:     i64,
    pub uid:     u64,
    pub fire_at: i64,
    pub content: String,
}
```

---

### 7.3 Tags (TagScript)

**Purpose**: Per-guild custom commands with a lightweight scripting language (TagScript).

**Commands**:

| Command | Args | Description |
|---|---|---|
| `tag create <name> <content>` | — | Create a new tag |
| `tag delete <name>` | — | Delete a tag (owner or manage-guild) |
| `tag edit <name> <new content>` | — | Edit tag content |
| `tag info <name>` | — | Show tag metadata |
| `tag list` | — | List all tags in guild |
| `tag raw <name>` | — | Show raw tag content |
| `<tagname>` | — | Invoke tag (via on_message shortcut) |

**Tag struct**:
```rust
pub struct Tag {
    pub guild_id:   u64,
    pub name:       String,
    pub content:    String,
    pub owner_id:   u64,
    pub uses:       u64,
    pub created_at: i64,
}
```

**TagScript Engine**: Implement a minimal interpreter supporting the following block types (based on bTagScript):

| Block | Syntax | Description |
|---|---|---|
| Variable | `{user}`, `{target}`, `{channel}`, `{server}` | Context variables |
| Assign | `{=(name):value}` | Set a variable |
| Get | `{name}` | Get a variable value |
| If | `{if(condition):true\|false}` | Conditional |
| Range | `{range(min,max)}` | Random integer |
| Choice | `{choose:a\|b\|c}` | Random choice |
| Embed | `{embed(field):value}` | Compose an embed |
| React | `{react:emoji}` | React to the invoking message |
| Delete | `{delete}` | Delete the invoking message |
| Cooldown | `{cd(seconds)}` | Per-user cooldown |
| Redirect | `{redirect:channel}` | Send output to different channel |

Variables available at tag runtime:
- `{user}` / `{user.id}` / `{user.name}` / `{user.mention}` / `{user.avatar}`
- `{target}` — first mentioned user, fallback to invoker
- `{channel}` / `{channel.id}` / `{channel.name}`
- `{server}` / `{server.id}` / `{server.name}` / `{server.member_count}`
- `{args}` — everything after the tag name
- `{unix}` — current unix timestamp

---

### 7.4 Music

**Purpose**: Stream audio into voice channels via Lavalink.

**Commands**:

| Command | Aliases | Args | Description |
|---|---|---|---|
| `play <query>` | `p` | URL or search string | Search and queue a track |
| `disconnect` | `dc`, `leave` | — | Disconnect from voice |
| `pause` | — | — | Pause playback |
| `resume` | — | — | Resume playback |
| `skip` | — | — | Skip current track |
| `queue` | `q` | — | Show the queue |
| `nowplaying` | `np` | — | Show current track |
| `volume <1-100>` | `vol` | integer | Set volume |
| `stop` | — | — | Stop and clear queue |

**Lavalink integration** (`lavalink-rs`):
- Connect to server on `host:port` with `password`
- Default search source: `ytsearch` (configurable: `scsearch`, `ytmsearch`)
- Spotify URLs → resolve via Spotify API to track name → search on YouTube
- Handle Lavalink events: `TrackStart`, `TrackEnd`, `TrackException`, `TrackStuck`

**Custom exceptions** (map to user-facing error messages):

```rust
pub enum MusicError {
    QueueFull,
    QueueEmpty,
    NothingPlaying,
    NotConnected,
    AlreadyConnected,
    NotSameChannel,
    PlaylistTooLong,
    TrackNotFound,
    SpotifyNotResolved,
}
```

---

### 7.5 Moderation

**Purpose**: Basic moderation tooling with case tracking.

**Commands** (all require appropriate Discord permissions):

| Command | Args | Description |
|---|---|---|
| `warn <member> [reason]` | — | Issue a warning |
| `kick <member> [reason]` | — | Kick a member |
| `ban <member> [reason] [delete_days]` | — | Ban a member |
| `unban <user_id> [reason]` | — | Unban a user |
| `case <id>` | case number | Look up a case |
| `cases <member>` | — | List cases for a member |
| `modlog` | — | Show recent moderation actions |

**Case struct**:
```rust
pub struct ModCase {
    pub guild_id:     u64,
    pub case_number:  u32,
    pub action_type:  ModAction,  // Warn, Kick, Ban, Unban
    pub target_id:    u64,
    pub moderator_id: u64,
    pub reason:       String,
    pub timestamp:    DateTime<Utc>,
    pub active:       bool,
}

pub enum ModAction { Warn, Kick, Ban, Unban }
```

**Storage**: MongoDB `mod_cases` collection. `mod_counts` stores per-guild case counter (increment atomically on every new case).

---

### 7.6 Sentinel (Toxicity Detection)

**Purpose**: Automatically scan messages for toxic content using an ML model, log flagged messages to a configured webhook.

**Setup commands** (requires `Manage Guild`):

| Command | Args | Description |
|---|---|---|
| `sentinel enable` | — | Enable sentinel for guild |
| `sentinel disable` | — | Disable sentinel |
| `sentinel channel <#channel>` | — | Set log channel |
| `sentinel threshold <category> <0.0–1.0>` | — | Override threshold |

**Toxicity categories** (7 scores, each 0.0–1.0):

```rust
pub struct ToxicityScores {
    pub toxicity:          f32,
    pub severe_toxicity:   f32,
    pub obscene:           f32,
    pub identity_attack:   f32,
    pub insult:            f32,
    pub threat:            f32,
    pub sexual_explicit:   f32,
}
```

**Flow**:
1. On every `on_message` event in a sentinel-enabled guild, run the message content through the model
2. Compare each score to the guild's configured thresholds (default 0.7–0.8 depending on category)
3. If any threshold is exceeded, POST to the guild's logging webhook with:
   - The message content
   - Author info
   - Channel link
   - Per-category scores (color-coded)
4. Optionally delete the message (configurable)

**Model**: The original uses the Python `detoxify` library (Unitary toxic-bert). For Rust, run the same ONNX export via `ort`, or proxy calls to a local Python/FastAPI microservice.

---

### 7.7 Translation

**Purpose**: Translate text to any language using Google Translate.

**Commands**:

| Command | Aliases | Args | Description |
|---|---|---|---|
| `translate <text>` | `trans` | text + optional `--to <lang>` | Translate text |
| Context menu → "Translate" | — | — | Translate a message via right-click |

**Behavior**:
- Auto-detect source language
- Default target: English
- Respond with an embed showing original text, detected language, target language, and translated text
- Buttons: "Show Original" / "Show Translation" toggle

**API**: Call `https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl={target}&dt=t&q={text}` (unofficial free endpoint, no key required).

---

### 7.8 Dictionary

**Purpose**: Look up word definitions, pronunciations, and examples.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `define <word>` | — | Look up a word |

**API**: `https://api.dictionaryapi.dev/api/v2/entries/en/{word}` (free, no key).

**Response handling**:
- Multiple entries may be returned (different parts of speech)
- Use a select-menu dropdown to let the user choose between meanings
- Each meaning shows definitions with optional examples
- Phonetic text and audio link (if available)

**Data models**:
```rust
pub struct WordEntry {
    pub word:      String,
    pub phonetics: Vec<Phonetic>,
    pub meanings:  Vec<Meaning>,
}
pub struct Phonetic { pub text: Option<String>, pub audio: Option<String> }
pub struct Meaning  { pub part_of_speech: String, pub definitions: Vec<Definition> }
pub struct Definition { pub definition: String, pub example: Option<String>, pub synonyms: Vec<String> }
```

---

### 7.9 Settings & Prefixes

**Purpose**: Per-guild bot configuration.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `prefix add <prefix>` | — | Add a guild prefix |
| `prefix remove <prefix>` | — | Remove a guild prefix |
| `prefix list` | — | List all guild prefixes |
| `prefix reset` | — | Reset to default prefix |
| `settings timezone <tz>` | IANA tz string | Set user timezone |

**Prefix rules**:
- Maximum 5 custom prefixes per guild
- `@mention` always works regardless
- Prefixes stored in `settings_prefixes`, cached in `prefix_cache` (`DashMap<u64, Vec<String>>`)
- Original implementation uses a `:|:` separator in a single column; the Rust rewrite should use a proper join table as shown in the schema above

---

### 7.10 Help System

**Purpose**: Dynamic help command that lists commands grouped by category.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `help [command/category]` | — | Show help |

**Behavior**:
- Default invocation → paginated embed showing all categories with their commands
- `help <category>` → show all commands in that category with signatures and descriptions
- `help <command>` → detailed command page (usage, aliases, cooldown, required permissions)
- Use Discord component buttons for "Previous / Next" pagination
- Apply ANSI escape formatting for monospaced code blocks in the terminal (retain as plain text in Discord)

---

### 7.11 Info & Permissions

**Commands**:

| Command | Aliases | Args | Description |
|---|---|---|---|
| `info [member]` | `i` | optional member | Display user/member card |
| `permissions [member]` | `perms` | optional member | List permissions |
| `avatar [member]` | `av` | optional member | Show avatar |
| `about` | — | — | Bot information |

**`info` embed fields**: Username, nickname, ID, account created, joined server, roles (up to 20 shown), bot status, flags/badges.

**`about` fields**: Bot version, library (serenity), uptime, guild count, user count, total commands, source file count + char count.

---

### 7.12 Welcome / Goodbye

**Purpose**: Send a configurable embed when members join or leave the guild.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `welcome setup` | — | Interactive setup wizard |
| `welcome channel <#channel>` | — | Set welcome channel |
| `welcome message` | — | Open embed editor for message |
| `welcome enable` / `disable` | — | Toggle feature |
| `goodbye` (same subcommands) | — | Mirror commands for goodbye |

**Message content**: Stored as JSON embed definition in the DB. TagScript variables resolved at send-time:
- `{member}` / `{member.mention}` / `{member.name}` / `{member.id}` / `{member.avatar}`
- `{server.name}` / `{server.member_count}`

---

### 7.13 Logging (clogging)

**Purpose**: Log Discord server events to a webhook.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `logging setup <webhook_url>` | — | Configure logging webhook |
| `logging disable` | — | Disable logging |
| `logging test` | — | Send a test event |

**Logged events**: Message edit, message delete, member join, member leave, member ban/unban, role create/delete, channel create/delete.

**Delivery**: POST embed to the configured webhook URL with event details and timestamp.

---

### 7.14 Embed Creator

**Purpose**: Interactive UI for building and sending Discord embeds.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `embed create` | — | Open embed editor |
| `embed import <json>` | — | Import from JSON |

**Embed editor UI** (Discord components):
- Buttons to edit sections: Author, Base (title/description/color/URL), Images (thumbnail/image), Footer, Add Field, Remove Field
- Each button opens a Modal with pre-filled values
- Preview embed updates after each edit
- "Send" button with channel select
- "Export JSON" / "Export to Mystbin (pastebin)" options
- "Cancel" button

**Embed JSON schema** matches Discord's embed object format.

---

### 7.15 Image OCR

**Purpose**: Extract text from an image attachment using Tesseract.

**Commands**:

| Command | Args | Description |
|---|---|---|
| `ocr [image_url]` | optional URL | Read text from image |

**Behavior**:
- If no URL provided, use the most recent image attachment in the channel
- Download image → run through Tesseract → return extracted text
- Wrap in codeblock; if >2000 chars, upload to Mystbin

---

### 7.16 Bulk Role Management

**Purpose**: Assign or remove a role to/from all guild members.

**Commands** (require `Manage Roles` + `Manage Guild`):

| Command | Args | Description |
|---|---|---|
| `roleall <role>` | — | Give role to all members |
| `roleall remove <role>` | — | Remove role from all members |

**Behavior**:
- Confirm prompt before starting
- Progress message updated every N members (rate-limited to avoid hitting Discord API limits)
- 0.5s sleep between API calls to avoid rate-limit
- Final summary showing success/failure counts

---

### 7.17 Events & Internals

**Purpose**: Background tasks and guild lifecycle management.

**Auto-leave policy**: If the bot is added to a guild where >20% of members are bots OR there are fewer than 5 human members, automatically leave and log the action.

**Thread auto-join**: On `THREAD_CREATE`, join the thread if the bot has access.

**Ping loop**: Every 15 seconds, record gateway latency to an internal log (used by `about` command for "uptime" tracking).

**Command/interaction logging**: On every command invocation, log `{guild} / {channel} / {user}: {command}` to the terminal with color coding.

**`on_guild_join` / `on_guild_remove`**: Log guild name, ID, member count, and a rough bot-to-human ratio.

---

### 7.18 Developer Commands

These commands are owner-only (checked against a hardcoded owner ID).

| Command | Args | Description |
|---|---|---|
| `dev sysinfo` | — | CPU, RAM, disk, network stats |
| `dev gitpull` | — | Run `git pull` and report output |
| `dev eval <code>` | — | Execute arbitrary code (DANGER) |
| `dev redis <command>` | — | Execute a raw Redis command |
| `dev reload <cog>` | — | Hot-reload a command module |
| `dev load <cog>` | — | Load a module |
| `dev unload <cog>` | — | Unload a module |
| `dev logs [n]` | optional count | Show last N log entries |

---

### 7.19 Premium (Stub)

The premium system is defined but not implemented. Placeholder data models:

```rust
pub enum PremiumLevel { None = 0, Basic = 1, Pro = 2, Max = 3 }

pub struct PremiumToken {
    pub token:     String,
    pub level:     PremiumLevel,
    pub redeemed:  bool,
    pub owner_id:  Option<u64>,
}

pub struct PremiumSubscriber {
    pub user_id:   u64,
    pub level:     PremiumLevel,
    pub expires_at: Option<DateTime<Utc>>,
}
```

Premium gates in certain commands (sentinel thresholds, logging customization, cooldown reduction) check `user.premium_level > 0`.

---

## 8. Shared Utilities

### Colors (style module)

```rust
pub struct Color(pub u32);

impl Color {
    pub const BLURPLE:     Color = Color(0x5865F2);
    pub const GREEN:       Color = Color(0x57F287);
    pub const YELLOW:      Color = Color(0xFEE75C);
    pub const RED:         Color = Color(0xED4245);
    pub const WHITE:       Color = Color(0xFFFFFF);
    pub const DARK_GRAY:   Color = Color(0x2B2D31);
    pub const LIGHT_GRAY:  Color = Color(0x99AAB5);
    // ... ~20 total

    pub fn random() -> Color { /* pick one at random */ }
}
```

### Emojis

Define emoji constants (both Unicode and custom Discord emoji IDs) for consistent UI:
- Success / warning / error / loading indicators
- Music controls (play, pause, skip, stop, queue)
- Navigation arrows (for pagination)

### Loading Bar

```rust
pub fn loading_bar(current: usize, total: usize, width: usize) -> String {
    // Returns a string like: [████████░░░░] 66%
}
```

### File Stats

On startup, walk the `src/` directory, count `.rs` files, total lines, and total characters. Store in `BotState` for the `about` command.

### UserManager

```rust
pub struct User {
    pub user_id:       u64,
    pub premium_level: u8,
    pub is_blacklisted: bool,
    pub timezone:      String,
}

impl UserManager {
    /// Fetch from cache or DB; insert defaults if new user.
    pub async fn get_or_create(&self, user_id: u64) -> Result<User>;
    pub async fn is_blacklisted(&self, user_id: u64) -> bool;
    pub async fn is_owner(&self, user_id: u64) -> bool;
}
```

### Cooldowns

```rust
pub struct CooldownBucket {
    per_user:    DashMap<u64, Instant>,
    per_channel: DashMap<u64, Instant>,
    per_guild:   DashMap<u64, Instant>,
}
```

Premium users get reduced cooldown multipliers (default 0.5×).

---

## 9. Error Handling

Centralized error handler catches all command errors and produces a consistent embed:

| Error Type | User Message |
|---|---|
| Missing permissions | "You don't have permission to use this command." |
| Bot missing permissions | "I don't have the required permissions." |
| Command on cooldown | "Slow down! Try again in X seconds." |
| Member not found | "Could not find that member." |
| Bad argument | "Invalid argument: \<details\>." |
| Check failure | "You can't use this command here." |
| Music errors | Context-specific message (see §7.4) |
| Unexpected error | "Something went wrong. The error has been logged." + optional traceback to owner DM |

All errors are logged via `tracing` at the appropriate level. Unexpected errors additionally send a formatted traceback to a designated owner DM channel.

---

## 10. Startup Sequence

```
1. Load config from bot_config.json (or env vars)
2. Initialize tracing subscriber (log to stdout)
3. Open SQLite pools (users.db, servers.db) and run migrations
4. Connect to MongoDB
5. Connect to Redis
6. Build reqwest HTTP client
7. Initialize BotState with empty caches
8. Register all command modules (equivalent of loading cogs)
9. Start Lavalink connection
10. Connect to Discord gateway
11. on_ready:
    a. Load all guild prefixes into prefix_cache
    b. Load all tags into tag_cache
    c. Load all sentinel configs into sentinel_managers
    d. Load pending reminders → start reminder background task
    e. Load user cache (active users)
    f. Register slash commands (globally or per dev guild)
    g. Print startup banner to terminal
12. Enter event loop
```

---

## 11. Security Considerations

- **Secrets**: Never commit credentials. Use environment variables or a `.env` file excluded from version control. Load via `dotenvy`.
- **Owner checks**: `dev` commands must verify `ctx.author.id == config.owner_id`.
- **Eval command**: The `dev eval` command executes arbitrary code at runtime. In Rust this is significantly harder to implement safely than Python. Consider restricting to pre-approved script snippets or removing entirely.
- **Input validation**: Tag names, prefix strings, and reminder content must be length-checked before DB insertion.
- **Rate limiting**: Respect Discord rate limits. Use serenity's built-in rate-limit handling. Add per-guild/per-user cooldowns on expensive commands (OCR, translate, music play).
- **SQL injection**: Use parameterized queries exclusively via `sqlx` macros (`query!`, `query_as!`).
- **Webhook URLs**: Treat stored webhook URLs as secrets; never expose them in command output.
