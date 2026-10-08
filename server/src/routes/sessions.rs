//! 会话：增删查、投递消息、SSE 事件流、任务列表。

use super::MAX_TEXT_CHARS;
use crate::core::auth::AuthUser;
use crate::core::db::SessionView;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use agent::{AgentEvent, TaskInfo};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use model::Message;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use symbol::IngestRequest;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

#[derive(Deserialize)]
pub(super) struct CreateSessionRequest {
    #[serde(default)]
    title: Option<String>,
}

pub(super) async fn list_sessions(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Json<Vec<SessionView>>, ApiError> {
    Ok(Json(state.db().list_sessions(&user.sub).await?))
}

pub(super) async fn create_session(
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
pub(super) struct SessionDetail {
    #[serde(flatten)]
    session: SessionView,
    history: Vec<Message>,
}

pub(super) async fn get_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    let (session, history) = state.db().get_session(&user.sub, &id).await?;
    Ok(Json(SessionDetail { session, history }))
}

pub(super) async fn delete_session(
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
pub(super) struct SendMessageRequest {
    content: String,
}

pub(super) async fn send_message(
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
pub(super) async fn session_events(
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

pub(super) async fn session_tasks(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<TaskInfo>>, ApiError> {
    Ok(Json(state.session(&user.sub, &id).await?.tasks()))
}

