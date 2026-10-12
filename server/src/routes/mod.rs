//! HTTP 路由。
//!
//! 公开：`GET /health`、`POST /api/auth/login`。其余全部需要 `Authorization: Bearer <jwt>`：
//!
//! | 方法 | 路径 | 说明 |
//! | --- | --- | --- |
//! | GET | /api/me | 当前用户 |
//! | GET/POST | /api/users | 列出 / 创建用户（admin） |
//! | GET | /api/models | 模型配置（admin，不含密钥） |
//! | GET | /api/agents | 主控与 worker 定义（含 system、limits） |
//! | GET/PUT | /api/config | 读取（ETag，脱敏）/ 写回配置（admin，If-Match） |
//! | GET | /api/mind/budget | 今日用量（按来源）、软/硬上限、疲劳度、是否急停 |
//! | GET | /api/mind/budget/history?days= | 按日用量历史（admin） |
//! | POST | /api/mind/pause / resume | 急停 / 恢复自主活动（admin） |
//! | POST | /api/auth/elevate | 二次鉴权：重输密码换 10 分钟 inner 令牌（admin） |
//! | GET | /api/mind/status | 她的状态：在想吗、自我摘要、心情、用量、任务 |
//! | POST | /api/mind/say | 对她说话（进入意识流） |
//! | GET | /api/mind/conversation[/events] | 你和她的对话 / 实时（SSE，连接期间算在线） |
//! | GET | /api/mind/stream[/events] | 完整意识流（admin + X-Inner-Token；她会知道） |
//! | GET | /api/mind/journal · /self · /audit | 日记、自我与成长、审计（admin + X-Inner-Token） |
//! | GET | /api/backup | 备份状态与配置（admin，不含密钥） |
//! | GET | /api/backup/list | 对象存储里的备份（admin） |
//! | POST | /api/backup/run | 立即备份（admin；恢复走命令行 thought restore） |
//! | GET | /api/tasks | 全局任务列表（含历史，游标分页） |
//! | GET | /api/tasks/events | 全局任务事件流（SSE） |
//! | GET | /api/tasks/{sid}/{tid} | 任务详情（含活动事件） |
//! | POST | /api/tasks/{sid}/{tid}/cancel | 取消任务 |
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
mod backup;
mod memory;
mod mind;
mod sessions;
mod tasks;
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
        .route("/api/auth/elevate", post(auth::elevate))
        .route("/api/me", get(auth::me))
        .route("/api/users", get(admin::list_users).post(admin::create_user))
        .route("/api/models", get(admin::list_models))
        .route("/api/agents", get(admin::list_agents))
        .route("/api/config", get(admin::get_config).put(admin::put_config))
        .route("/api/backup", get(backup::status))
        .route("/api/backup/list", get(backup::list))
        .route("/api/backup/run", post(backup::run))
        .route("/api/mind/budget", get(mind::budget))
        .route("/api/mind/budget/history", get(mind::budget_history))
        .route("/api/mind/pause", post(mind::pause))
        .route("/api/mind/resume", post(mind::resume))
        .route("/api/mind/status", get(mind::status))
        .route("/api/mind/say", post(mind::say))
        .route("/api/mind/conversation", get(mind::conversation))
        .route("/api/mind/conversation/events", get(mind::conversation_events))
        .route("/api/mind/stream", get(mind::stream))
        .route("/api/mind/stream/events", get(mind::stream_events))
        .route("/api/mind/journal", get(mind::journal))
        .route("/api/mind/self", get(mind::self_history))
        .route("/api/mind/audit", get(mind::audit))
        .route("/api/tasks", get(tasks::list_tasks))
        .route("/api/tasks/events", get(tasks::task_events))
        .route("/api/tasks/{session_id}/{task_id}", get(tasks::get_task))
        .route("/api/tasks/{session_id}/{task_id}/cancel", post(tasks::cancel_task))
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
