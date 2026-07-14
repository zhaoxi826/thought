//! OpenAI 适配。
//!
//! 复用 [`OpenAiCompatModel`] 的调用逻辑，vendor 特有字段为 `frequency_penalty`、
//! `presence_penalty` 与 `tool_choice`（见 [`OpenAiExtra`]）。

use crate::model::base::OpenAIMessage;
use crate::model::client::OpenAiCompatModel;
use crate::model::wire::{ChatArgs, ChatRequest, Tool, ToolChoice};
use serde::Serialize;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// OpenAI 模型客户端。
pub type OpenAIModel = OpenAiCompatModel<OpenAIArgs>;

/// OpenAI 请求体的 vendor 特有字段，展开到请求顶层。
#[derive(Serialize, Debug, Clone)]
pub struct OpenAiExtra {
    /// 频率惩罚，范围 -2.0 ~ 2.0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// 存在惩罚，范围 -2.0 ~ 2.0。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// 工具选择策略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

/// OpenAI 预存调用参数。
#[derive(Clone, Debug)]
pub struct OpenAIArgs {
    /// 使用模型的 id。
    pub model: String,

    /// 温度参数，内部用 0-200 的 u8 表示 0.0-2.0，序列化时自动除 100。
    pub temperature: u8,

    /// top-p 采样参数，内部用 0-100 的 u8 表示 0.0-1.0，序列化时自动除 100。
    pub top_p: u8,

    /// 频率惩罚，范围 -2.0 ~ 2.0。
    pub frequency_penalty: Option<f32>,

    /// 存在惩罚，范围 -2.0 ~ 2.0。
    pub presence_penalty: Option<f32>,

    /// 工具列表。
    pub tools: Vec<Tool>,

    /// 工具选择策略。
    pub tool_choice: Option<ToolChoice>,
}

impl Default for OpenAIArgs {
    fn default() -> Self {
        Self {
            model: String::new(),
            temperature: 100,
            top_p: 100,
            frequency_penalty: None,
            presence_penalty: None,
            tools: Vec::new(),
            tool_choice: None,
        }
    }
}

impl OpenAIArgs {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn temperature(mut self, temperature: u8) -> Self {
        self.temperature = temperature;
        self
    }

    pub fn top_p(mut self, top_p: u8) -> Self {
        self.top_p = top_p;
        self
    }

    pub fn frequency_penalty(mut self, frequency_penalty: f32) -> Self {
        self.frequency_penalty = Some(frequency_penalty);
        self
    }

    pub fn presence_penalty(mut self, presence_penalty: f32) -> Self {
        self.presence_penalty = Some(presence_penalty);
        self
    }

    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    pub fn tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }
}

impl ChatArgs for OpenAIArgs {
    type Extra = OpenAiExtra;

    fn default_base_url() -> &'static str {
        DEFAULT_BASE_URL
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn build_request(&self, messages: Vec<OpenAIMessage>) -> ChatRequest<OpenAiExtra> {
        ChatRequest {
            messages,
            model: self.model.clone(),
            response_format: None,
            stream: false,
            temperature: self.temperature,
            top_p: self.top_p,
            tools: self.tools.clone(),
            user_id: None,
            extra: OpenAiExtra {
                frequency_penalty: self.frequency_penalty,
                presence_penalty: self.presence_penalty,
                tool_choice: self.tool_choice.clone(),
            },
        }
    }
}
