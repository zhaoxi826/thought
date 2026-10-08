//! 记忆：检索、最近、写入、删除（按用户隔离）。

use super::MAX_TEXT_CHARS;
use crate::core::auth::AuthUser;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use symbol::{IngestRequest, IngestResult, Memory, MemoryHit, MemoryStore};

async fn memory(state: &AppState, user_id: &str) -> Result<MemoryStore, ApiError> {
    state
        .memory_for(user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("memory is not enabled"))
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

pub(super) async fn search_memory(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<MemoryHit>>, ApiError> {
    let store = memory(&state, &user.sub).await?;
    Ok(Json(
        store.search(&query.q, query.limit.clamp(1, 50)).await?,
    ))
}

#[derive(Deserialize)]
pub(super) struct RecentQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

pub(super) async fn recent_memory(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<RecentQuery>,
) -> Result<Json<Vec<Memory>>, ApiError> {
    let store = memory(&state, &user.sub).await?;
    Ok(Json(store.recent(query.limit.clamp(1, 200)).await?))
}

#[derive(Deserialize)]
pub(super) struct SaveMemoryRequest {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    external_id: Option<String>,
}

pub(super) async fn save_memory(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<SaveMemoryRequest>,
) -> Result<(StatusCode, Json<IngestResult>), ApiError> {
    if req.content.chars().count() > MAX_TEXT_CHARS {
        return Err(ApiError::bad_request("content too long"));
    }
    let store = memory(&state, &user.sub).await?;
    let mut request = IngestRequest::new(req.content).source("api").tags(req.tags);
    request.external_id = req.external_id;
    let result = store.ingest(request).await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(result)))
}

pub(super) async fn delete_memory(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let store = memory(&state, &user.sub).await?;
    if store.delete(&id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("memory not found"))
    }
}
