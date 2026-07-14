//! DeepSeek 适配。
//!
//! 复用 [`OpenAiCompatModel`] 的调用逻辑，vendor 特有字段为 `thinking` 与
//! `reasoning_effort`（见 [`DeepseekExtra`]）。

use crate::model::base::OpenAIMessage;
use crate::model::client::OpenAiCompatModel;
use crate::model::wire::{ChatArgs, ChatRequest, Tool};
use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// DeepSeek 模型客户端。
pub type DeepSeekModel = OpenAiCompatModel<DeepseekArgs>;

/// DeepSeek 请求体的 vendor 特有字段，展开到请求顶层。
#[derive(Serialize, Debug, Clone)]
pub struct DeepseekExtra {
    /// 思考模式开关，默认关闭。
    pub thinking: Thinking,

    /// 思考强度配置，默认 high。
    pub reasoning_effort: ThinkingConfig,
}

/// DeepSeek 预存调用参数。
#[derive(Clone, Debug)]
pub struct DeepseekArgs {
    /// 使用模型的 id。
    pub model: String,

    /// 思考模式开关。
    pub thinking: Thinking,

    /// 思考强度配置。
    pub reasoning_effort: ThinkingConfig,

    /// 温度参数，内部用 0-200 的 u8 表示 0.0-2.0，序列化时自动除 100。
    pub temperature: u8,

    /// top-p 采样参数，内部用 0-100 的 u8 表示 0.0-1.0，序列化时自动除 100。
    pub top_p: u8,

    /// 工具列表。
    pub tools: Vec<Tool>,
}

impl Default for DeepseekArgs {
    fn default() -> Self {
        Self {
            model: String::new(),
            thinking: Thinking::enabled(),
            reasoning_effort: ThinkingConfig::High,
            temperature: 100,
            top_p: 100,
            tools: Vec::new(),
        }
    }
}

impl DeepseekArgs {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = thinking;
        self
    }

    pub fn reasoning_effort(mut self, reasoning_effort: ThinkingConfig) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    pub fn temperature(mut self, temperature: u8) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn top_p(mut self, top_p: u8) -> Self {
        self.top_p = top_p;
        self
    }

    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }
}

impl ChatArgs for DeepseekArgs {
    type Extra = DeepseekExtra;

    fn default_base_url() -> &'static str {
        DEFAULT_BASE_URL
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn build_request(&self, messages: Vec<OpenAIMessage>) -> ChatRequest<DeepseekExtra> {
        ChatRequest {
            messages,
            model: self.model.clone(),
            response_format: None,
            stream: false,
            temperature: self.temperature,
            top_p: self.top_p,
            tools: self.tools.clone(),
            user_id: None,
            extra: DeepseekExtra {
                thinking: self.thinking.clone(),
                reasoning_effort: self.reasoning_effort.clone(),
            },
        }
    }
}

/// 思考模式配置。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub r#type: ThinkingType,
}

impl Thinking {
    pub fn disabled() -> Self {
        Self {
            r#type: ThinkingType::Disabled,
        }
    }

    pub fn enabled() -> Self {
        Self {
            r#type: ThinkingType::Enabled,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub enum ThinkingType {
    #[default]
    #[serde(rename = "disabled")]
    Disabled,

    #[serde(rename = "enabled")]
    Enabled,
}

/// 思考强度配置。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub enum ThinkingConfig {
    #[serde(rename = "high")]
    #[default]
    High,

    #[serde(rename = "max")]
    Max,
}
