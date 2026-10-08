//! HTTP 路由。
//!
//! 公开：`GET /health`、`POST /api/auth/login`。其余全部需要 `Authorization: Bearer <jwt>`：
//!
//! | 方法 | 路径 | 说明 |
//! | --- | --- | --- |
//! | GET | /api/me | 当前用户 |
//! | GET/POST | /api/users | 列出 / 创建用户（admin） |
//! | GET | /api/models | 模型配置（admin，不含密钥） |
//! | GET | /api/agents | 主控与 worker 定义 |
//! | GET/POST | /api/sessions | 列出 / 新建会话 |
//! | GET/DELETE | /api/sessions/{id} | 会话详情（含历史） / 删除 |
//! | POST | /api/sessions/{id}/messages | 发送消息（202，结果走事件流） |
//! | GET | /api/sessions/{id}/events | SSE 事件流 |
//! | GET | /api/sessions/{id}/tasks | 运行中的任务 |
//! | GET/POST | /api/memory | 最近记忆 / 写入记忆 |
//! | GET | /api/memory/search?q=&limit= | 检索记忆 |
//! | DELETE | /api/memory/{id} | 删除记忆 |

use crate::auth::{AdminUser, AuthUser, Role, verify_password};
use crate::db::{SessionView, UserView};
use crate::error::ApiError;
use crate::state::AppState;
use agent::{AgentEvent, TaskInfo};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use futures::Stream;
use model::Message;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use symbol::{IngestRequest, IngestResult, Memory, MemoryHit, MemoryStore};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tower_http::services::{ServeDir, ServeFile};

/// 单条消息 / 记忆的最大长度（字符）。
const MAX_TEXT_CHARS: usize = 64 * 1024;

pub fn router(state: AppState) -> Router {
    // 静态前端：SPA 兜底到 index.html；未匹配的 /api/* 仍返回 JSON 404。
    let web_dir = state.inner.web_dir.clone();
    if !std::path::Path::new(&web_dir).is_dir() {
        tracing::warn!(web_dir, "web dir does not exist; static frontend will 404");
    }
    let spa =
        ServeDir::new(&web_dir).not_found_service(ServeFile::new(format!("{web_dir}/index.html")));

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/auth/login", post(login))
        .route("/api/me", get(me))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/models", get(list_models))
        .route("/api/agents", get(list_agents))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route("/api/sessions/{id}/messages", post(send_message))
        .route("/api/sessions/{id}/events", get(session_events))
        .route("/api/sessions/{id}/tasks", get(session_tasks))
        .route("/api/memory", get(recent_memory).post(save_memory))
        .route("/api/memory/search", get(search_memory))
        .route("/api/memory/{id}", delete(delete_memory))
        .route("/api", any(api_not_found))
        .route("/api/{*_}", any(api_not_found))
        .fallback_service(spa)
        .with_state(state)
}

/// 未匹配的 /api/* 返回与业务一致的 JSON 404，而不是 SPA 的 index.html。
async fn api_not_found() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })))
}

// ---- 认证 ----

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    expires_at: u64,
    user: UserView,
}

async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let user = state.db().find_user(&req.username).await?;
    // 用户不存在时也做一次哈希校验，避免通过响应时间探测用户名
    const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$7QjKqRZ6yEJfTOCLUL7t1cD3qNhEOHVGDl1b1mbVFkM";
    let ok = verify_password(
        &req.password,
        user.as_ref().map_or(DUMMY, |u| &u.password_hash),
    );
    let user = match user {
        Some(user) if ok => user,
        _ => return Err(ApiError::unauthorized("invalid username or password")),
    };
    let (token, expires_at) = state.jwt.issue(&user.id, &user.username, user.role)?;
    Ok(Json(LoginResponse {
        token,
        expires_at,
        user: user.into(),
    }))
}

async fn me(AuthUser(claims): AuthUser) -> Json<Value> {
    Json(json!({ "id": claims.sub, "username": claims.name, "role": claims.role }))
}

// ---- 用户（admin） ----

async fn list_users(
    State(state): State<AppState>,
    _: AdminUser,
) -> Result<Json<Vec<UserView>>, ApiError> {
    Ok(Json(state.db().list_users().await?))
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
    #[serde(default = "default_role")]
    role: Role,
}

fn default_role() -> Role {
    Role::User
}

