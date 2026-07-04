//! Error type for the dashboard API. Every handler returns `ApiResult<T>` and
//! relies on `ApiError`'s `IntoResponse` to map to a status code + JSON body of
//! the shape `{ "error": "<message>" }`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    /// Missing or invalid bearer token.
    Unauthorized,
    /// Authenticated but not permitted (e.g. blacklisted actor).
    Forbidden,
    /// Resource (or guild the bot isn't in) not found.
    NotFound,
    /// Request was well-formed but conflicts with current state (e.g. tag
    /// already exists).
    Conflict(String),
    /// Malformed input.
    BadRequest(String),
    /// Per-actor rate limit exceeded.
    TooManyRequests,
    /// Unexpected server-side failure (DB error, etc.).
    Internal,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::TooManyRequests => {
                (StatusCode::TOO_MANY_REQUESTS, "rate limited".to_string())
            }
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error".to_string(),
            ),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

/// A SeaORM error in a handler is never the client's fault — surface it as a
/// 500 and keep the detail in the logs (logged at the call site).
impl From<sea_orm::DbErr> for ApiError {
    fn from(e: sea_orm::DbErr) -> Self {
        tracing::error!(error = ?e, "dashboard API database error");
        ApiError::Internal
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
