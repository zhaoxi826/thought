//! 用户管理与配置查看。

use crate::core::auth::{AdminUser, AuthUser, Role};
use crate::core::db::UserView;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use crate::core::config_store;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
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
    let config = state.inner.config.read().expect("config lock poisoned");
    Json(json!(config.models))
}

pub(super) async fn list_agents(State(state): State<AppState>, _: AuthUser) -> Json<Value> {
    let agents = state.agents();
    let config = agents.config();
    Json(json!({
        "controller": { "model": config.controller.model },
        "workers": config.workers.iter().map(|w| json!({
            "name": w.name,
            "description": w.description,
            "model": w.model,
            "system": w.system,
            "tools": w.tools,
            "can_spawn": w.can_spawn,
        })).collect::<Vec<_>>(),
        "limits": config.limits,
    }))
}

#[derive(Deserialize)]
pub(super) struct ConfigQuery {
    #[serde(default)]
    format: Option<String>,
}

pub(super) async fn get_config(
    State(state): State<AppState>,
    _: AdminUser,
    Query(query): Query<ConfigQuery>,
) -> Result<Response, ApiError> {
    if query.format.as_deref() == Some("toml") {
        let text = config_store::read_toml(&state)?;
        return Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response());
    }
    let (etag, body) = config_store::read(&state)?;
    Ok(([(header::ETAG, etag)], Json(body)).into_response())
}

pub(super) async fn put_config(
    State(state): State<AppState>,
    _: AdminUser,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<config_store::ApplyResult>, ApiError> {
    let if_match = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok());
    Ok(Json(config_store::apply(&state, if_match, body)?))
}
