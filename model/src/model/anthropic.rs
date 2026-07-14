//! Anthropic Messages 协议包。
//!
//! - [`anthropic`]：协议实现（客户端、线协议、参数）。

#[allow(clippy::module_inception)]
pub mod anthropic;

pub use anthropic::{AnthropicArgs, AnthropicModel, ANTHROPIC_BASE_URL, DEFAULT_MAX_TOKENS};
