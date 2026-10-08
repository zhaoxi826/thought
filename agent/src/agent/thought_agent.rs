//! 最简 agent：模型 + 工具注册表的 ReAct 循环。
//!
//! 每轮把完整对话历史发给模型：模型回工具调用就经 [`Registry`] 执行，按调用 id
//! 以标准 tool 消息回传结果，直到模型回纯文本为止。

use model::{Error as ModelError, Message, Model};
use plugin::Registry;

/// agent 运行错误。
#[derive(Debug)]
pub enum AgentError {
    /// 底层模型调用失败。
    Model(ModelError),

    /// 超过最大轮次仍未得到最终回复。
    MaxTurnsExceeded,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Model(e) => write!(f, "model error: {e}"),
            AgentError::MaxTurnsExceeded => write!(f, "max turns exceeded"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AgentError::Model(e) => Some(e),
            AgentError::MaxTurnsExceeded => None,
        }
    }
}

impl From<ModelError> for AgentError {
    fn from(e: ModelError) -> Self {
        AgentError::Model(e)
    }
}

/// 最简 agent：维护对话历史，循环「模型 → 工具 → 模型」直到模型不再调工具。
pub struct Agent {
    model: Model,
    registry: Registry,
    system: Option<String>,
    history: Vec<Message>,
    max_turns: usize,
}

impl Agent {
    /// 默认最大轮次，防止工具循环失控。
    pub const DEFAULT_MAX_TURNS: usize = 10;

    /// 创建 agent：把注册表里的工具定义装到模型上。
    pub fn new(mut model: Model, registry: Registry) -> Self {
        model.set_tools(&registry.definitions());
        Self {
            model,
            registry,
            system: None,
            history: Vec::new(),
            max_turns: Self::DEFAULT_MAX_TURNS,
        }
    }

    /// 设置 system 提示（每轮请求都会带上）。
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// 覆盖最大轮次（默认 [`DEFAULT_MAX_TURNS`](Agent::DEFAULT_MAX_TURNS)）。
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    /// 对话历史（含 system 消息）。
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// 跑一轮任务：模型循环调工具直到给出最终文本回复。
    pub async fn run(&mut self, task: &str) -> Result<String, AgentError> {
        if self.history.is_empty()
            && let Some(system) = &self.system
        {
            self.history.push(Message::system(system.clone()));
        }
        self.history.push(Message::user(task));

        for _ in 0..self.max_turns {
            let output = self.model.chat(&self.history, None, None).await?;

            if output.tool_calls.is_empty() {
                let content = output.content.clone().unwrap_or_default();
                self.history.push(output.into());
                return Ok(content);
            }

            // assistant 这一轮（含 tool_calls）原样入历史，再逐个执行工具按 id 回传
            let calls = output.tool_calls.clone();
            self.history.push(output.into());
            for call in calls {
                let result = match self.registry.call(&call.name, &call.arguments).await {
                    Some(result) => result,
                    None => format!("unknown tool: {}", call.name),
                };
                self.history.push(Message::tool(call.id, result));
            }
        }

        Err(AgentError::MaxTurnsExceeded)
    }
}
