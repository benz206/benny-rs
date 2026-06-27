# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                  # compile (dev)
cargo build --release        # compile (release)
cargo run                    # run the bot (uses dev_token in debug builds)
cargo check                  # fast type-check without linking
cargo clippy                 # lint
```

There are no tests. The bot requires `config.json` in the working directory and a `databases/` directory (auto-created on first run) for SQLite files.

## Architecture

### Core flow

`main.rs` wires everything together: loads `config.json` → connects SQLite (two pools) + MongoDB + Redis → constructs `Arc<AppState>` → registers all cogs into `CogManager` → starts the serenity Discord client + Axum HTTP server.

In debug builds (`cargo run`), `config.dev_token` is used instead of `token`.

### AppState (`src/state.rs`)

The single shared state passed via `Arc<AppState>` into every cog constructor. Contains:
- `http` — reqwest client for external API calls
- `servers_db` / `users_db` — two SQLite pools (guild data vs user data)
- `mongo` / `redis` — both `Option<T>`; the bot runs without them (moderation cases and caching degrade gracefully)
- Seven `DashMap` caches: `prefix_cache`, `afk_cache`, `tag_cache`, `sentinel_cache`, `welcome_cache`, `goodbye_cache`, `logging_cache` — populated in each cog's `on_ready` from the DB

### Cog system (`src/cogs/mod.rs`)

The `Cog` trait defines 11 event hooks (all default no-ops). `CogManager` holds a `Vec<Arc<dyn Cog>>` and fans out each Discord event to every registered cog. Cogs capture `Arc<AppState>` in their constructor — the trait method signatures never carry state.

To add a new cog:
1. Create `src/cogs/my_cog.rs` implementing `Cog`
2. Add `pub mod my_cog;` to `src/cogs/mod.rs`
3. Register `MyCog::new(app_state.clone())` in `main.rs`

### Database

- `src/db.rs` — SQLite schema creation (inline `CREATE TABLE IF NOT EXISTS`, called at startup)
- `src/db_mongo.rs` — MongoDB helpers for moderation cases (`mod_cases`, `mod_counts` collections in the `benny` database)
- SQLite `servers.db`: tags, prefixes, sentinel config, AFK, welcome/goodbye/logging config
- SQLite `users.db`: user settings (patron level, blacklist, timezone), reminders

### TagScript engine (`src/tagscript/`)

A mini template interpreter used by the Tags cog and Welcome/Goodbye messages. `run(template, &mut TagContext) -> TagOutput`. The lexer splits `{...}` blocks from literals; blocks are dispatched to handlers in `blocks.rs`. `TagOutput` carries side-effects: `react_emojis`, `delete_invoke`, `redirect_channel`.

### Background tasks (`src/tasks/`)

`spawn_reminder_task(state, http)` is called once from `on_ready` (guarded by `AtomicBool`). It polls `reminders_reminders WHERE fire_at <= now()` every 30 seconds and sends DMs.

### HTTP API (`src/http.rs`)

Axum server on `127.0.0.1:8080`. Endpoints: `GET /` (alive), `GET /ping` (latency history), `GET /health` (service status), `GET /stats` (version + uptime).

### Config (`src/config.rs`)

Flat `BotConfig` deserialized from `config.json`. All fields except `token` have defaults so the bot starts with a minimal config. `sentiment_api_url` enables the Sentinel toxicity feature (points to an external HTTP service returning `{ "toxicity": 0.0..1.0 }`).

### Slash commands (`src/slash.rs`)

Currently only `/ping` is registered globally. New slash commands are registered in `register_global()` and dispatched in `handle_interaction()`.

### Privileged gateway intents

`GUILD_MEMBERS` and `MESSAGE_CONTENT` are privileged — they must be enabled in the Discord Developer Portal for the bot application, or the bot will silently receive no member/message events.
