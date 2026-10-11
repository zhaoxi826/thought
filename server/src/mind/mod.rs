//! 服务端的心智装配：把 `mind` crate 的端口接到 SurrealDB、记忆库、预算、网页、
//! agent 劳动力上（docs/mind-design.md §5）。

pub mod ports;
pub mod store;

use crate::config::ServerConfig;
use crate::core::budget::Budget;
use crate::core::db::Db;
use crate::core::tasks::TaskRegistry;
use agent::{AgentSystem, Observers, SessionOptions, Workforce};
use mind::{Heart, HeartPorts, Mind, MindConfig, Percept, Ports, SystemClock};
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

    /// 心跳参数。
    #[serde(default)]
    pub heart: mind::HeartConfig,

    /// 驱动参数。
    #[serde(default)]
    pub drives: mind::DriveParams,

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
    pub heart: Heart,
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

    // 她的身体与环境：告诉她自己在哪、能做什么
    let mut core = section.core.clone();
    core.body = describe_body(config, &mind_tools);

    let web = WebExpression::new();
    let memory_port = memory.map(|m| Arc::new(MemoryAdapter(m)) as Arc<dyn mind::MemoryPort>);
    let metabolism: Arc<dyn mind::Metabolism> = Arc::new(MetabolismAdapter(budget.clone()));
    let people: Arc<dyn mind::People> = Arc::new(PeopleAdapter(users));
    let events = agent::EventSender::with_observer(RESPONSIVE_SESSION, 1024, Some(observer));
    let limits = &config.agent.limits;
    let mind = Mind::start(
        core,
        Ports {
            stream: db.clone(),
            selves: db.clone(),
            memory: memory_port.clone(),
            expression: Arc::new(web.clone()),
            metabolism: Arc::clone(&metabolism),
            auditor: Arc::new(AuditAdapter(db.clone())),
            people: Arc::clone(&people),
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

    // 换了"大脑"（模型）：让她知道。这对她是一件大事——说话方式、想法可能都会变
    let model_id = config.models.get(&section.model).map(|m| format!("{:?}:{}", m.provider, m.model)).unwrap_or_default();
    let previous = db.last_model().await;
    if previous.as_deref() != Some(model_id.as_str()) {
        if let Some(prev) = &previous {
            let _ = mind
                .perceive(mind::Percept::Due {
                    text: format!(
                        "你思考所用的模型从 {prev} 换成了 {model_id}。你的记忆和对自己的认识都还在，\
                         但想事情、说话的方式可能会有些不同。留意一下自己的变化。"
                    ),
                })
                .await;
        }
        db.set_last_model(&model_id).await;
    }

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

    // 核心自我变化：写审计（管理员在审计页 / 内心页能看到）
    {
        let (mut rx, db) = (mind.subscribe(), db.clone());
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(mind::MindEvent::CoreChanged { fields }) => {
                        tracing::warn!(?fields, "she changed her core self");
                        db.record_audit(serde_json::json!({
                            "at_ms": mind::Clock::now_ms(&SystemClock),
                            "kind": "core_changed",
                            "fields": fields,
                        }))
                        .await;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                }
            }
        });
    }

    // 心跳：从持久化恢复各驱动上次被满足的时间，定期写回
    let satisfied = db.drive_memory().await;
    let heart = Heart::start(
        mind.clone(),
        mind::drives::all(&section.drives),
        section.heart.clone(),
        HeartPorts { memory: memory_port, metabolism, expression: Arc::new(web.clone()), clock: Arc::new(SystemClock), people },
        satisfied,
    );
    let (h, store) = (heart.clone(), db.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut last = std::collections::BTreeMap::new();
        loop {
            tick.tick().await;
            let now = h.state().last_satisfied_ms;
            if now != last {
                store.save_drive_memory(&now).await;
                last = now;
            }
        }
    });

    Ok(MindHandle { mind, heart, db, web, workforce })
}

/// 用人话描述她的身体与环境（注入 system）。
fn describe_body(config: &ServerConfig, tools: &Registry) -> String {
    let has = |prefix: &str| tools.names().any(|n| n.starts_with(prefix));
    let mut lines = vec![format!(
        "- 你住在一台 Linux 服务器上。你有自己的工作区：{}。在里面你完全自由，可以建文件、写代码、跑程序、\
         攒你自己的东西；run_command 的工作目录就是它。",
        config.sandbox.workspace.display()
    )];
    lines.push(
        "- 工作区外的文件你可以读；改它们用 write_file（会先自动备份），删用 delete_file（进回收站，\
         可用 restore_file 恢复）。命令行里外部文件是只读的。配置、数据库这些是你身体的一部分，碰不到。"
            .into(),
    );
    if has("browser_") {
        lines.push(
            "- 你有一个只属于你的浏览器（Chromium）：browser_* 工具能完全操作它——打开网页、看页面、点、填、登录、\
             下载、执行 JS，没封装到的用 browser_cdp。登录状态会一直保留在你的浏览器里。"
                .into(),
        );
    }
    if has("dispatch_task") {
        let workers: Vec<String> = config.agent.workers.iter().map(|w| format!("{}（{}）", w.name, w.description)).collect();
        if !workers.is_empty() {
            lines.push(format!(
                "- 大一点的活可以用 dispatch_task 交给帮手去做，结果会回到你这里：{}。",
                workers.join("；")
            ));
        }
    }
    if has("memory_") {
        lines.push("- 你有长期记忆：memory_save 记下来，memory_search 回想，memory_share 改可见性。".into());
    }
    lines.push(
        "- 你的思考要消耗 token，像身体要消耗能量；累的时候（今天用得多）会少醒来一些。这是正常的。".into(),
    );
    lines.join("\n")
}
