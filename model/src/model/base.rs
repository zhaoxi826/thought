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
/// 区分底层传输错误、服务端返回的错误状态（保留响应体便于排查）与
/// “响应结构异常”，避免把空回复静默当成成功。
#[derive(Debug)]
pub enum Error {
    /// 底层 reqwest 传输错误。
    Http(reqwest::Error),

    /// 服务端返回非 2xx 状态；`body` 为原始响应体（通常是厂商的错误 JSON）。
    Api { status: u16, body: String },

    /// 响应里没有任何可用内容。
    EmptyResponse,

    /// 响应数据无法按协议解析（例如流式事件体损坏）。
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "http error: {e}"),
            Error::Api { status, body } => write!(f, "api error {status}: {body}"),
            Error::EmptyResponse => write!(f, "response contained no content"),
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// 检查响应状态：非 2xx 时读出响应体，包成 [`Error::Api`]。
pub(crate) async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(Error::Api {
        status: status.as_u16(),
        body,
    })
}

/// 一次非流式 chat 的结构化结果（协议中立）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

impl Usage {
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }

    /// 合并一份流式用量快照。
    ///
    /// 各协议在流里报告的用量都是“截至当前的累计值”，且可能只带部分字段
    /// （Anthropic `message_start` 只有 input、`message_delta` 只有 output），
    /// 因此逐字段取较大值，`total` 至少为两者之和。
    pub fn merge(&mut self, other: Usage) {
        self.prompt_tokens = self.prompt_tokens.max(other.prompt_tokens);
        self.completion_tokens = self.completion_tokens.max(other.completion_tokens);
        self.total_tokens = self
            .total_tokens
            .max(other.total_tokens)
            .max(self.prompt_tokens + self.completion_tokens);
    }
}

/// 一次完整的工具调用（协议中立）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
/// `arguments` 是 JSON 参数串的碎片，需要按序拼接（见 [`StreamAccumulator`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// 流式 chunk（协议中立）。
///
/// `content` / `reasoning` / `tool_calls` 为增量碎片；`finish_reason` 只在结束帧
/// 出现；`usage` 为截至当前的累计用量快照（可能只含部分字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatChunk {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

/// 流式 chat 的 chunk 流。
pub type ChatStream = futures::stream::BoxStream<'static, Result<ChatChunk, Error>>;

/// 流式累加器：把一串 [`ChatChunk`] 拼回完整的 [`ChatOutput`]。
///
/// 调用方可以边转发 chunk（如推给前端）边 [`push`](Self::push)，流结束后
/// [`finish`](Self::finish) 得到与非流式 `chat` 同构的结果。工具调用按 `index`
/// 归并，与碎片到达顺序无关。
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: std::collections::BTreeMap<u32, PartialToolCall>,
    finish_reason: Option<String>,
    usage: Usage,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 并入一个 chunk。
    pub fn push(&mut self, chunk: &ChatChunk) {
        if let Some(text) = &chunk.content {
            self.content.push_str(text);
        }
        if let Some(text) = &chunk.reasoning {
            self.reasoning.push_str(text);
        }
        for delta in &chunk.tool_calls {
            let call = self.tool_calls.entry(delta.index).or_default();
            if let Some(id) = &delta.id {
                call.id.clone_from(id);
            }
            if let Some(name) = &delta.name {
                call.name.clone_from(name);
            }
            if let Some(args) = &delta.arguments {
                call.arguments.push_str(args);
            }
        }
        if let Some(reason) = &chunk.finish_reason {
            self.finish_reason = Some(reason.clone());
        }
        if let Some(usage) = chunk.usage {
            self.usage.merge(usage);
        }
    }

    /// 结束累加，产出完整结果。
    ///
    /// 没有任何文本也没有工具调用时返回 [`Error::EmptyResponse`]；工具调用缺少
    /// 名字视为协议错误。空参数串规范化为 `{}`。
    pub fn finish(self) -> Result<ChatOutput, Error> {
        let mut tool_calls = Vec::with_capacity(self.tool_calls.len());
        for (index, call) in self.tool_calls {
            if call.name.is_empty() {
                return Err(Error::Protocol(format!("tool call #{index} has no name")));
            }
            tool_calls.push(ToolCall {
                id: call.id,
                name: call.name,
                arguments: if call.arguments.is_empty() {
                    "{}".to_owned()
                } else {
                    call.arguments
                },
            });
        }
        if self.content.is_empty() && tool_calls.is_empty() {
            return Err(Error::EmptyResponse);
        }
        Ok(ChatOutput {
            content: non_empty(self.content),
            reasoning: non_empty(self.reasoning),
            tool_calls,
            finish_reason: self.finish_reason.unwrap_or_default(),
            usage: self.usage,
        })
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// 把整条流消费完并拼成 [`ChatOutput`]；`on_chunk` 在每个 chunk 到达时回调
/// （用于实时转发），不需要时传 `|_| {}`。
pub async fn collect_stream(
    mut stream: ChatStream,
    mut on_chunk: impl FnMut(&ChatChunk),
) -> Result<ChatOutput, Error> {
    use futures::StreamExt;

    let mut acc = StreamAccumulator::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        on_chunk(&chunk);
        acc.push(&chunk);
    }
    acc.finish()
}

