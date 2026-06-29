//! Authentication + authorization for `/api/v1`.
//!
//! `auth_middleware` runs on every matched API route and enforces, in order:
//! a constant-time bearer-token check, a required `X-Actor-Id` header, a
//! per-actor in-memory rate limit, and a blacklist lookup. It stashes the
//! validated actor id in request extensions; the [`Actor`] extractor reads it
//! back out, and [`GuildScope`] validates the `{gid}` path segment against the
//! bot's live guild membership.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use axum::{
    extract::{FromRequestParts, Path, Request, State},
    http::{HeaderMap, header::AUTHORIZATION, request::Parts},
    middleware::Next,
    response::Response,
};
use dashmap::DashMap;
use sea_orm::EntityTrait;
use subtle::ConstantTimeEq;

use super::error::ApiError;
use crate::entities::settings_users;
use crate::state::AppState;

/// The validated acting Discord user id (`X-Actor-Id`), inserted into request
/// extensions by [`auth_middleware`].
#[derive(Clone, Copy, Debug)]
pub struct Actor(pub u64);

impl<S: Send + Sync> FromRequestParts<S> for Actor {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Actor>()
            .copied()
            .ok_or(ApiError::Unauthorized)
    }
}

/// A `{gid}` path segment confirmed to be a guild the bot is currently in.
/// Rejects with 404 when the segment is missing/unparseable or the bot is not
/// a member — so the API never reveals config for guilds it doesn't manage.
#[derive(Clone, Copy, Debug)]
pub struct GuildScope(pub u64);

impl FromRequestParts<Arc<AppState>> for GuildScope {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::NotFound)?;
        let gid = params
            .get("gid")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or(ApiError::NotFound)?;
        if !state.in_guild(gid) {
            return Err(ApiError::NotFound);
        }
        Ok(GuildScope(gid))
    }
}

// ---- rate limiting ---------------------------------------------------------

/// Fixed-window per-actor limiter. The actor set is small (dashboard service
/// users), so unbounded growth is not a concern in practice.
struct RateLimiter {
    window: Duration,
    max: u32,
    hits: DashMap<u64, (Instant, u32)>,
}

impl RateLimiter {
    /// Record a hit for `key`; returns false once the window's budget is spent.
    fn allow(&self, key: u64) -> bool {
        let mut e = self.hits.entry(key).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= self.window {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        e.1 <= self.max
    }
}

/// 100 requests per 10s per actor.
static RATE_LIMITER: LazyLock<RateLimiter> = LazyLock::new(|| RateLimiter {
    window: Duration::from_secs(10),
    max: 100,
    hits: DashMap::new(),
});

// ---- middleware ------------------------------------------------------------

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    // The token is guaranteed Some here (the API is only mounted when it is),
    // but guard anyway so a misconfiguration fails closed.
    let expected = state
        .config
        .dashboard_api_token
        .as_deref()
        .ok_or(ApiError::NotFound)?;

    // 1. Bearer token, compared in constant time.
    let provided = bearer(req.headers()).ok_or(ApiError::Unauthorized)?;
    if !ct_eq(provided, expected) {
        return Err(ApiError::Unauthorized);
    }

    // 2. Acting Discord user id (required for blacklist + audit logging).
    let actor = req
        .headers()
        .get("x-actor-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .ok_or_else(|| ApiError::bad_request("missing or invalid X-Actor-Id header"))?;

    // 3. Cheap per-actor rate limit, before any DB work.
    if !RATE_LIMITER.allow(actor) {
        return Err(ApiError::TooManyRequests);
    }

    // 4. Blacklisted actors are refused outright.
    if is_blacklisted(&state, actor).await {
        return Err(ApiError::Forbidden);
    }

    req.extensions_mut().insert(Actor(actor));
    Ok(next.run(req).await)
}

/// Extract the `Bearer <token>` value from the `Authorization` header.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Constant-time string equality (length still leaks, which is acceptable for a
/// fixed-length token).
fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

async fn is_blacklisted(state: &AppState, user_id: u64) -> bool {
    settings_users::Entity::find_by_id(user_id as i64)
        .one(state.users_orm())
        .await
        .ok()
        .flatten()
        .map(|m| m.is_blacklisted)
        .unwrap_or(false)
}
