//! Entity for `mod_config` (per-guild moderation settings; PK `guild_id`).
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "mod_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    pub mute_role_id: Option<i64>,
    /// Active warns that trigger auto-punishment; 0 disables escalation.
    pub warn_threshold: i64,
    /// One of "timeout" | "kick" | "ban".
    pub warn_action: String,
    /// Timeout length applied when `warn_action` is "timeout".
    pub warn_timeout_secs: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
