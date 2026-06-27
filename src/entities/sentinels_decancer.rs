//! Entity for `sentinels_decancer` (per-guild name-sanitization toggle).
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "sentinels_decancer")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    pub enabled: bool,
    pub log_channel_id: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
