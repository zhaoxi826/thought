//! 三层编排：会话（主控队列、任务表）与元工具（派单、待办、子 agent）。

pub mod session;
pub(crate) mod tools;
