//! agent 系统的元工具：主控的派单 / 管理工具，以及 L2 的 `spawn_subagent`。

use crate::config::WorkerSpec;
use crate::session::Session;
use model::{Function, Message, Parameter, ParameterType};
use plugin::{Registry, Tool};
use serde::Deserialize;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 解析参数，失败时把错误文本直接作为工具输出返回给模型。
macro_rules! parse_args {
    ($tool:literal, $arguments:expr) => {
        match serde_json::from_str($arguments) {
            Ok(args) => args,
            Err(e) => return format!("invalid arguments for {}: {e}", $tool),
        }
    };
}

/// 主控可用工具：四个元工具 + 配置里额外允许的真实工具。
pub(crate) fn controller_registry(session: &Arc<Session>) -> Registry {
    let mut registry = session
        .system
        .tools
        .subset(&session.system.config.controller.tools)
        .expect("validated in AgentSystem::new");
    registry
        .register(Arc::new(DispatchTask(Arc::clone(session))))
        .register(Arc::new(ListTasks(Arc::clone(session))))
        .register(Arc::new(CancelTask(Arc::clone(session))))
        .register(Arc::new(AddTodo(Arc::clone(session))));
    registry
}

/// L2 worker 可用工具：配置的工具 + （允许时）`spawn_subagent`。
pub(crate) fn worker_registry(session: &Arc<Session>, spec: &WorkerSpec, label: &str) -> Registry {
    let mut registry = session
        .system
        .tools
        .subset(&spec.tools)
        .expect("validated in AgentSystem::new");
    if spec.can_spawn {
        registry.register(Arc::new(SpawnSubagent {
            session: Arc::clone(session),
            spec: spec.clone(),
            parent: label.to_owned(),
            next: AtomicU32::new(1),
        }));
    }
    registry
}

fn string_param(name: &str, description: &str) -> Parameter {
    Parameter::required(name, description, ParameterType::String)
}

// ---- dispatch_task ----

struct DispatchTask(Arc<Session>);

#[derive(Deserialize)]
struct DispatchArgs {
    worker: String,
    task: String,
}

#[async_trait::async_trait]
impl Tool for DispatchTask {
    fn name(&self) -> &str {
        "dispatch_task"
    }

    fn definition(&self) -> Function {
        let workers: Vec<_> = self.0.system.config.workers.iter().map(|w| w.name.as_str()).collect();
        Function {
            name: "dispatch_task".to_owned(),
            description: "Dispatch a self-contained task to a worker agent. Returns immediately \
                          with a task id; the result arrives later as a <task_result> message."
                .to_owned(),
            parameters: vec![
                string_param("worker", &format!("Worker name, one of: {}", workers.join(", "))),
                string_param(
                    "task",
                    "Complete task description with all context the worker needs",
                ),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: DispatchArgs = parse_args!("dispatch_task", arguments);
        match self.0.dispatch(&args.worker, args.task) {
            Ok(task_id) => format!("dispatched as task {task_id}"),
            Err(e) => format!("dispatch failed: {e}"),
        }
    }
}

// ---- list_tasks ----

struct ListTasks(Arc<Session>);

#[async_trait::async_trait]
impl Tool for ListTasks {
    fn name(&self) -> &str {
        "list_tasks"
    }

    fn definition(&self) -> Function {
        Function {
            name: "list_tasks".to_owned(),
            description: "List tasks that are still running.".to_owned(),
            parameters: vec![],
            strict: false,
        }
    }

    async fn call(&self, _arguments: &str) -> String {
        let tasks = self.0.tasks();
        if tasks.is_empty() {
            return "no running tasks".to_owned();
        }
        tasks
            .iter()
            .map(|t| format!("task {} [{}]: {}", t.task_id, t.worker, t.task))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---- cancel_task ----

struct CancelTask(Arc<Session>);

#[derive(Deserialize)]
struct CancelArgs {
    task_id: u64,
}

#[async_trait::async_trait]
impl Tool for CancelTask {
    fn name(&self) -> &str {
        "cancel_task"
    }

    fn definition(&self) -> Function {
        Function {
            name: "cancel_task".to_owned(),
            description: "Cancel a running task.".to_owned(),
            parameters: vec![Parameter::required(
                "task_id",
                "Id returned by dispatch_task",
                ParameterType::Integer,
            )],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: CancelArgs = parse_args!("cancel_task", arguments);
        if self.0.cancel(args.task_id) {
            format!("task {} cancelled", args.task_id)
        } else {
            format!("task {} is not running", args.task_id)
        }
    }
}

// ---- add_todo ----

struct AddTodo(Arc<Session>);

#[derive(Deserialize)]
struct AddTodoArgs {
    text: String,
    #[serde(default)]
    delay_seconds: u64,
}

/// 待办最长延迟：7 天。
const MAX_TODO_DELAY: u64 = 7 * 24 * 3600;

#[async_trait::async_trait]
impl Tool for AddTodo {
    fn name(&self) -> &str {
        "add_todo"
    }

    fn definition(&self) -> Function {
        Function {
            name: "add_todo".to_owned(),
            description: "Schedule a reminder for yourself. After the delay it comes back to you \
                          as a <todo_due> message, so you can follow up or dispatch more work."
                .to_owned(),
            parameters: vec![
                string_param("text", "What to do when the reminder fires"),
                Parameter::optional(
                    "delay_seconds",
                    "Seconds to wait before the reminder fires (default 0)",
                    ParameterType::Integer,
                ),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: AddTodoArgs = parse_args!("add_todo", arguments);
        let delay = args.delay_seconds.min(MAX_TODO_DELAY);
        self.0.schedule_todo(args.text, Duration::from_secs(delay));
        format!("todo scheduled in {delay}s")
    }
}

// ---- spawn_subagent ----

struct SpawnSubagent {
    session: Arc<Session>,
    spec: WorkerSpec,
    parent: String,
    next: AtomicU32,
}

#[derive(Deserialize)]
struct SpawnArgs {
    task: String,
}

#[async_trait::async_trait]
impl Tool for SpawnSubagent {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn definition(&self) -> Function {
        Function {
            name: "spawn_subagent".to_owned(),
            description: "Run a sub-agent on a self-contained sub-task and wait for its final \
                          report. The sub-agent has your tools but none of your context."
                .to_owned(),
            parameters: vec![string_param(
                "task",
                "Complete sub-task description with all needed context",
            )],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: SpawnArgs = parse_args!("spawn_subagent", arguments);
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let runner = self
            .session
            .subagent_runner(&self.spec, format!("{}/sub{n}", self.parent));
        let mut history = vec![
            Message::system(format!(
                "{}\n\nYou are a sub-agent. Finish the task and reply with a concise final report.",
                self.spec.system
            )),
            Message::user(args.task),
        ];
        match runner.run(&mut history).await {
            Ok(report) => report,
            Err(e) => format!("sub-agent failed: {e}"),
        }
    }
}
