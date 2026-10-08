use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// HTTP 层错误：状态码 + 对外消息。内部细节只进日志，不返回给客户端。
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// 内部错误：详情写日志，对外只说 internal error。
    pub fn internal(detail: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {detail}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<surrealdb::Error> for ApiError {
    fn from(e: surrealdb::Error) -> Self {
        ApiError::internal(e)
    }
}

impl From<symbol::MemoryError> for ApiError {
    fn from(e: symbol::MemoryError) -> Self {
        match e {
            symbol::MemoryError::Invalid(msg) => ApiError::bad_request(msg),
            other => ApiError::internal(other),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::internal(e)
    }
}
