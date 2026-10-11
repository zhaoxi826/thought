//! 心智相关接口：和她说话、对话流、内心透明（二次鉴权）、代谢与急停。

use crate::core::auth::{AdminUser, AuthUser};
use crate::core::budget::BudgetStatus;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use mind::{Moment, Percept, person};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use super::MAX_TEXT_CHARS;

// ---- 代谢与急停 ----

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

// ---- 和她说话 ----

#[derive(Deserialize)]
pub(super) struct SayRequest {
    text: String,
}

/// 对她说话：进入意识流，返回该时刻（202，她的回应走 `/api/mind/conversation/events`）。
pub(super) async fn say(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(req): Json<SayRequest>,
) -> Result<(StatusCode, Json<Moment>), ApiError> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("empty message"));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(ApiError::bad_request("message too long"));
    }
    let handle = state.mind()?;
    let moment = handle
        .mind
        .perceive(Percept::Heard { from: person(&user.sub, &user.name), text: text.to_owned() })
        .await
        .map_err(ApiError::internal)?;
    // 回显给本人的其他标签页
    handle.web.push(&user.sub, &moment);
    Ok((StatusCode::ACCEPTED, Json(moment)))
}

#[derive(Deserialize)]
pub(super) struct ConversationQuery {
    #[serde(default)]
    after: u64,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

#[derive(Serialize)]
pub(super) struct ConversationView {
    moments: Vec<Moment>,
    /// 首次对话时的告知：对话对管理员可见。
    notice: &'static str,
}

/// 当前用户与她的对话：他说的 + 对他说的（含他不在时她说的）。
pub(super) async fn conversation(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(q): Query<ConversationQuery>,
) -> Result<Json<ConversationView>, ApiError> {
    let handle = state.mind()?;
    let moments = handle.db.conversation(&user.sub, q.after, q.limit.clamp(1, 500)).await?;
    Ok(Json(ConversationView { moments, notice: "你和思想的对话对管理员可见" }))
}

/// 对话事件流：她对你说的话、你在其他标签页说的话；连接期间你算"在线"。
pub(super) async fn conversation_events(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let handle = state.mind()?;
    let (presence, rx) = handle.web.connect(&user.sub);
    let thinking = handle.mind.subscribe();
    let me = user.sub.clone();
    let deliveries = BroadcastStream::new(rx).filter_map(move |item| match item {
        Ok(d) if d.to == me => Some(Ok(Event::default().event("moment").json_data(&d.moment).unwrap_or_default())),
        Ok(_) => None,
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            Some(Ok(Event::default().event("lagged").data(n.to_string())))
        }
    });
    // "她在想"的状态对所有人可见（像看到对方正在输入），内容不可见
    let typing = BroadcastStream::new(thinking).filter_map(|item| match item {
        Ok(mind::MindEvent::Thinking { active }) => {
            Some(Ok(Event::default().event("thinking").data(if active { "true" } else { "false" })))
        }
        _ => None,
    });
    let stream = deliveries.merge(typing).map(move |e| {
        let _keep = &presence;
        e
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// ---- 状态（所有人） ----

pub(super) async fn status(State(state): State<AppState>, _: AuthUser) -> Result<Json<Value>, ApiError> {
    let handle = state.mind()?;
    let model = handle.mind.self_model().await.map_err(ApiError::internal)?;
    Ok(Json(json!({
        "thinking": handle.mind.is_thinking(),
        "identity": model.identity,
        "mood": model.mood,
        "interests": model.interests.iter().map(|i| &i.topic).collect::<Vec<_>>(),
        "projects": model.projects.iter().filter(|p| p.status == "active").map(|p| &p.title).collect::<Vec<_>>(),
        "budget": state.budget().status(),
        "tasks": handle.workforce.tasks(),
    })))
}

// ---- 内心透明（admin + 二次鉴权） ----

/// 校验 `X-Inner-Token`，并在审计里、她的意识流里留下记录。
async fn inner_access(state: &AppState, headers: &HeaderMap, admin: &crate::core::auth::Claims, what: &str) -> Result<(), ApiError> {
    let token = headers
        .get("x-inner-token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("inner token required (POST /api/auth/elevate)"))?;
    state.jwt.verify_inner(token, &admin.sub)?;
    let handle = state.mind()?;
    handle
        .db
        .record_audit(json!({ "at_ms": mind::Clock::now_ms(&mind::SystemClock), "kind": "inner_view", "by": admin.sub, "what": what }))
        .await;
    // 透明是双向的：她会知道
    handle.mind.perceive_nowait(Percept::Observed { by: person(&admin.sub, &admin.name), range: what.to_owned() });
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct PageQuery {
    #[serde(default)]
    before: Option<u64>,
    #[serde(default = "default_limit")]
    limit: usize,
}

/// 完整意识流（含她和别人的对话、独处的念头、做过的事），倒序分页。
pub(super) async fn stream(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Vec<Moment>>, ApiError> {
    inner_access(&state, &headers, &admin, "意识流").await?;
    let before = q.before.unwrap_or(u64::MAX);
    Ok(Json(state.mind()?.db.stream_page(before, q.limit.clamp(1, 500)).await?))
}

/// 内心实时：意识流上的每个新时刻、思考状态、回合失败。
pub(super) async fn stream_events(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    inner_access(&state, &headers, &admin, "实时意识流").await?;
    let rx = state.mind()?.mind.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(e) => Some(Ok(Event::default().event("mind").json_data(&e).unwrap_or_default())),
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
            Some(Ok(Event::default().event("lagged").data(n.to_string())))
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub(super) async fn journal(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Json<Vec<Value>>, ApiError> {
    inner_access(&state, &headers, &admin, "日记").await?;
    let before = q.before.map_or(i64::MAX, |b| b as i64);
    Ok(Json(state.mind()?.db.journal_page(before, q.limit.clamp(1, 200)).await?))
}

pub(super) async fn self_history(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    inner_access(&state, &headers, &admin, "自我模型与成长").await?;
    let handle = state.mind()?;
    let model = handle.mind.self_model().await.map_err(ApiError::internal)?;
    let history = mind::SelfStore::history(&*handle.db, 200).await.map_err(ApiError::internal)?;
    Ok(Json(json!({ "self_model": model, "history": history })))
}

pub(super) async fn audit(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, ApiError> {
    inner_access(&state, &headers, &admin, "审计").await?;
    Ok(Json(state.mind()?.db.audit_page(200).await?))
}
