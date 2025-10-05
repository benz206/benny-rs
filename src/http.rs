use crate::state::AppState;
use axum::{extract::State, routing::get, Json, Router};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/ping", get(ping))
        .with_state(state)
}

pub async fn serve(router: Router, addr: SocketAddr) {
    axum::Server::bind(&addr)
        .serve(router.into_make_service())
        .await
        .ok();
}

async fn root() -> Json<serde_json::Value> {
    Json(json!({ "alive": true }))
}

async fn ping(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let lat = state.latency();
    let snapshot = lat.lock().clone();
    Json(json!({ "latency_ms": snapshot }))
}


