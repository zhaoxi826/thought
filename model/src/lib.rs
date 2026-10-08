pub mod model;

// 统一模型接口
pub use model::base::{
    AnthropicMessage, ChatChunk, ChatOutput, ChatStream, Error, Message, Model as ModelTrait,
    OpenAIMessage, ToolCall, ToolCallDelta, Usage,
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
/// Anthropic Messages。`chat` / `chat_stream` 以中立的 `{role, content}`
/// （[`OpenAIMessage`]）收参，Anthropic 分支内部转换为 content block。
pub enum Model {
    OpenAi(OpenAiModel),
    Anthropic(AnthropicModel),
}

impl Model {
    /// 非流式 chat，返回结构化结果。使用的模型 id 取自各自的 `args`。
    pub async fn chat(
        &self,
        messages: Vec<OpenAIMessage>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<ChatOutput, Error> {
        match self {
            Model::OpenAi(inner) => inner.chat(messages, user_id, response_format).await,
            Model::Anthropic(inner) => {
                inner
                    .chat(convert_messages(messages), user_id, response_format)
                    .await
            }
        }
    }

    /// 流式 chat：建立连接后返回 chunk 流，逐帧产出增量。
    pub async fn chat_stream(
        &self,
        messages: Vec<OpenAIMessage>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<ChatStream, Error> {
        match self {
            Model::OpenAi(inner) => inner.chat_stream(messages, user_id, response_format).await,
            Model::Anthropic(inner) => {
                inner
                    .chat_stream(convert_messages(messages), user_id, response_format)
                    .await
            }
        }
    }

    /// 拉取可用模型列表。
    pub async fn get_models(&mut self) -> Result<(), Error> {
        match self {
            Model::OpenAi(inner) => inner.get_models().await,
            Model::Anthropic(inner) => inner.get_models().await,
        }
    }

    /// 覆盖可用工具列表（协议中立的 [`Function`]），构造请求时各协议
    /// 自行翻译成自己的 wire 格式。
    pub fn set_tools(&mut self, tools: Vec<Function>) {
        match self {
            Model::OpenAi(inner) => inner.args.tools = tools,
            Model::Anthropic(inner) => inner.args.tools = tools,
        }
    }
}

/// 中立消息 → Anthropic 消息（content 包成单个 text block）。
fn convert_messages(messages: Vec<OpenAIMessage>) -> Vec<AnthropicMessage> {
    messages
        .into_iter()
        .map(|m| AnthropicMessage::new(m.role, m.content))
        .collect()
}