/// 协议中立的对话消息。
///
/// 覆盖完整的工具调用回合：assistant 发起 `tool_calls`，随后每个调用对应一条
/// [`Message::Tool`] 回传结果。各协议在构造请求时把它翻译成自己的 wire 格式
/// （OpenAI 的 `tool` 角色 / Anthropic 的 `tool_use` + `tool_result` block），
/// 序列化时只借用、不拷贝历史。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,

        /// 推理内容；DeepSeek 思考模式在工具回合内要求原样回传。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,

        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        /// 对应 [`ToolCall::id`]。
        call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Message::User {
            content: content.into(),
        }
    }

    /// 纯文本的 assistant 回复。
    pub fn assistant(content: impl Into<String>) -> Self {
        Message::Assistant {
            content: Some(content.into()),
            reasoning: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message::Tool {
            call_id: call_id.into(),
            content: content.into(),
        }
    }

    /// 角色名（与 OpenAI wire 一致）。
    pub fn role(&self) -> &'static str {
        match self {
            Message::System { .. } => "system",
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
            Message::Tool { .. } => "tool",
        }
    }

    /// 文本内容；只有工具调用的 assistant 消息返回空串。
    pub fn text(&self) -> &str {
        match self {
            Message::System { content }
            | Message::User { content }
            | Message::Tool { content, .. } => content,
            Message::Assistant { content, .. } => content.as_deref().unwrap_or(""),
        }
    }
}

impl From<ChatOutput> for Message {
    /// 把模型输出转成可回放进历史的 assistant 消息。
    fn from(output: ChatOutput) -> Self {
        Message::Assistant {
            content: output.content,
            reasoning: output.reasoning,
            tool_calls: output.tool_calls,
        }
    }
}

/// 模型适配器统一 trait。
///
/// 所有协议都收同一份中立的 [`Message`] 切片，内部借用翻译成 wire 格式。
pub trait Model {
    /// 非流式 chat，返回结构化结果（文本 + 工具调用 + 结束原因 + 用量）。
    fn chat(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ChatOutput, Error>> + Send;

    /// 流式 chat：建立连接后返回 chunk 流，逐帧产出增量（含累计用量）。
    fn chat_stream(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ChatStream, Error>> + Send;

    /// 拉取可用模型列表，写入自身 `model_list`。
    fn get_models(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;
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

    use super::{ChatChunk, Error, Message, StreamAccumulator, ToolCall, ToolCallDelta, Usage};

    #[test]
    fn usage_merge_takes_partial_snapshots() {
        let mut usage = Usage::default();
        usage.merge(Usage { prompt_tokens: 11, completion_tokens: 1, total_tokens: 0 });
        usage.merge(Usage { prompt_tokens: 0, completion_tokens: 7, total_tokens: 0 });
        assert_eq!(usage, Usage::new(11, 7));
    }

    #[test]
    fn accumulator_merges_text_tool_calls_and_usage() {
        let chunks = [
            ChatChunk { content: Some("he".into()), ..Default::default() },
            ChatChunk { reasoning: Some("think".into()), content: Some("llo".into()), ..Default::default() },
            // 两个工具调用交错到达，index 1 先于 0 出现
            ChatChunk {
                tool_calls: vec![ToolCallDelta { index: 1, id: Some("b".into()), name: Some("g".into()), arguments: None }],
                ..Default::default()
            },
            ChatChunk {
                tool_calls: vec![
                    ToolCallDelta { index: 0, id: Some("a".into()), name: Some("f".into()), arguments: Some("{\"x\"".into()) },
                    ToolCallDelta { index: 0, arguments: Some(":1}".into()), ..Default::default() },
                ],
                ..Default::default()
            },
            ChatChunk {
                finish_reason: Some("tool_calls".into()),
                usage: Some(Usage::new(3, 4)),
                ..Default::default()
            },
        ];
        let mut acc = StreamAccumulator::new();
        for c in &chunks {
            acc.push(c);
        }
        let out = acc.finish().unwrap();
        assert_eq!(out.content.as_deref(), Some("hello"));
        assert_eq!(out.reasoning.as_deref(), Some("think"));
        assert_eq!(out.finish_reason, "tool_calls");
        assert_eq!(out.usage.total_tokens, 7);
        assert_eq!(
            out.tool_calls,
            vec![
                ToolCall { id: "a".into(), name: "f".into(), arguments: "{\"x\":1}".into() },
                // 没有参数碎片的调用规范化为 {}
                ToolCall { id: "b".into(), name: "g".into(), arguments: "{}".into() },
            ]
        );
    }

    #[test]
    fn accumulator_rejects_empty_and_nameless() {
        assert!(matches!(StreamAccumulator::new().finish(), Err(Error::EmptyResponse)));

        let mut acc = StreamAccumulator::new();
        acc.push(&ChatChunk {
            tool_calls: vec![ToolCallDelta { index: 0, arguments: Some("{}".into()), ..Default::default() }],
            ..Default::default()
        });
        assert!(matches!(acc.finish(), Err(Error::Protocol(_))));
    }

    #[test]
    fn message_serde_roundtrip_is_role_tagged() {
        let msg = Message::Assistant {
            content: None,
            reasoning: None,
            tool_calls: vec![ToolCall { id: "1".into(), name: "f".into(), arguments: "{}".into() }],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "assistant");
        assert!(json.get("content").is_none());
        assert_eq!(serde_json::from_value::<Message>(json).unwrap(), msg);
        assert_eq!(Message::tool("1", "ok").role(), "tool");
        assert_eq!(msg.text(), "");
    }
}
