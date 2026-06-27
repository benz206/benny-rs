use mongodb::{
    Client as MongoClient,
    bson::{Document, doc},
    options::ReturnDocument,
};
use serde::{Deserialize, Serialize};
use serenity::futures::TryStreamExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModCase {
    pub guild_id: i64,
    pub case_number: i64,
    pub action_type: String,
    pub target_id: i64,
    pub moderator_id: i64,
    pub reason: String,
    pub timestamp: String, // ISO 8601 string
    pub active: bool,
    /// Unix timestamp at which a timed infraction (mute / temp-ban) lifts.
    /// `None` for permanent / instantaneous actions. `#[serde(default)]` keeps
    /// older documents (written before this field existed) deserializable.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

pub fn mod_cases_collection(client: &MongoClient) -> mongodb::Collection<ModCase> {
    client.database("benny").collection("mod_cases")
}

pub fn mod_counts_collection(client: &MongoClient) -> mongodb::Collection<Document> {
    client.database("benny").collection("mod_counts")
}

/// Atomically increment case count for a guild and return the new case number.
pub async fn next_case_number(client: &MongoClient, guild_id: i64) -> mongodb::error::Result<i64> {
    let collection = mod_counts_collection(client);
    let filter = doc! { "guild_id": guild_id };
    let update = doc! { "$inc": { "case_count": 1_i64 } };

    let doc = collection
        .find_one_and_update(filter, update)
        .upsert(true)
        .return_document(ReturnDocument::After)
        .await?;

    let count = doc.and_then(|d| d.get_i64("case_count").ok()).unwrap_or(1);
    Ok(count)
}

pub async fn insert_case(client: &MongoClient, case: &ModCase) -> mongodb::error::Result<()> {
    let collection = mod_cases_collection(client);
    collection.insert_one(case).await?;
    Ok(())
}

pub async fn get_case(
    client: &MongoClient,
    guild_id: i64,
    case_number: i64,
) -> mongodb::error::Result<Option<ModCase>> {
    let collection = mod_cases_collection(client);
    let filter = doc! { "guild_id": guild_id, "case_number": case_number };
    collection.find_one(filter).await
}

pub async fn get_cases_for_user(
    client: &MongoClient,
    guild_id: i64,
    target_id: i64,
) -> mongodb::error::Result<Vec<ModCase>> {
    let collection = mod_cases_collection(client);
    let filter = doc! { "guild_id": guild_id, "target_id": target_id };
    let cursor = collection.find(filter).await?;
    let cases = cursor.try_collect::<Vec<ModCase>>().await?;
    Ok(cases)
}

/// Most recent cases for a guild (highest case number first), capped at `limit`.
/// Backs the `modlog` command.
pub async fn recent_cases(
    client: &MongoClient,
    guild_id: i64,
    limit: i64,
) -> mongodb::error::Result<Vec<ModCase>> {
    let collection = mod_cases_collection(client);
    let filter = doc! { "guild_id": guild_id };
    let cursor = collection
        .find(filter)
        .sort(doc! { "case_number": -1 })
        .limit(limit)
        .await?;
    let cases = cursor.try_collect::<Vec<ModCase>>().await?;
    Ok(cases)
}

/// Flip a case's `active` flag (used by the expiry task to mark a lifted
/// mute / temp-ban inactive). No-op if the case does not exist.
pub async fn set_case_active(
    client: &MongoClient,
    guild_id: i64,
    case_number: i64,
    active: bool,
) -> mongodb::error::Result<()> {
    let collection = mod_cases_collection(client);
    let filter = doc! { "guild_id": guild_id, "case_number": case_number };
    collection
        .update_one(filter, doc! { "$set": { "active": active } })
        .await?;
    Ok(())
}
