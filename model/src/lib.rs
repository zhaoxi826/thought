pub mod model;

// 统一模型接口
pub use model::base::{AnthropicMessage, Error, Message, Model as ModelTrait, OpenAIMessage};

// DeepSeek
pub use model::deepseek::{DeepSeekModel, DeepseekArgs};

// OpenAI
pub use model::openai::{OpenAIArgs, OpenAIModel};

/// 顶层模型枚举，统一封装 DeepSeek 和 OpenAI 实现。
///
/// 两者都是 OpenAI 兼容接口，接受 [`OpenAIMessage`]。
pub enum Model {
    Deepseek(DeepSeekModel),
    OpenAI(OpenAIModel),
}

impl Model {
    /// 非流式 chat，统一透传给底层模型实现。
    ///
    /// 使用的模型 id 取自各实现预存的 `args`。
    pub async fn chat(
        &self,
        messages: Vec<OpenAIMessage>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<String, Error> {
        match self {
            Model::Deepseek(inner) => inner.chat(messages, user_id, response_format).await,
            Model::OpenAI(inner) => inner.chat(messages, user_id, response_format).await,
        }
    }

    /// 拉取可用模型列表。
    pub async fn get_models(&mut self) -> Result<(), Error> {
        match self {
            Model::Deepseek(inner) => inner.get_models().await,
            Model::OpenAI(inner) => inner.get_models().await,
        }
    }
}
