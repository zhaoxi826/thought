//! 三层 agent 系统 + 队列驱动的会话。
//!
//! - **L1 主控**（每个会话一个）：从队列取输入，只能派单 / 查询 / 取消 / 记待办，
//!   不直接干活。最终文本即给用户的回复。
//! - **L2 工作 agent**：由配置定义（[`WorkerSpec`]），在独立 tokio task 里跑，
//!   完成后把结果作为一条输入塞回主控队列。
//! - **L3 子 agent**：L2 通过 `spawn_subagent` 同步创建，继承 L2 的工具（不含再派生），
//!   结果直接作为工具输出返回。
//!
//! 队列来源：server 转来的用户消息、任务结果、到期的待办。主控每次把队列里
//! 已积压的输入一次性取出合并处理，避免逐条触发模型调用。

use crate::core::config::{AgentConfig, WorkerSpec};
use crate::core::event::{AgentEvent, EventObserver, EventSender};
use crate::core::runner::Runner;
use crate::orchestration::tools::{controller_registry, worker_registry};
use crate::AgentError;
use model::{Message, Model};
use plugin::Registry;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

use tokio::task::AbortHandle;
use tracing::warn;

/// 主控回复这个标记表示“无需告知用户”。
pub const NO_REPLY: &str = "NO_REPLY";

/// 进入主控队列的一条输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// 用户消息（来自 server）。
    User(String),

    /// 到期的待办（主控自己通过 `add_todo` 安排的）。
    Todo(String),

    /// 工作 agent 的结果。
    TaskDone {
        task_id: u64,
        worker: String,
        result: Result<String, String>,
    },
}

impl Input {
    /// 渲染成交给主控模型的文本。
    fn render(&self) -> String {
        match self {
            Input::User(text) => text.clone(),
            Input::Todo(text) => format!("<todo_due>\n{text}\n</todo_due>"),
            Input::TaskDone {
                task_id,
                worker,
                result: Ok(result),
            } => format!("<task_result task_id=\"{task_id}\" worker=\"{worker}\" status=\"ok\">\n{result}\n</task_result>"),
            Input::TaskDone {
                task_id,
                worker,
                result: Err(error),
            } => format!("<task_result task_id=\"{task_id}\" worker=\"{worker}\" status=\"failed\">\n{error}\n</task_result>"),
        }
    }
}

/// 会话历史持久化出口：主控每处理完一批输入回调一次（含 system 消息）。
#[async_trait::async_trait]
pub trait HistorySink: Send + Sync {
    async fn save(&self, session_id: &str, history: &[Message]);
}

/// 启动会话的选项。
#[derive(Default)]
pub struct SessionOptions {
    /// 从持久化恢复的历史（其中的 system 消息会被丢弃，按当前配置重建）。
    pub history: Vec<Message>,

    /// 会话专属工具（如按用户隔离的记忆工具），对主控、worker、子 agent 都可用。
    pub tools: Registry,

    pub sink: Option<Arc<dyn HistorySink>>,

    /// 无损事件观察者（如任务注册表）；收到本会话的全部事件。
    pub observer: Option<Arc<dyn EventObserver>>,

    /// 任务 id 起点（恢复会话时传入已用过的最大 id + 1，保证会话内 id 不重复）。
    /// 0 视为 1。
    pub first_task_id: u64,
}

/// 一个正在运行的任务的概况。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskInfo {
    pub task_id: u64,
    pub worker: String,
    pub task: String,
}

/// 整个 agent 系统：模型池 + 工具池 + 配置。`Clone` 廉价。
#[derive(Clone)]
pub struct AgentSystem {
    inner: Arc<SystemInner>,
}

pub(crate) struct SystemInner {
    pub(crate) config: AgentConfig,
    pub(crate) models: HashMap<String, Model>,
    pub(crate) tools: Registry,
}

