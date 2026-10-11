//! 服务端的心智装配：把 `mind` crate 的端口接到 SurrealDB、记忆库、预算、网页、
//! agent 劳动力上（docs/mind-design.md §5）。

pub mod ports;
pub mod store;

use crate::config::ServerConfig;
use crate::core::budget::Budget;
use crate::core::db::Db;
use crate::core::tasks::TaskRegistry;
use agent::{AgentSystem, Observers, SessionOptions, Workforce};
use mind::{Mind, MindConfig, Percept, Ports, SystemClock};
use plugin::Registry;
use ports::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use store::MindDb;
use symbol::MemoryStore;

/// `[mind]` 配置段。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MindSection {
    /// 是否启用心智（启用后对话走她的意识流）。
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 她思考用的模型（模型池里的 key）。
    pub model: String,

    /// 压缩摘要用的廉价模型；缺省用 `model`。
    #[serde(default)]
    pub summary_model: Option<String>,

    /// 单个回合内最多几轮工具调用。
    #[serde(default = "default_turns")]
    pub max_turns: usize,

    /// 心智参数（出身、工作记忆、时区……）。
    #[serde(flatten)]
    pub core: MindConfig,
}

fn default_true() -> bool {
    true
}

fn default_turns() -> usize {
    16
}

/// 运行中的心智与它的附属件。
#[derive(Clone)]
pub struct MindHandle {
    pub mind: Mind,
    pub db: Arc<MindDb>,
    pub web: WebExpression,
    pub workforce: Arc<Workforce>,
}

/// 她的劳动力（L2 / L3）使用的会话 id。
pub const WORKFORCE_SESSION: &str = "mind";

#[allow(clippy::too_many_arguments)]
pub async fn start(
    config: &ServerConfig,
    section: &MindSection,
    conn: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    users: Db,
    agents: &AgentSystem,
    tasks: &TaskRegistry,
    budget: &Budget,
    memory: Option<MemoryStore>,
    tools: Registry,
) -> Result<MindHandle, String> {
    let build_model = |name: &str| {
        config
            .models
            .get(name)
            .ok_or_else(|| format!("mind: unknown model {name:?}"))?
            .build()
            .map_err(|e| format!("mind: model {name}: {e}"))
    };
    let model = build_model(&section.model)?;
    let summary_model = section.summary_model.as_deref().map(build_model).transpose()?;

    let db = Arc::new(
        MindDb::new(conn, &config.storage.namespace, &config.storage.database)
            .await
            .map_err(|e| format!("mind storage: {e}"))?,
    );

    // 用量来源：回应算 user，自主算 mind，派出的任务按它们的回合继承（这里统一算 mind）
    budget.set_origin(RESPONSIVE_SESSION, "user");
    budget.set_origin(AUTONOMOUS_SESSION, "mind");
    budget.set_origin(WORKFORCE_SESSION, "mind");
    // 任务注册表：她的任务归第一个管理员（admin 在任务页能看到她派出的所有任务）
    let owner = users
        .list_users()
        .await
        .map_err(|e| e.message)?
        .into_iter()
        .find(|u| u.role == crate::core::auth::Role::Admin)
        .map(|u| u.id)
        .unwrap_or_default();
    tasks.register_session(WORKFORCE_SESSION, &owner);
    let observer: Arc<dyn agent::EventObserver> =
        Arc::new(Observers(vec![Arc::new(tasks.clone()), Arc::new(budget.clone())]));

    // 记忆工具：她自己的唯一空间
    let mut mind_tools = tools;
    if let Some(memory) = &memory {
        symbol::register_memory_tools(&mut mind_tools, memory.clone(), "mind");
    }

    // 劳动力：任务结果作为感知回到她的意识流
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<Percept>();
    let workforce = Arc::new(agents.start_workforce(
        WORKFORCE_SESSION,
        SessionOptions {
            tools: mind_tools.clone(),
            observer: Some(Arc::clone(&observer)),
            first_task_id: tasks.max_task_id(WORKFORCE_SESSION).await.map_err(|e| e.message)? + 1,
            ..Default::default()
        },
        move |task_id, worker, result| {
            let (ok, result) = match result {
                Ok(r) => (true, r),
                Err(e) => (false, e),
            };
            let _ = done_tx.send(Percept::TaskDone { task_id, worker, ok, result, for_person: None });
        },
    ));
    mind_tools.extend(&workforce.tools());

    let web = WebExpression::new();
    let events = agent::EventSender::with_observer(RESPONSIVE_SESSION, 1024, Some(observer));
    let limits = &config.agent.limits;
    let mind = Mind::start(
        section.core.clone(),
        Ports {
            stream: db.clone(),
            selves: db.clone(),
            memory: memory.map(|m| Arc::new(MemoryAdapter(m)) as Arc<dyn mind::MemoryPort>),
            expression: Arc::new(web.clone()),
            metabolism: Arc::new(MetabolismAdapter(budget.clone())),
            auditor: Arc::new(AuditAdapter(db.clone())),
            people: Arc::new(PeopleAdapter(users)),
            cognition: Arc::new(RunnerCognition {
                model,
                max_turns: section.max_turns,
                max_tool_output_chars: limits.max_tool_output_chars,
                events: events.clone(),
            }),
            clock: Arc::new(SystemClock),
            tools: mind_tools,
            summarizer: summary_model.map(|model| {
                Arc::new(RunnerCognition { model, max_turns: 1, max_tool_output_chars: 4096, events }) as Arc<dyn mind::Cognition>
            }),
        },
    )
    .await
    .map_err(|e| format!("mind: {e}"))?;

    // 任务结果 → 感知
    let m = mind.clone();
    tokio::spawn(async move {
        while let Some(p) = done_rx.recv().await {
            let _ = m.perceive(p).await;
        }
    });

    // 待办到期 → 感知（每 15 秒检查一次，持久化，重启不丢）
    let (m, selves) = (mind.clone(), db.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tick.tick().await;
            let now = mind::Clock::now_ms(&SystemClock);
            match mind::SelfStore::take_due(&*selves, now).await {
                Ok(due) => {
                    for item in due {
                        let _ = m.perceive(Percept::Due { text: item.text }).await;
                    }
                }
                Err(e) => tracing::warn!("scheduled check failed: {e}"),
            }
        }
    });

    Ok(MindHandle { mind, db, web, workforce })
}
