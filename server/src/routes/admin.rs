//! 用户管理与配置查看。

use crate::core::auth::{AdminUser, AuthUser, Role};
use crate::core::db::UserView;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) async fn list_users(
    State(state): State<AppState>,
    _: AdminUser,
) -> Result<Json<Vec<UserView>>, ApiError> {
    Ok(Json(state.db().list_users().await?))
}

#[derive(Deserialize)]
pub(super) struct CreateUserRequest {
    username: String,
    password: String,
    #[serde(default = "default_role")]
    role: Role,
}

fn default_role() -> Role {
    Role::User
}

pub(super) async fn create_user(
    State(state): State<AppState>,
    _: AdminUser,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    let user = state
        .db()
        .create_user(&req.username, &req.password, req.role)
        .await?;
    Ok((StatusCode::CREATED, Json(user)))
}

pub(super) async fn list_models(State(state): State<AppState>, _: AdminUser) -> Json<Value> {
    // ModelConfig 序列化时跳过 api_key
    Json(json!(state.inner.models))
}

pub(super) async fn list_agents(State(state): State<AppState>, _: AuthUser) -> Json<Value> {
    let config = state.inner.agents.config();
    Json(json!({
        "controller": { "model": config.controller.model },
        "workers": config.workers.iter().map(|w| json!({
            "name": w.name,
            "description": w.description,
            "model": w.model,
            "tools": w.tools,
            "can_spawn": w.can_spawn,
        })).collect::<Vec<_>>(),
    }))
}