impl AgentSystem {
    /// 校验配置里引用的模型 / 工具都存在、worker 名不重复。
    pub fn new(
        config: AgentConfig,
        models: HashMap<String, Model>,
        tools: Registry,
    ) -> Result<Self, AgentError> {
        let check_model = |owner: &str, name: &str| {
            if models.contains_key(name) {
                Ok(())
            } else {
                Err(AgentError::Config(format!("{owner}: unknown model {name:?}")))
            }
        };
        let check_tools = |owner: &str, names: &[String]| {
            tools
                .subset(names)
                .map(|_| ())
                .map_err(|name| AgentError::Config(format!("{owner}: unknown tool {name:?}")))
        };

        check_model("controller", &config.controller.model)?;
        check_tools("controller", &config.controller.tools)?;
        let mut seen = std::collections::HashSet::new();
        for worker in &config.workers {
            if !seen.insert(worker.name.as_str()) {
                return Err(AgentError::Config(format!("duplicate worker {:?}", worker.name)));
            }
            check_model(&worker.name, &worker.model)?;
            check_tools(&worker.name, &worker.tools)?;
        }

        Ok(Self {
            inner: Arc::new(SystemInner {
                config,
                models,
                tools,
            }),
        })
    }

    pub fn config(&self) -> &AgentConfig {
        &self.inner.config
    }

    /// 启动一个会话：spawn 主控循环，返回句柄。
    ///
    /// 所有句柄被丢弃后会话结束，仍在运行的任务一并取消。
    pub fn start_session(&self, session_id: impl Into<String>, options: SessionOptions) -> SessionHandle {
        let session_id = session_id.into();
        let SessionOptions {
            history,
            tools,
            sink,
            observer,
            first_task_id,
        } = options;
        let (inbox, rx) = mpsc::unbounded_channel();
        let events = EventSender::with_observer(&session_id, 1024, observer);
        let shared = Arc::new(Session {
            system: Arc::clone(&self.inner),
            session_id,
            inbox: inbox.downgrade(),
            events: events.clone(),
            next_task_id: AtomicU64::new(first_task_id.max(1)),
            tasks: Mutex::new(BTreeMap::new()),
            session_tools: tools,
        });
        tokio::spawn(controller_loop(Arc::clone(&shared), rx, history, sink));
        SessionHandle {
            inbox,
            events,
            shared,
        }
    }
}

/// 会话句柄：向主控队列投递输入、订阅事件。
#[derive(Clone)]
pub struct SessionHandle {
    inbox: mpsc::UnboundedSender<Input>,
    events: EventSender,
    shared: Arc<Session>,
}

impl SessionHandle {
    pub fn session_id(&self) -> &str {
        &self.shared.session_id
    }

    /// 投递一条输入；会话已结束时返回 `false`。
    pub fn send(&self, input: Input) -> bool {
        self.inbox.send(input).is_ok()
    }

    pub fn send_user(&self, text: impl Into<String>) -> bool {
        self.send(Input::User(text.into()))
    }

    /// 订阅之后产生的事件。
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    /// 正在运行的任务。
    pub fn tasks(&self) -> Vec<TaskInfo> {
        self.shared.tasks()
    }

    /// 取消一个运行中的任务（与主控 `cancel_task` 工具效果一致）；不在运行时返回 `false`。
    pub fn cancel_task(&self, task_id: u64) -> bool {
        self.shared.cancel(task_id)
    }
}

/// 会话共享状态：主控工具与后台任务都持有它。
///
/// 只持有队列的弱引用，避免工具 → 会话 → 队列的引用环让会话永不结束。
pub(crate) struct Session {
    pub(crate) system: Arc<SystemInner>,
    session_id: String,
    inbox: mpsc::WeakUnboundedSender<Input>,
    events: EventSender,
    next_task_id: AtomicU64,
    tasks: Mutex<BTreeMap<u64, RunningTask>>,
    pub(crate) session_tools: Registry,
}

struct RunningTask {
    info: TaskInfo,
    abort: AbortHandle,
}

impl Session {
    pub(crate) fn worker(&self, name: &str) -> Option<&WorkerSpec> {
        self.system.config.workers.iter().find(|w| w.name == name)
    }

