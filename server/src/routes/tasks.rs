//! 任务注册表：全局任务列表、详情、取消、全局事件流。

use crate::core::auth::AuthUser;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use crate::core::tasks::{TaskEvent, TaskStatus, TaskView};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

#[derive(Deserialize)]
pub(super) struct ListQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ListResponse {
    tasks: Vec<TaskView>,
    next_cursor: Option<String>,
}

pub(super) async fn list_tasks(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<ListResponse>, ApiError> {
    let mut statuses = Vec::new();
    for s in query.status.iter().flat_map(|s| s.split(',')).map(str::trim).filter(|s| !s.is_empty()) {
        statuses.push(
            serde_json::from_value::<TaskStatus>(serde_json::json!(s))
                .map_err(|_| ApiError::bad_request(format!("invalid status {s:?}")))?,
        );
    }
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let (tasks, next_cursor) = state
        .tasks()
        .list(&user.sub, &statuses, limit, query.cursor.as_deref())
        .await?;
    Ok(Json(ListResponse { tasks, next_cursor }))
}

#[derive(Serialize)]
pub(super) struct DetailResponse {
    #[serde(flatten)]
    task: TaskView,
    events: Vec<TaskEvent>,
}

pub(super) async fn get_task(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((session_id, task_id)): Path<(String, u64)>,
) -> Result<Json<DetailResponse>, ApiError> {
    let (task, events) = state.tasks().detail(&user.sub, &session_id, task_id).await?;
    Ok(Json(DetailResponse { task, events }))
}

pub(super) async fn cancel_task(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((session_id, task_id)): Path<(String, u64)>,
) -> Result<StatusCode, ApiError> {
    if state.tasks().status(&user.sub, &session_id, task_id).await? != TaskStatus::Running {
        return Err(ApiError::conflict("task already finished"));
    }
    // 运行中的任务一定属于一个活跃会话；不唤醒会话
    let cancelled = match state.live_session(&user.sub, &session_id).await {
        Some(handle) => handle.cancel_task(task_id),
        None => false,
    };
    if cancelled {
        Ok(StatusCode::NO_CONTENT)
    } else {
        // 竞态：刚好已结束
        Err(ApiError::conflict("task already finished"))
    }
}

/// 全局任务事件流：只推送属于当前用户的任务事件，data 带 `session_id`。
pub(super) async fn task_events(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let owner = user.sub;
    let stream = BroadcastStream::new(state.tasks().subscribe()).filter_map(move |item| match item {
        Ok((event_owner, event)) if event_owner == owner => Some(Ok(Event::default()
            .event(event.name)
            .json_data(&event.data)
            .unwrap_or_else(|_| Event::default().event("error")))),
        Ok(_) => None,
        Err(BroadcastStreamRecvError::Lagged(n)) => Some(Ok(Event::default().event("lagged").data(n.to_string()))),
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
