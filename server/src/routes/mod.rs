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

mod admin;
mod auth;
mod memory;
mod sessions;
mod web;

use crate::core::state::AppState;
use axum::http::StatusCode;
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

/// 单条消息 / 记忆的最大长度（字符）。
const MAX_TEXT_CHARS: usize = 64 * 1024;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/auth/login", post(auth::login))
        .route("/api/me", get(auth::me))
        .route("/api/users", get(admin::list_users).post(admin::create_user))
        .route("/api/models", get(admin::list_models))
        .route("/api/agents", get(admin::list_agents))
        .route("/api/sessions", get(sessions::list_sessions).post(sessions::create_session))
        .route("/api/sessions/{id}", get(sessions::get_session).delete(sessions::delete_session))
        .route("/api/sessions/{id}/messages", post(sessions::send_message))
        .route("/api/sessions/{id}/events", get(sessions::session_events))
        .route("/api/sessions/{id}/tasks", get(sessions::session_tasks))
        .route("/api/memory", get(memory::recent_memory).post(memory::save_memory))
        .route("/api/memory/search", get(memory::search_memory))
        .route("/api/memory/{id}", delete(memory::delete_memory))
        .route("/api", any(api_not_found))
        .route("/api/{*_}", any(api_not_found));

    let web_dir = state.inner.web_dir.clone();
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(api)
        .with_state(state)
        .merge(web::router(&web_dir))
        .layer(axum::middleware::from_fn(web::security_headers))
}

/// 未匹配的 /api/* 返回与业务一致的 JSON 404，而不是 SPA 的 index.html。
async fn api_not_found() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })))
}
