//! 大模型接口适配层。
//!
//! - [`core`]：协议无关的消息、输出、流式累加、工具定义、错误；
//! - [`providers`]：OpenAI（含 DeepSeek 预设）/ Anthropic 协议、向量化、配置构造；
//! - [`Model`]：运行时按协议分派的统一入口。

pub mod core;
mod model;
pub mod providers;

pub use model::Model;

// 统一模型接口
pub use core::{
    collect_stream, ChatChunk, ChatOutput, ChatStream, Error, Function, Message,
    Model as ModelTrait, Parameter, ParameterType, StreamAccumulator, ToolCall, ToolCallDelta,
    Usage,
};

// 配置构造与向量化
pub use providers::config::{ConfigError, ModelConfig, Provider};
pub use providers::embedding::EmbeddingModel;

// OpenAI Chat Completions（DeepSeek 为其预设）
pub use providers::openai::{
    OpenAiArgs, OpenAiModel, OpenAiVendor, OpenaiTool, ResponseFormat, Thinking, ThinkingConfig,
    ThinkingType, ToolChoice, DEEPSEEK_BASE_URL, OPENAI_BASE_URL,
};

// Anthropic Messages
pub use providers::anthropic::{AnthropicArgs, AnthropicModel, AnthropicTool, ANTHROPIC_BASE_URL};
