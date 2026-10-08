use serde::{Deserialize, Serialize};

/// 将 u8 以“百分之一”的定点数形式与 JSON 浮点数互转（跨协议共享）。
///
/// 内部始终是 u8（0-200 表示 0.0-2.0），序列化时直接用整数运算拼出十进制字符串
/// 再作为 JSON number 输出，全程不经过 f32/f64，既无浮点误差也无精度损失。
/// 例如 100 → `1`，70 → `0.7`，150 → `1.5`。
pub mod percent {
    use serde::de::{Error, Unexpected};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::value::RawValue;

    /// 把 `value`（百分之一单位）拼成十进制字符串，如 70 → "0.7"、100 → "1"。
    fn to_decimal_string(value: u8) -> String {
        let int = value / 100;
        let frac = value % 100;
        if frac == 0 {
            int.to_string()
        } else if frac.is_multiple_of(10) {
            format!("{int}.{}", frac / 10)
        } else {
            format!("{int}.{frac:02}")
        }
    }

    pub fn serialize<S: Serializer>(value: &u8, serializer: S) -> Result<S::Ok, S::Error> {
        // RawValue 让拼好的十进制字符串原样作为 JSON number 输出，不再走浮点。
        let raw =
            RawValue::from_string(to_decimal_string(*value)).map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
        // 反序列化仅在读回请求体时用到；用 f64 解析后换算回 u8。
        let v = f64::deserialize(deserializer)?;
        if !(0.0..=2.0).contains(&v) {
            return Err(Error::invalid_value(
                Unexpected::Float(v),
                &"a number in 0.0..=2.0",
            ));
        }
        Ok((v * 100.0).round() as u8)
    }
}

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
    /// 响应数据无法按协议解析（例如流式事件体损坏）。
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "http error: {e}"),
            Error::EmptyResponse => write!(f, "response contained no choices"),
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::EmptyResponse | Error::Protocol(_) => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// 一次非流式 chat 的结构化结果（协议中立）。
#[derive(Debug, Clone, Default)]
pub struct ChatOutput {
    /// 文本内容；模型只回工具调用、没有文本时为 `None`。
    pub content: Option<String>,

    /// 推理内容（DeepSeek 思考模式的 `reasoning_content` 等）。
    pub reasoning: Option<String>,

    /// 模型请求的工具调用。
    pub tool_calls: Vec<ToolCall>,

    /// 结束原因，协议原生字符串（OpenAI `stop` / `tool_calls`，
    /// Anthropic `end_turn` / `tool_use` / `max_tokens` 等）。
    pub finish_reason: String,

    /// token 用量。
    pub usage: Usage,
}

/// token 用量（协议中立；Anthropic 的 input/output 映射为 prompt/completion）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 一次完整的工具调用（协议中立）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// 调用 id，回传工具结果时用它对应（OpenAI 的 tool_call_id / Anthropic 的 tool_use_id）。
    pub id: String,

    /// 工具名。
    pub name: String,

    /// 原始 JSON 参数串。
    pub arguments: String,
}

/// 流式工具调用增量。
///
/// 同一 `index` 的片段属于同一次调用：`id` / `name` 只在首个片段出现，
/// `arguments` 是 JSON 参数串的碎片，需要调用方按序拼接。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// 流式 chunk（协议中立）。
///
/// 每个字段都是增量：`content` / `reasoning` 为文本碎片，`tool_calls` 为工具调用
/// 碎片（见 [`ToolCallDelta`]），`finish_reason` 只在最后一帧出现。
#[derive(Debug, Clone, Default)]
pub struct ChatChunk {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
}

/// 流式 chat 的 chunk 流。
pub type ChatStream = futures::stream::BoxStream<'static, Result<ChatChunk, Error>>;

/// 模型适配器统一 trait。
///
/// 用关联类型 `Message` 区分不同接口所接受的消息格式：
/// OpenAI 兼容接口使用 [`OpenAIMessage`]，Anthropic 接口使用 [`AnthropicMessage`]。
pub trait Model {
    /// 该实现所接受的消息格式。
    type Message: Message;

    /// 非流式 chat，返回结构化结果（文本 + 工具调用 + 结束原因 + 用量）。
    fn chat(
        &self,
        messages: Vec<Self::Message>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ChatOutput, Error>> + Send;

    /// 流式 chat：建立连接后返回 chunk 流，逐帧产出增量。
    fn chat_stream(
        &self,
        messages: Vec<Self::Message>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ChatStream, Error>> + Send;

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

#[cfg(test)]
mod tests {
    use super::percent;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct WithPercent {
        #[serde(with = "percent")]
        value: u8,
    }

    #[test]
    fn percent_serialize_decimal_string() {
        let cases = [(0, "0"), (5, "0.05"), (70, "0.7"), (100, "1"), (150, "1.5")];
        for (input, expected) in cases {
            let json = serde_json::to_string(&WithPercent { value: input }).unwrap();
            assert_eq!(json, format!("{{\"value\":{expected}}}"), "input {input}");
        }
    }

    #[test]
    fn percent_deserialize_roundtrip() {
        let w: WithPercent = serde_json::from_str("{\"value\":0.7}").unwrap();
        assert_eq!(w.value, 70);
        let w: WithPercent = serde_json::from_str("{\"value\":1}").unwrap();
        assert_eq!(w.value, 100);
    }

    #[test]
    fn percent_deserialize_out_of_range() {
        assert!(serde_json::from_str::<WithPercent>("{\"value\":2.5}").is_err());
        assert!(serde_json::from_str::<WithPercent>("{\"value\":-0.1}").is_err());
    }
}
