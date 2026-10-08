//! OpenAI Chat Completions 协议包。
//!
//! - [`client`]：协议引擎（客户端、线协议、参数）与原生 OpenAI 预设。
//! - [`deepseek`]：DeepSeek 的 OpenAI 兼容预设（薄层）。

pub mod client;
pub mod deepseek;

pub use client::{
    ChatResponse, OpenAiArgs, OpenAiModel, OpenAiVendor, OpenaiTool, ResponseFormat, Thinking,
    ThinkingConfig, ThinkingType, ToolChoice, DEEPSEEK_BASE_URL, OPENAI_BASE_URL,
};
