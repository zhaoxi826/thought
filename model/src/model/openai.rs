//! OpenAI Chat Completions 协议包。
//!
//! - [`openai`]：协议引擎（客户端、线协议、参数）与原生 OpenAI 预设。
//! - [`deepseek`]：DeepSeek 的 OpenAI 兼容预设（薄层）。

#[allow(clippy::module_inception)]
pub mod openai;
pub mod deepseek;

pub use openai::{
    ChatRequest, ChatResponse, Function, OpenAiArgs, OpenAiModel, OpenAiVendor, Parameter,
    ParameterType, ResponseFormat, Thinking, ThinkingConfig, ThinkingType, Tool, ToolChoice,
    DEEPSEEK_BASE_URL, OPENAI_BASE_URL,
};
