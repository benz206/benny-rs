//! SeaORM entity definitions.
//!
//! Each module maps one SQLite table to a typed `Entity`/`Model`/`ActiveModel`
//! trio so cogs can query without hand-written SQL. The table schemas live in
//! `migrations`; these entities describe the existing tables for querying.
//!
//! Boolean-ish `INTEGER` columns (enabled / redeemed / is_blacklisted) are
//! mapped to `bool` — SeaORM stores them as 0/1, matching the old schema.

// servers.db
pub mod afk;
pub mod automod_config;
pub mod goodbye_config;
pub mod logging;
pub mod mod_cases;
pub mod mod_config;
pub mod mod_timed;
pub mod prefixes;
pub mod sentinel_config;
pub mod sentinels_decancer;
pub mod sticky_roles;
pub mod sticky_roles_config;
pub mod tags;
pub mod welcome_autoroles;
pub mod welcome_config;

// users.db
pub mod premium_tokens;
pub mod reminders;
pub mod reminders_users;
pub mod settings_users;
