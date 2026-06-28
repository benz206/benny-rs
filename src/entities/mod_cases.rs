//! Entity for `mod_cases` (moderation case log; composite PK `(guild_id, case_number)`).
//! Replaces the old MongoDB `mod_cases` collection.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "mod_cases")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub case_number: i64,
    pub action_type: String,
    pub target_id: i64,
    pub moderator_id: i64,
    pub reason: String,
    /// Unix seconds at which the action was taken.
    pub created_at: i64,
    pub active: bool,
    /// Unix seconds at which a timed infraction lifts; `None` for permanent ones.
    pub expires_at: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
