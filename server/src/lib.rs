//! 后端：配置模型、聊天（SSE 事件流）、记忆管理，JWT 认证。
//!
//! 组装入口是 [`build`]：按 [`ServerConfig`] 打开存储、构造模型池 / 工具池 /
//! agent 系统 / 记忆器，返回可直接 `axum::serve` 的 [`Router`](axum::Router)。

pub mod config;
pub mod core;
pub mod routes;

pub use config::ServerConfig;
pub use core::state::AppState;

use agent::AgentSystem;
use core::auth::{Jwt, Role};
use axum::Router;
use config::secret;
use model::{EmbeddingModel, Model};
use plugin::Registry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use symbol::{ApiEmbedder, Embedder, Extractor, LlmExtractor, MemoryStore};

/// 组装失败的原因（启动期错误，直接展示给运维）。
#[derive(Debug)]
pub struct BuildError(pub String);

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BuildError {}

fn fail(context: &str, e: impl std::fmt::Display) -> BuildError {
    BuildError(format!("{context}: {e}"))
}

/// 组装后的后端。
pub struct Built {
    pub router: Router,
    pub state: AppState,
}

/// 按配置组装后端。`tools` 是可供 agent 使用的真实工具池（文件、命令等）。
///
/// 配置里的相对路径（如 `http.web_dir`）按进程工作目录解析；从配置文件加载时
/// 先调用 [`ServerConfig::resolve_paths`] 改为相对配置文件所在目录。
pub async fn build(config: &ServerConfig, tools: Registry) -> Result<Built, BuildError> {
    let jwt_secret = secret(&config.auth.jwt_secret, &config.auth.jwt_secret_env)
        .ok_or_else(|| BuildError("auth: jwt_secret / jwt_secret_env is not set".into()))?;
    let jwt = Jwt::new(jwt_secret.as_bytes(), config.auth.token_ttl_secs).map_err(|e| fail("auth", e))?;

    // 模型池
    let mut models: HashMap<String, Model> = HashMap::new();
    for (name, cfg) in &config.models {
        models.insert(name.clone(), cfg.build().map_err(|e| fail(&format!("model {name}"), e))?);
    }

    // 存储：后端与记忆库共用一个连接（同一 SurrealKV 文件只能打开一次）
    let conn = surrealdb::engine::any::connect(config.storage.endpoint.as_str())
        .await
        .map_err(|e| fail("storage", e))?;
    let db = core::db::Db::new(&conn, &config.storage.namespace, &config.storage.database)
        .await
        .map_err(|e| fail("storage", e))?;
    bootstrap_admin(config, &db).await?;

    // 记忆器
    let memory = match &config.memory {
        None => None,
        Some(section) => {
            let embedder: Option<Arc<dyn Embedder>> = config.embedding.as_ref().map(|e| {
                let key = secret(&e.api_key, &e.api_key_env).unwrap_or_default();
                let mut client = EmbeddingModel::new(&e.base_url, &key, &e.model);
                if e.send_dimensions {
                    client = client.dimensions(e.dimension as u32);
                }
                Arc::new(ApiEmbedder::new(client, e.dimension)) as Arc<dyn Embedder>
            });
            let extractor: Option<Arc<dyn Extractor>> = match &section.extractor_model {
                None => None,
                Some(name) => {
                    let model = models
                        .get(name)
                        .ok_or_else(|| BuildError(format!("memory: unknown extractor_model {name:?}")))?;
                    Some(Arc::new(LlmExtractor::new(model.clone())))
                }
            };
            let store = MemoryStore::with_connection(&conn, section.store.clone(), embedder, extractor)
                .await
                .map_err(|e| fail("memory", e))?;
            Some(store)
        }
    };

    let agents = AgentSystem::new(config.agent.clone(), models, tools).map_err(|e| fail("agent", e))?;
    let state = AppState::new(config, db, agents, memory, jwt);
    Ok(Built {
        router: routes::router(state.clone()),
        state,
    })
}

/// 没有任何用户时按配置创建管理员。
async fn bootstrap_admin(config: &ServerConfig, db: &core::db::Db) -> Result<(), BuildError> {
    let count = db.user_count().await.map_err(|e| fail("storage", e.message))?;
    if count > 0 {
        return Ok(());
    }
    let Some(admin) = &config.auth.bootstrap_admin else {
        tracing::warn!("no users exist and auth.bootstrap_admin is not configured; nobody can log in");
        return Ok(());
    };
    let password = secret(&admin.password, &admin.password_env)
        .ok_or_else(|| BuildError("auth.bootstrap_admin: password / password_env is not set".into()))?;
    db.create_user(&admin.username, &password, Role::Admin)
        .await
        .map_err(|e| fail("auth.bootstrap_admin", e.message))?;
    tracing::info!(user = %admin.username, "created bootstrap admin");
    Ok(())
}

/// 记忆器后台维护：周期性处理抽取队列、冻结 / 清理衰减的关系。
pub fn spawn_memory_worker(state: AppState, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(e) = maintain_spaces(&state).await {
                tracing::warn!("memory maintenance failed: {e}");
            }
        }
    })
}

/// 对所有记忆空间跑一轮：抽取至多 32 条待处理段落，然后衰减维护。
pub async fn maintain_spaces(state: &AppState) -> Result<(), String> {
    let Some(cfg) = &state.inner.memory_cfg else {
        return Ok(());
    };
    let owners: Vec<String> = if cfg.per_user {
        let users = state.db().list_users().await.map_err(|e| e.message)?;
        users.into_iter().map(|u| u.id).collect()
    } else {
        vec![String::new()]
    };
    for owner in owners {
        if let Some(store) = state.memory_for(&owner).await.map_err(|e| e.message)? {
            store.process_pending(32).await.map_err(|e| e.to_string())?;
            store.maintain().await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
