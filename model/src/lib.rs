pub mod model;

// 统一模型接口
pub use model::base::{
    AnthropicMessage, Error, Message, Model as ModelTrait, OpenAIMessage,
};

// OpenAI Chat Completions（DeepSeek 为其预设）
pub use model::openai::{
    OpenAiArgs, OpenAiModel, OpenAiVendor, Thinking, ThinkingConfig, ThinkingType,
    DEEPSEEK_BASE_URL, OPENAI_BASE_URL,
};

// Anthropic Messages
pub use model::anthropic::{AnthropicArgs, AnthropicModel, ANTHROPIC_BASE_URL};

/// 顶层模型枚举，按协议统一封装两大接口。
///
/// 只有两种协议：OpenAI Chat Completions（DeepSeek 是它的预设）与
/// Anthropic Messages。`chat` 以中立的 `{role, content}`（[`OpenAIMessage`]）
/// 收参，Anthropic 分支内部转换为 content block。
pub enum Model {
    OpenAi(OpenAiModel),
    Anthropic(AnthropicModel),
}

impl Model {
    /// 非流式 chat，统一透传给底层实现。使用的模型 id 取自各自的 `args`。
    pub async fn chat(
        &self,
        messages: Vec<OpenAIMessage>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<String, Error> {
        match self {
            Model::OpenAi(inner) => inner.chat(messages, user_id, response_format).await,
            Model::Anthropic(inner) => {
                let converted = messages
                    .into_iter()
                    .map(|m| AnthropicMessage::new(m.role, m.content))
                    .collect();
                inner.chat(converted, user_id, response_format).await
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
}