    pub(crate) fn tasks(&self) -> Vec<TaskInfo> {
        let tasks = self.tasks.lock().expect("task table poisoned");
        tasks.values().map(|t| t.info.clone()).collect()
    }

    pub(crate) fn emit(&self, event: AgentEvent) {
        self.events.send(event);
    }

    fn push_input(&self, input: Input) {
        if let Some(inbox) = self.inbox.upgrade() {
            let _ = inbox.send(input);
        }
    }

    /// 派出一个任务；超过并发上限或 worker 不存在时返回错误文本。
    pub(crate) fn dispatch(self: &Arc<Self>, worker: &str, task: String) -> Result<u64, String> {
        let spec = self
            .worker(worker)
            .ok_or_else(|| format!("unknown worker {worker:?}"))?
            .clone();
        let mut tasks = self.tasks.lock().expect("task table poisoned");
        let limit = self.system.config.limits.max_concurrent_tasks;
        if tasks.len() >= limit {
            return Err(format!(
                "too many running tasks ({limit}); wait for results or cancel one"
            ));
        }

        let task_id = self.next_task_id.fetch_add(1, Ordering::Relaxed);
        let runner = self.worker_runner(&spec, format!("{}#{task_id}", spec.name));
        let session = Arc::clone(self);
        let task_text = task.clone();
        let handle = tokio::spawn(async move {
            let mut history = vec![Message::system(runner_system(&spec)), Message::user(task_text)];
            let result = runner.run(&mut history).await.map_err(|e| e.to_string());
            session.finish(task_id, result);
        });

        tasks.insert(
            task_id,
            RunningTask {
                info: TaskInfo {
                    task_id,
                    worker: worker.to_owned(),
                    task: task.clone(),
                },
                abort: handle.abort_handle(),
            },
        );
        drop(tasks);
        self.emit(AgentEvent::TaskStarted {
            task_id,
            worker: worker.to_owned(),
            task,
        });
        Ok(task_id)
    }

    /// 任务结束：移出任务表，发事件，结果入主控队列。
    fn finish(&self, task_id: u64, result: Result<String, String>) {
        let Some(task) = self.tasks.lock().expect("task table poisoned").remove(&task_id) else {
            return; // 已被取消
        };
        let worker = task.info.worker;
        self.emit(AgentEvent::TaskFinished {
            task_id,
            worker: worker.clone(),
            result: result.as_ref().ok().cloned(),
            error: result.as_ref().err().cloned(),
        });
        self.push_input(Input::TaskDone {
            task_id,
            worker,
            result,
        });
    }

    /// 取消任务；不存在时返回 `false`。
    pub(crate) fn cancel(&self, task_id: u64) -> bool {
        let Some(task) = self.tasks.lock().expect("task table poisoned").remove(&task_id) else {
            return false;
        };
        task.abort.abort();
        self.emit(AgentEvent::TaskFinished {
            task_id,
            worker: task.info.worker,
            result: None,
            error: Some("cancelled".to_owned()),
        });
        true
    }

    /// 安排一条待办：`delay` 后进入主控队列。
    pub(crate) fn schedule_todo(&self, text: String, delay: Duration) {
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(inbox) = inbox.upgrade() {
                let _ = inbox.send(Input::Todo(text));
            }
        });
    }

    /// 构造某个 worker 的执行器（L2）。
    fn worker_runner(self: &Arc<Self>, spec: &WorkerSpec, label: String) -> Runner {
        let limits = &self.system.config.limits;
        Runner::new(
            label.clone(),
            self.system.models[&spec.model].clone(),
            worker_registry(self, spec, &label),
            spec.max_turns,
            limits.max_tool_output_chars,
            self.events.clone(),
        )
    }

    /// 构造 L3 子 agent 执行器：工具与父 worker 相同但不能再派生。
    pub(crate) fn subagent_runner(&self, spec: &WorkerSpec, label: String) -> Runner {
        let limits = &self.system.config.limits;
        let mut tools = self
            .system
            .tools
            .subset(&spec.tools)
            .expect("validated in AgentSystem::new");
        tools.extend(&self.session_tools);
        Runner::new(
            label,
            self.system.models[&spec.model].clone(),
            tools,
            limits.subagent_max_turns,
            limits.max_tool_output_chars,
            self.events.clone(),
        )
    }

    fn abort_all(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().expect("task table poisoned"));
        for task in tasks.into_values() {
            task.abort.abort();
        }
    }
}

