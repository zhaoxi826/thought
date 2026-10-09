//! agent 运行事件：供 server 以 SSE 推给前端，或用于日志 / 观测。

use model::Usage;
use serde::Serialize;

/// 一个会话内产生的事件。`agent` 是发出者标签：`controller`、`coder#3`、
/// `coder#3/sub1` 这样的层级路径。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 模型输出的文本碎片（流式）。
    TextDelta { agent: String, text: String },

    /// 模型的推理碎片（流式）。
    ReasoningDelta { agent: String, text: String },

    /// 发起一次工具调用。
    ToolCall {
        agent: String,
        id: String,
        name: String,
        arguments: String,
    },

    /// 工具执行完毕（`output` 已截断）。
    ToolResult {
        agent: String,
        id: String,
        name: String,
        output: String,
    },

    /// 一轮模型调用的用量。
    Usage { agent: String, usage: UsageView },

    /// 主控派出一个任务。
    TaskStarted {
        task_id: u64,
        worker: String,
        task: String,
    },

    /// 任务结束；`error` 为 `Some` 表示失败或被取消。
    TaskFinished {
        task_id: u64,
        worker: String,
        result: Option<String>,
        error: Option<String>,
    },

    /// 主控给用户的最终回复（一次输入批处理完成）。
    Reply { text: String },

    /// 主控处理输入时出错。
    Error { message: String },
}

/// [`Usage`] 的可序列化视图。
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageView {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl From<Usage> for UsageView {
    fn from(u: Usage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

/// 无损事件观察者：同步回调，每个事件都会送达（不像 broadcast 会因慢消费者丢失）。
/// 用于持久化（任务注册表）等不能丢事件的场景；实现必须快速返回、不得阻塞。
pub trait EventObserver: Send + Sync {
    fn observe(&self, session_id: &str, event: &AgentEvent);
}

/// 事件出口：broadcast 给实时订阅者（多个前端标签页，没有订阅者时静默丢弃），
/// 同时同步交给可选的无损观察者。`Clone` 廉价。
#[derive(Clone)]
pub struct EventSender {
    session_id: std::sync::Arc<str>,
    broadcast: tokio::sync::broadcast::Sender<AgentEvent>,
    observer: Option<std::sync::Arc<dyn EventObserver>>,
}

impl EventSender {
    /// 只有 broadcast、没有观察者（单独使用 `Runner` 时）。
    pub fn new(capacity: usize) -> Self {
        Self::with_observer("", capacity, None)
    }

    pub fn with_observer(
        session_id: &str,
        capacity: usize,
        observer: Option<std::sync::Arc<dyn EventObserver>>,
    ) -> Self {
        let (broadcast, _) = tokio::sync::broadcast::channel(capacity);
        Self {
            session_id: session_id.into(),
            broadcast,
            observer,
        }
    }

    pub fn send(&self, event: AgentEvent) {
        if let Some(observer) = &self.observer {
            observer.observe(&self.session_id, &event);
        }
        let _ = self.broadcast.send(event);
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AgentEvent> {
        self.broadcast.subscribe()
    }
}