async fn create_user(
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

// ---- 模型 / agent 配置 ----

async fn list_models(State(state): State<AppState>, _: AdminUser) -> Json<Value> {
    // ModelConfig 序列化时跳过 api_key
    Json(json!(state.inner.models))
}

async fn list_agents(State(state): State<AppState>, _: AuthUser) -> Json<Value> {
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

// ---- 会话 ----

#[derive(Deserialize)]
struct CreateSessionRequest {
    #[serde(default)]
    title: Option<String>,
}

async fn list_sessions(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    Ok(Json(state.db().list_sessions(&user.sub).await?))
}

async fn create_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    body: Option<Json<CreateSessionRequest>>,
) -> Result<(StatusCode, Json<SessionView>), ApiError> {
    let title = body
        .and_then(|Json(b)| b.title)
        .map(|t| t.chars().take(200).collect::<String>())
        .unwrap_or_else(|| "New chat".to_owned());
    let session = state.db().create_session(&user.sub, &title).await?;
    Ok((StatusCode::CREATED, Json(session)))
}

#[derive(Serialize)]
struct SessionDetail {
    #[serde(flatten)]
    session: SessionView,
    history: Vec<Message>,
}

async fn get_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    let (session, history) = state.db().get_session(&user.sub, &id).await?;
    Ok(Json(SessionDetail { session, history }))
}

async fn delete_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !state.db().delete_session(&user.sub, &id).await? {
        return Err(ApiError::not_found("session not found"));
    }
    state.close_session(&id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SendMessageRequest {
    content: String,
}

async fn send_message(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<StatusCode, ApiError> {
    let content = req.content.trim();
    if content.is_empty() {
        return Err(ApiError::bad_request("empty message"));
    }
    if content.chars().count() > MAX_TEXT_CHARS {
        return Err(ApiError::bad_request("message too long"));
    }
    let handle = state.session(&user.sub, &id).await?;

    if let Some(cfg) = &state.inner.memory_cfg
        && cfg.save_user_messages
        && let Some(memory) = state.memory_for(&user.sub).await?
    {
        let request = IngestRequest::new(content).source(format!("chat:{id}"));
        if let Err(e) = memory.ingest(request).await {
            tracing::warn!(session = %id, "auto-save message failed: {e}");
        }
    }

    if !handle.send_user(content) {
        // 主控循环已退出（不应发生）：丢弃旧句柄，下次请求重建
        state.close_session(&id).await;
        return Err(ApiError::internal("session loop stopped"));
    }
    Ok(StatusCode::ACCEPTED)
}

/// SSE：订阅会话事件。慢消费者丢失的事件以 `lagged` 事件告知。
async fn session_events(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let handle = state.session(&user.sub, &id).await?;
    let stream = BroadcastStream::new(handle.subscribe()).map(|item| {
        let event = match item {
            Ok(event) => Event::default()
                .event(event_name(&event))
                .json_data(&event)
                .unwrap_or_else(|_| Event::default().event("error")),
            Err(BroadcastStreamRecvError::Lagged(n)) => {
                Event::default().event("lagged").data(n.to_string())
            }
        };
        Ok(event)
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::TextDelta { .. } => "text_delta",
        AgentEvent::ReasoningDelta { .. } => "reasoning_delta",
        AgentEvent::ToolCall { .. } => "tool_call",
        AgentEvent::ToolResult { .. } => "tool_result",
        AgentEvent::Usage { .. } => "usage",
        AgentEvent::TaskStarted { .. } => "task_started",
        AgentEvent::TaskFinished { .. } => "task_finished",
        AgentEvent::Reply { .. } => "reply",
        AgentEvent::Error { .. } => "error",
    }
}

async fn session_tasks(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<TaskInfo>>, ApiError> {
    Ok(Json(state.session(&user.sub, &id).await?.tasks()))
}

// ---- 记忆 ----

async fn memory(state: &AppState, user_id: &str) -> Result<MemoryStore, ApiError> {
    state
        .memory_for(user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("memory is not enabled"))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    10
}

async fn search_memory(
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
struct RecentQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

async fn recent_memory(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<RecentQuery>,
) -> Result<Json<Vec<Memory>>, ApiError> {
    let store = memory(&state, &user.sub).await?;
    Ok(Json(store.recent(query.limit.clamp(1, 200)).await?))
}

#[derive(Deserialize)]
struct SaveMemoryRequest {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    external_id: Option<String>,
}

async fn save_memory(
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

async fn delete_memory(
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
