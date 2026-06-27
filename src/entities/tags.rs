//! Entity for the `tags_tags` table (see `db::ensure_servers_schema`).
//!
//! Composite primary key `(guild_id, name)`; `auto_increment = false` because
//! both key columns are supplied by the application, not the database.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "tags_tags")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    pub content: String,
    pub owner_id: i64,
    pub uses: i64,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
