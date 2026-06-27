//! Entity for `sentinels_config` (per-guild toxicity thresholds).
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sentinels_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    pub enabled: bool,
    pub log_channel_id: Option<i64>,
    pub toxicity: f64,
    pub severe_toxicity: f64,
    pub obscene: f64,
    pub threat: f64,
    pub insult: f64,
    pub identity_attack: f64,
    pub sexual_explicit: f64,
    /// Whether flagged messages are auto-deleted. Historically added to the
    /// table via a runtime `ALTER TABLE`; now part of the migration schema.
    pub delete_flagged: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
