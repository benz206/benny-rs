//! Entity for `levels_config` (per-guild leveling settings; PK `guild_id`).
//! `levelup_channel_id` NULL means announce in the channel the member spoke in.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "levels_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    pub enabled: bool,
    pub announce: bool,
    pub levelup_channel_id: Option<i64>,
    pub xp_min: i64,
    pub xp_max: i64,
    pub cooldown_secs: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
