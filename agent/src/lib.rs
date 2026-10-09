//! 三层 agent 系统。
//!
//! - [`AgentSystem`]：模型池 + 工具池 + [`AgentConfig`]，`start_session` 开出会话；
//! - [`SessionHandle`]：向会话的主控队列投递 [`Input`]、订阅 [`AgentEvent`]；
//! - [`Runner`]：三层共用的 ReAct 执行器，也可单独使用。

pub mod core;
pub mod orchestration;

pub use core::config::{AgentConfig, ControllerSpec, Limits, WorkerSpec};
pub use core::event::{AgentEvent, EventObserver, EventSender, UsageView};
pub use core::runner::Runner;
pub use orchestration::session::{
    AgentSystem, HistorySink, Input, SessionHandle, SessionOptions, TaskInfo, NO_REPLY,
};

use model::Error as ModelError;

/// agent 运行错误。
#[derive(Debug)]
pub enum AgentError {
    /// 底层模型调用失败。
    Model(ModelError),

    /// 超过最大轮次仍未得到最终回复。
    MaxTurnsExceeded,

    /// 配置引用了不存在的模型 / 工具等。
    Config(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Model(e) => write!(f, "model error: {e}"),
            AgentError::MaxTurnsExceeded => write!(f, "max turns exceeded"),
            AgentError::Config(msg) => write!(f, "config error: {msg}"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AgentError::Model(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ModelError> for AgentError {
    fn from(e: ModelError) -> Self {
        AgentError::Model(e)
    }
}
