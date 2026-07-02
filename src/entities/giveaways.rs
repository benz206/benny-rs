//! Entity for `giveaways` (one row per giveaway; `ended` flips when winners
//! are drawn; `message_id` is the announcement message carrying the entry
//! button, filled in right after posting).
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "giveaways")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub message_id: i64,
    pub prize: String,
    pub winners: i64,
    pub host_id: i64,
    pub ends_at: i64,
    pub ended: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
