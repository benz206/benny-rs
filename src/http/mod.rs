//! HTTP server: public liveness/stats endpoints (always on) plus an optional,
//! authenticated dashboard API under `/api/v1`.
//!
//! The public routes (`/`, `/ping`, `/health`, `/stats`) are unauthenticated
//! and unchanged. The `/api/v1` tree is mounted ONLY when
//! `config.dashboard_api_token` is set — secure by default: with no token the
//! API simply does not exist (404). See `auth` for the security layer and `v1`
//! for the endpoints.

mod auth;
mod error;
mod v1;

use crate::state::AppState;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    routing::get,
};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

/// Max accepted request body (dashboard payloads are small JSON objects).
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Per-request timeout for the API tree.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub fn router(state: Arc<AppState>) -> Router {
    let mut app = Router::new()
        .route("/", get(root))
        .route("/ping", get(ping))
        .route("/health", get(health))
        .route("/stats", get(stats));

    // Mount the dashboard API only when a bearer token is configured.
    if state.config.dashboard_api_token.is_some() {
        tracing::info!("dashboard API enabled at /api/v1");
        let api = v1::router()
            // `route_layer` so unmatched /api/v1/* paths 404 without running auth.
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth::auth_middleware,
            ))
            .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                REQUEST_TIMEOUT,
            ))
            .layer(cors_layer(&state));
        app = app.nest("/api/v1", api);
    }

    app.with_state(state)
}

/// CORS for the API. The dashboard calls through a server-side BFF so the
/// browser never hits this directly; the allowlist is defense in depth. With no
/// configured origin, no cross-origin requests are permitted.
fn cors_layer(state: &AppState) -> CorsLayer {
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
    ];
    let headers = [
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
        HeaderName::from_static("x-actor-id"),
    ];
    match state
        .config
        .dashboard_allowed_origin
        .as_deref()
        .and_then(|o| o.parse::<HeaderValue>().ok())
    {
        Some(origin) => CorsLayer::new()
            .allow_origin(origin)
            .allow_methods(methods)
            .allow_headers(headers),
        None => CorsLayer::new(),
    }
}

pub async fn serve(router: Router, addr: SocketAddr) {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(_) => return,
    };
    let _ = axum::serve(listener, router).await;
}

// ---- public handlers (unauthenticated) -------------------------------------

async fn root() -> Json<serde_json::Value> {
    Json(json!({ "alive": true, "service": "benny-rs" }))
}

async fn ping(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let lat = state.latency();
    let snapshot = lat.lock().clone();
    Json(json!({ "latency_ms": snapshot }))
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let db_ok = state.servers_orm().ping().await.is_ok();
    let redis_ok = state.redis.is_some();
    Json(json!({
        "ok": db_ok,
        "db": if db_ok { "ok" } else { "error" },
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