/// worker 的 system 提示：配置里的提示 + 能否派生子 agent 的说明。
fn runner_system(spec: &WorkerSpec) -> String {
    if spec.can_spawn {
        format!(
            "{}\n\nYou can delegate self-contained sub-tasks with the `spawn_subagent` tool. \
             Sub-agents share your tools, start with no context, and return a single final report. \
             Several calls in one turn run in parallel.",
            spec.system
        )
    } else {
        spec.system.clone()
    }
}

/// 主控的 system 提示：职责说明 + worker 清单 + 配置追加的提示。
fn controller_system(session: &Session) -> String {
    let config = &session.system.config;
    let mut workers = String::new();
    for w in &config.workers {
        workers.push_str(&format!("- {}: {}\n", w.name, w.description));
    }
    let mut prompt = format!(
        "You are the controller of a multi-agent system. You do not do the work yourself: \
         you understand what the user wants, split it into tasks, and dispatch each task to the \
         most suitable worker with `dispatch_task`. Write every task description so that it is \
         fully self-contained, because workers cannot see this conversation.\n\n\
         Available workers:\n{workers}\n\
         Tasks run asynchronously. Their results arrive later as <task_result> messages; due \
         reminders you scheduled with `add_todo` arrive as <todo_due> messages. Use `list_tasks` \
         and `cancel_task` to manage running tasks.\n\n\
         Your final text in each turn is shown to the user. Keep it short: acknowledge what you \
         dispatched, and summarize results when they arrive. If there is nothing worth telling \
         the user, reply with exactly {NO_REPLY}."
    );
    if let Some(extra) = &config.controller.system {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    prompt
}

/// 主控循环：取一批输入 → 跑一轮 → 发回复，直到所有句柄被丢弃。
async fn controller_loop(
    session: Arc<Session>,
    mut rx: mpsc::UnboundedReceiver<Input>,
    restored: Vec<Message>,
    sink: Option<Arc<dyn HistorySink>>,
) {
    let config = &session.system.config;
    let runner = Runner::new(
        "controller",
        session.system.models[&config.controller.model].clone(),
        controller_registry(&session),
        config.controller.max_turns,
        config.limits.max_tool_output_chars,
        session.events.clone(),
    );

    let mut history = Vec::with_capacity(restored.len() + 1);
    history.push(Message::system(controller_system(&session)));
    history.extend(restored.into_iter().filter(|m| !matches!(m, Message::System { .. })));

    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }
        let text = batch.iter().map(Input::render).collect::<Vec<_>>().join("\n\n");
        let checkpoint = history.len();
        history.push(Message::user(text));

        match runner.run(&mut history).await {
            Ok(reply) => {
                let reply = reply.trim();
                if !reply.is_empty() && reply != NO_REPLY {
                    session.emit(AgentEvent::Reply {
                        text: reply.to_owned(),
                    });
                }
            }
            Err(e) => {
                warn!(session = %session.session_id, "controller failed: {e}");
                // 回滚到本批之前，保证历史协议合法；输入本身保留，后续可见
                history.truncate(checkpoint);
                history.push(Message::user(batch.iter().map(Input::render).collect::<Vec<_>>().join("\n\n")));
                history.push(Message::assistant(format!("[error while handling this input: {e}]")));
                session.emit(AgentEvent::Error {
                    message: e.to_string(),
                });
            }
        }
        if let Some(sink) = &sink {
            sink.save(&session.session_id, &history).await;
        }
    }
    session.abort_all();
}
