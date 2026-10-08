//! 各厂商协议实现与配置构造。
//!
//! 只有两种协议：OpenAI Chat Completions（DeepSeek 是它的预设）与 Anthropic
//! Messages；另有 OpenAI 兼容的向量化接口。

pub mod anthropic;
pub mod config;
pub mod embedding;
pub mod openai;
pub(crate) mod sse;
