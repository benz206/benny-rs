//! Entity for `starboard_posts` (mirror of a starred message on the
//! starboard; PK `(guild_id, message_id)` — the ORIGINAL message id).
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "starboard_posts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: i64,
    pub starboard_message_id: i64,
    pub star_count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
