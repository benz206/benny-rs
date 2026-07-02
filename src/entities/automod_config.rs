//! Entity for `automod_config` (per-guild automod + anti-raid settings; PK
//! `guild_id`). Punishment is one of "delete" | "warn" | "timeout" | "kick";
//! raid_action is "alert" | "kick". Zeroed numeric limits disable their
//! filter.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "automod_config")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub guild_id: i64,
    pub enabled: bool,
    pub log_channel_id: Option<i64>,
    pub anti_invite: bool,
    pub anti_link: bool,
    pub mention_limit: i64,
    pub spam_msgs: i64,
    pub spam_secs: i64,
    pub punishment: String,
    pub timeout_secs: i64,
    pub raid_enabled: bool,
    pub raid_joins: i64,
    pub raid_secs: i64,
    pub min_account_age_days: i64,
    pub raid_action: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
