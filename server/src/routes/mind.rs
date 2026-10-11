//! 心智相关接口（阶段 0：代谢与急停）。

use crate::core::auth::{AdminUser, AuthUser};
use crate::core::budget::BudgetStatus;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::Value;

pub(super) async fn budget(State(state): State<AppState>, _: AuthUser) -> Json<BudgetStatus> {
    Json(state.budget().status())
}

#[derive(Deserialize)]
pub(super) struct HistoryQuery {
    #[serde(default = "default_days")]
    days: usize,
}

fn default_days() -> usize {
    14
}

pub(super) async fn budget_history(
    State(state): State<AppState>,
    _: AdminUser,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<Value>>, ApiError> {
    Ok(Json(state.budget().history(q.days.clamp(1, 365)).await?))
}

/// 急停：她停止一切自主活动；和人的对话不受影响。持久化，重启后保持。
pub(super) async fn pause(State(state): State<AppState>, AdminUser(user): AdminUser) -> Result<StatusCode, ApiError> {
    state.budget().set_paused(true).await?;
    tracing::warn!(by = %user.name, "mind paused");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn resume(State(state): State<AppState>, AdminUser(user): AdminUser) -> Result<StatusCode, ApiError> {
    state.budget().set_paused(false).await?;
    tracing::info!(by = %user.name, "mind resumed");
    Ok(StatusCode::NO_CONTENT)
}
