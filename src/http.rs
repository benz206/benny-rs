use crate::state::AppState;
use axum::{Json, Router, extract::State, routing::get};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/ping", get(ping))
        .route("/health", get(health))
        .route("/stats", get(stats))
        .with_state(state)
}

pub async fn serve(router: Router, addr: SocketAddr) {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(_) => return,
    };
    let _ = axum::serve(listener, router).await;
}

async fn root() -> Json<serde_json::Value> {
    Json(json!({ "alive": true, "service": "benny-rs" }))
}

async fn ping(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let lat = state.latency();
    let snapshot = lat.lock().clone();
    Json(json!({ "latency_ms": snapshot }))
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let db_ok = sqlx::query("SELECT 1")
        .execute(state.servers_db())
        .await
        .is_ok();
    let mongo_ok = state.mongo.is_some();
    let redis_ok = state.redis.is_some();
    Json(json!({
        "ok": db_ok,
        "db": if db_ok { "ok" } else { "error" },
        "mongo": if mongo_ok { "connected" } else { "unavailable" },
        "redis": if redis_ok { "connected" } else { "unavailable" },
        "uptime_secs": state.uptime_secs()
    }))
}

async fn stats(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.uptime_secs(),
    }))
}
