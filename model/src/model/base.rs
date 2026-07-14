use serde::{Deserialize, Serialize};

/// 统一错误类型。
///
/// 目前区分底层 HTTP/传输错误与“响应结构异常”（例如没有任何 choice），
/// 避免把空回复静默当成成功。
#[derive(Debug)]
pub enum Error {
    /// 底层 reqwest 传输 / HTTP 状态错误。
    Http(reqwest::Error),

    /// 响应里没有任何 choice，无法取出回复内容。
    EmptyResponse,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "http error: {e}"),
            Error::EmptyResponse => write!(f, "response contained no choices"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::EmptyResponse => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// 模型适配器统一 trait。
///
/// 用关联类型 `Message` 区分不同接口所接受的消息格式：
/// OpenAI 兼容接口使用 [`OpenAIMessage`]，Anthropic 接口使用 [`AnthropicMessage`]。
pub trait Model {
    /// 该实现所接受的消息格式。
    type Message: Message;

    /// 非流式 chat。
    fn chat(
        &self,
        messages: Vec<Self::Message>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<String, Error>> + Send;

    /// 拉取可用模型列表，写入自身 `model_list`。
    fn get_models(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}

/// 通用消息 trait。
///
/// 所有 vendor 的请求消息都需要实现这个 trait，保证可以用统一方式构造。
pub trait Message {
    fn role(&self) -> &str;
    fn content(&self) -> &str;
    fn new(role: impl Into<String>, content: impl Into<String>) -> Self;
}

/// OpenAI / OpenAI 兼容格式消息。
///
/// 结构为 `{ role, content }`，适用于 OpenAI、DeepSeek 等兼容接口。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: String,
}

impl Message for OpenAIMessage {
    fn role(&self) -> &str {
        &self.role
    }

    fn content(&self) -> &str {
        &self.content
    }

    fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// Anthropic Messages API 消息。
///
/// Anthropic 的消息 content 是 block 数组，例如 `[{ type: "text", text: "..." }]`。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<AnthropicContentBlock>,
}

/// Anthropic content block。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
}

impl Message for AnthropicMessage {
    fn role(&self) -> &str {
        &self.role
    }

    fn content(&self) -> &str {
        match self.content.first() {
            Some(AnthropicContentBlock::Text { text }) => text,
            None => "",
        }
    }

    fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: vec![AnthropicContentBlock::Text {
                text: content.into(),
            }],
        }
    }
}
