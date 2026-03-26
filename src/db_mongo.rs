use serenity::futures::TryStreamExt;
use mongodb::{
    bson::{doc, Document},
    options::ReturnDocument,
    Client as MongoClient,
};
use serde::{Deserialize, Serialize};

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

    let count = doc
        .and_then(|d| d.get_i64("case_count").ok())
        .unwrap_or(1);
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
