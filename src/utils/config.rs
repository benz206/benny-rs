use crate::framework::{Context, Error, send_error, send_plain};
use dashmap::DashMap;
use sea_orm::{DatabaseConnection, DbErr, EntityTrait};
use std::future::Future;

/// Await a per-cog `update_config` future, replying with `ok_msg` on success
/// or logging (tagged with `label`) and replying with `err_msg` on failure.
/// Shared by the per-cog `apply_setting` wrappers that persist a single
/// config change.
pub async fn apply_setting<Fut, T>(
    ctx: Context<'_>,
    label: &str,
    ok_msg: String,
    err_msg: &str,
    op: Fut,
) -> Result<(), Error>
where
    Fut: Future<Output = Result<T, DbErr>>,
{
    match op.await {
        Ok(_) => send_plain(ctx, ok_msg).await,
        Err(e) => {
            tracing::error!(error = ?e, "failed to save {label} config");
            send_error(ctx, err_msg).await
        }
    }
}

/// Fetch all rows of `E` and insert each into `cache`, keyed by `key(&row)`
/// with value `val(row)`. Returns the number of rows hydrated. Used by each
/// cog's `on_ready` to rebuild its config cache from the DB at startup.
pub async fn hydrate_cache<E, V>(
    db: &DatabaseConnection,
    cache: &DashMap<u64, V>,
    key: impl Fn(&E::Model) -> u64,
    val: impl Fn(E::Model) -> V,
) -> usize
where
    E: EntityTrait,
{
    let rows = E::find().all(db).await.unwrap_or_default();
    let count = rows.len();
    for m in rows {
        cache.insert(key(&m), val(m));
    }
    count
}
