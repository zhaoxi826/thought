pub mod model;

// 统一模型接口
pub use model::base::{
    collect_stream, ChatChunk, ChatOutput, ChatStream, Error, Message, Model as ModelTrait,
    StreamAccumulator, ToolCall, ToolCallDelta, Usage,
};

// 协议中立的工具定义
pub use model::tool::{Function, Parameter, ParameterType};

// OpenAI Chat Completions（DeepSeek 为其预设）
pub use model::openai::{
    OpenAiArgs, OpenAiModel, OpenAiVendor, OpenaiTool, ResponseFormat, Thinking, ThinkingConfig,
    ThinkingType, ToolChoice, DEEPSEEK_BASE_URL, OPENAI_BASE_URL,
};

// Anthropic Messages
pub use model::anthropic::{AnthropicArgs, AnthropicModel, AnthropicTool, ANTHROPIC_BASE_URL};

/// 顶层模型枚举，按协议统一封装两大接口。
///
/// 只有两种协议：OpenAI Chat Completions（DeepSeek 是它的预设）与
/// Anthropic Messages。两者都收同一份中立的 [`Message`] 切片。
/// `Clone` 廉价，可给每个 agent 各持一份再分别 [`set_tools`](Model::set_tools)。
#[derive(Clone)]
pub enum Model {
    OpenAi(OpenAiModel),
    Anthropic(AnthropicModel),
}

impl Model {
    /// 非流式 chat，返回结构化结果。使用的模型 id 取自各自的 `args`。
    pub async fn chat(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<ChatOutput, Error> {
        match self {
            Model::OpenAi(inner) => inner.chat(messages, user_id, response_format).await,
            Model::Anthropic(inner) => inner.chat(messages, user_id, response_format).await,
        }
    }

    /// 流式 chat：建立连接后返回 chunk 流，逐帧产出增量。
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<ChatStream, Error> {
        match self {
            Model::OpenAi(inner) => inner.chat_stream(messages, user_id, response_format).await,
            Model::Anthropic(inner) => inner.chat_stream(messages, user_id, response_format).await,
        }
    }

    /// 拉取可用模型列表。
    pub async fn get_models(&mut self) -> Result<(), Error> {
        match self {
            Model::OpenAi(inner) => inner.get_models().await,
            Model::Anthropic(inner) => inner.get_models().await,
        }
    }

    /// 已拉取的模型列表（先调 [`get_models`](Model::get_models)）。
    pub fn model_list(&self) -> &[String] {
        match self {
            Model::OpenAi(inner) => &inner.model_list,
            Model::Anthropic(inner) => &inner.model_list,
        }
    }

    /// 覆盖可用工具列表（协议中立的 [`Function`]），立即翻译成各协议的
    /// wire 格式并缓存，之后每轮请求只借用。
    pub fn set_tools(&mut self, tools: &[Function]) {
        match self {
            Model::OpenAi(inner) => inner.args.set_tools(tools),
            Model::Anthropic(inner) => inner.args.set_tools(tools),
        }
    }
}
