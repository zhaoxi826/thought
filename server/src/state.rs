//! 运行时共享状态。

use crate::auth::Jwt;
use crate::config::{MemorySection, ServerConfig};
use crate::db::Db;
use crate::error::ApiError;
use agent::{AgentSystem, HistorySink, SessionHandle, SessionOptions};
use model::{Message, ModelConfig};
use plugin::Registry;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use symbol::MemoryStore;
use tokio::sync::Mutex;

/// axum 共享状态。`Clone` 廉价。
#[derive(Clone)]
pub struct AppState {
    pub(crate) inner: Arc<Inner>,
    pub(crate) jwt: Arc<Jwt>,
}

pub(crate) struct Inner {
    pub db: Db,
    pub agents: AgentSystem,
    /// 记忆库根（按用户 scoped 前的那个）。
    pub memory: Option<MemoryStore>,
    pub memory_cfg: Option<MemorySection>,
    /// 模型配置（只读展示用，密钥不会被序列化）。
    pub models: BTreeMap<String, ModelConfig>,
    /// 静态前端目录（SPA 兜底服务）。
    pub web_dir: String,
    /// 活跃会话：session id → 句柄。
    pub sessions: Mutex<HashMap<String, LiveSession>>,
}

pub(crate) struct LiveSession {
    pub owner: String,
    pub handle: SessionHandle,
}

/// 把主控历史写回 `chat_session` 表。
struct DbSink(Db);

#[async_trait::async_trait]
impl HistorySink for DbSink {
    async fn save(&self, session_id: &str, history: &[Message]) {
        // system 每次按配置重建，不必持久化
        let body: Vec<Message> = history
            .iter()
            .filter(|m| !matches!(m, Message::System { .. }))
            .cloned()
            .collect();
        if let Err(e) = self.0.save_history(session_id, &body).await {
            tracing::warn!(session = session_id, "save history failed: {}", e.message);
        }
    }
}

impl AppState {
    pub fn new(
        config: &ServerConfig,
        db: Db,
        agents: AgentSystem,
        memory: Option<MemoryStore>,
        jwt: Jwt,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                agents,
                memory,
                memory_cfg: config.memory.clone(),
                models: config.models.clone(),
                web_dir: config.http.web_dir.clone(),
                sessions: Mutex::new(HashMap::new()),
            }),
            jwt: Arc::new(jwt),
        }
    }

    pub(crate) fn db(&self) -> &Db {
        &self.inner.db
    }

    /// 用户的记忆空间（`per_user` 时每人一个 database）。
    pub(crate) async fn memory_for(&self, user_id: &str) -> Result<Option<MemoryStore>, ApiError> {
        let (Some(root), Some(cfg)) = (&self.inner.memory, &self.inner.memory_cfg) else {
            return Ok(None);
        };
        if cfg.per_user {
            Ok(Some(root.scoped(&format!("user_{user_id}")).await?))
        } else {
            Ok(Some(root.clone()))
        }
    }

    /// 取（或从持久化恢复）会话的运行句柄，校验归属。
    pub(crate) async fn session(&self, owner: &str, id: &str) -> Result<SessionHandle, ApiError> {
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(live) = sessions.get(id) {
            if live.owner != owner {
                return Err(ApiError::not_found("session not found"));
            }
            return Ok(live.handle.clone());
        }

        let (_, history) = self.db().get_session(owner, id).await?;
        let mut tools = Registry::new();
        if let Some(memory) = self.memory_for(owner).await? {
            symbol::register_memory_tools(&mut tools, memory, &format!("chat:{id}"));
        }
        let handle = self.inner.agents.start_session(
            id,
            SessionOptions {
                history,
                tools,
                sink: Some(Arc::new(DbSink(self.db().clone()))),
            },
        );
        sessions.insert(
            id.to_owned(),
            LiveSession {
                owner: owner.to_owned(),
                handle: handle.clone(),
            },
        );
        Ok(handle)
    }

    /// 关闭会话：丢弃句柄，主控循环随之结束、运行中的任务被取消。
    pub(crate) async fn close_session(&self, id: &str) {
        self.inner.sessions.lock().await.remove(id);
    }
}
