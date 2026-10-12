//! 备份：状态、列表、立即备份（admin）。恢复走命令行 `thought restore`。

use crate::core::auth::AdminUser;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{json, Value};

pub(super) async fn status(State(state): State<AppState>, _: AdminUser) -> Json<Value> {
    let b = &state.inner.backup;
    Json(json!({ "status": b.status(), "config": crate::backup::public_config(&b.config()) }))
}

pub(super) async fn list(State(state): State<AppState>, _: AdminUser) -> Result<Json<Value>, ApiError> {
    let items = state.inner.backup.list().await.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(json!({ "backups": items })))
}

/// 立即备份。同步执行（通常几秒到几十秒），返回备份名。
pub(super) async fn run(State(state): State<AppState>, AdminUser(user): AdminUser) -> Result<Json<Value>, ApiError> {
    tracing::info!(by = %user.name, "manual backup");
    match state.inner.backup.run("manual").await {
        Ok(name) => Ok(Json(json!({ "name": name }))),
        Err(e) if e.contains("already running") => Err(ApiError::conflict(e)),
        Err(e) if e.contains("not configured") || e.contains("disabled") => Err(ApiError::bad_request(e)),
        Err(e) => Err(ApiError::new(StatusCode::BAD_GATEWAY, e)),
    }
}
