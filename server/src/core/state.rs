//! 运行时共享状态。

use crate::config::{MemorySection, ServerConfig};
use crate::core::auth::Jwt;
use crate::core::db::Db;
use crate::core::error::ApiError;
use crate::core::budget::Budget;
use crate::core::ratelimit::RateLimiter;
use crate::core::tasks::TaskRegistry;
use agent::{AgentSystem, HistorySink, SessionHandle, SessionOptions};
use model::Message;
use plugin::Registry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
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
    pub tasks: TaskRegistry,
    pub budget: Budget,
    /// 登录 / 二次鉴权失败限流。
    pub login_limiter: RateLimiter,
    /// agent 系统（可热替换：配置写回后新会话使用新系统，进行中的会话不受影响）。
    pub agents: RwLock<AgentSystem>,
    /// 构造 agent 系统用的真实工具池（热替换时复用）。
    pub tools: Registry,
    /// 当前生效的配置（含明文密钥，只在服务端内部使用）。
    pub config: RwLock<ServerConfig>,
    /// 配置文件路径；`None` 表示不是从文件启动，配置接口不可用。
    pub config_path: Option<PathBuf>,
    /// 记忆库根（按用户 scoped 前的那个）。
    pub memory: Option<MemoryStore>,
    pub memory_cfg: RwLock<Option<MemorySection>>,
    /// 静态前端目录（SPA 兜底服务）。
    pub web_dir: PathBuf,
    /// 心智（启用时）。
    pub mind: Option<crate::mind::MindHandle>,
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: &ServerConfig,
        config_path: Option<PathBuf>,
        db: Db,
        tasks: TaskRegistry,
        budget: Budget,
        agents: AgentSystem,
        tools: Registry,
        memory: Option<MemoryStore>,
        mind: Option<crate::mind::MindHandle>,
        jwt: Jwt,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                tasks,
                budget,
                login_limiter: RateLimiter::new(5, std::time::Duration::from_secs(15 * 60)),
                agents: RwLock::new(agents),
                tools,
                config: RwLock::new(config.clone()),
                config_path,
                memory,
                memory_cfg: RwLock::new(config.memory.clone()),
                web_dir: config.http.web_dir.clone(),
                mind,
                sessions: Mutex::new(HashMap::new()),
            }),
            jwt: Arc::new(jwt),
        }
    }

    /// 当前生效配置的快照（含明文密钥，勿直接输出给客户端）。
    pub fn config(&self) -> ServerConfig {
        self.inner.config.read().expect("config lock poisoned").clone()
    }

    /// 当前 agent 系统的快照。
    pub(crate) fn agents(&self) -> AgentSystem {
        self.inner.agents.read().expect("agents lock poisoned").clone()
    }

    pub(crate) fn memory_cfg(&self) -> Option<MemorySection> {
        self.inner.memory_cfg.read().expect("memory cfg lock poisoned").clone()
    }

    pub(crate) fn tasks(&self) -> &TaskRegistry {
        &self.inner.tasks
    }

    pub(crate) fn budget(&self) -> &Budget {
        &self.inner.budget
    }

    pub(crate) fn mind(&self) -> Result<&crate::mind::MindHandle, ApiError> {
        self.inner.mind.as_ref().ok_or_else(|| ApiError::not_found("mind is not enabled"))
    }

    pub(crate) fn db(&self) -> &Db {
        &self.inner.db
    }

    /// 用户的记忆空间（`per_user` 时每人一个 database）。
    pub(crate) async fn memory_for(&self, user_id: &str) -> Result<Option<MemoryStore>, ApiError> {
        let (Some(root), Some(cfg)) = (&self.inner.memory, self.memory_cfg()) else {
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
        let registry = self.tasks();
        registry.register_session(id, owner);
        let first_task_id = registry.max_task_id(id).await? + 1;
        let handle = self.agents().start_session(
            id,
            SessionOptions {
                history,
                tools,
                sink: Some(Arc::new(DbSink(self.db().clone()))),
                observer: Some(Arc::new(agent::Observers(vec![
                    Arc::new(registry.clone()),
                    Arc::new(self.budget().clone()),
                ]))),
                first_task_id,
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

    /// 已在运行的会话句柄（不会唤醒 / 恢复会话）。
    pub(crate) async fn live_session(&self, owner: &str, id: &str) -> Option<SessionHandle> {
        let sessions = self.inner.sessions.lock().await;
        sessions.get(id).filter(|l| l.owner == owner).map(|l| l.handle.clone())
    }

    /// 关闭会话：丢弃句柄，主控循环随之结束、运行中的任务被取消。
    pub(crate) async fn close_session(&self, id: &str) {
        self.inner.sessions.lock().await.remove(id);
    }
}
