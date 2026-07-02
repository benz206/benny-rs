//! Entity for `starboard_config` (per-guild starboard settings; PK `guild_id`).
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "starboard_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    pub enabled: bool,
    pub channel_id: Option<i64>,
    pub threshold: i64,
    pub emoji: String,
    pub self_star: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
