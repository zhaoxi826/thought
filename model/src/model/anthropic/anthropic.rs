//! Anthropic Messages API 适配。
//!
//! 与 OpenAI 兼容接口不同：走 `/v1/messages`，鉴权用 `x-api-key` +
//! `anthropic-version` 头，`system` 提示是顶层字段，消息 content 是 block 数组。
//! 因此不复用 OpenAI 协议的客户端，自带一套线协议，但实现同一个 [`Model`] trait
//! （`type Message = AnthropicMessage`）。

use crate::model::base::{self, percent, AnthropicMessage, Error, Message, Model};
use crate::model::sse::{sse_events, SseEvent};
use crate::model::tool::Function;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_VERSION: &str = "2023-06-01";
pub const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Anthropic 模型客户端。
pub struct AnthropicModel {
    base_url: String,
    api_key: String,
    version: String,
    pub model_list: Vec<String>,
    client: Client,
    pub args: AnthropicArgs,
}

impl AnthropicModel {
    /// 创建客户端。`base_url` 为空时回退到默认 URL；否则去掉末尾 `/`。
    pub fn new(base_url: &str, api_key: &str, args: AnthropicArgs) -> Self {
        let base_url = if base_url.is_empty() {
            ANTHROPIC_BASE_URL.to_owned()
        } else {
            base_url.trim_end_matches('/').to_owned()
        };
        Self {
            base_url,
            api_key: api_key.to_owned(),
            version: DEFAULT_VERSION.to_owned(),
            model_list: Vec::new(),
            client: Client::new(),
            args,
        }
    }

    /// 覆盖 `anthropic-version` 头，默认 [`DEFAULT_VERSION`]。
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }
}

impl AnthropicModel {
    /// 构造 `/v1/messages` 请求体。
    ///
    /// role == "system" 的消息提取合并进顶层 system 字段（拼在 `args.system` 之后），
    /// 其余消息原样保留；`stream` 由调用方决定。
    fn build_request(
        &self,
        messages: Vec<AnthropicMessage>,
        user_id: Option<&str>,
        stream: bool,
    ) -> MessagesRequest<'_> {
        let mut system = self.args.system.clone();
        let mut chat_messages = Vec::with_capacity(messages.len());
        for m in messages {
            if m.role() == "system" {
                let text = m.content().to_owned();
                system = Some(match system {
                    Some(prev) if !prev.is_empty() => format!("{prev}\n{text}"),
                    _ => text,
                });
            } else {
                chat_messages.push(m);
            }
        }

        MessagesRequest {
            model: &self.args.model,
            max_tokens: self.args.max_tokens,
            messages: chat_messages,
            system,
            temperature: self.args.temperature,
            top_p: self.args.top_p,
            stream,
            tools: self
                .args
                .tools
                .iter()
                .cloned()
                .map(AnthropicTool::from)
                .collect(),
            metadata: user_id.map(|id| Metadata {
                user_id: id.to_owned(),
            }),
        }
    }

    /// 发送请求并返回原始响应。
    async fn send(&self, request: &MessagesRequest<'_>) -> Result<reqwest::Response, Error> {
        Ok(self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await?
            .error_for_status()?)
    }
}

impl Model for AnthropicModel {
    type Message = AnthropicMessage;

    async fn chat(
        &self,
        messages: Vec<AnthropicMessage>,
        user_id: Option<&str>,
        // Anthropic 没有 OpenAI 式的 response_format，结构化输出走工具，此处忽略。
        _response_format: Option<&str>,
    ) -> Result<base::ChatOutput, Error> {
        let request = self.build_request(messages, user_id, false);
        let response = self.send(&request).await?.json::<MessagesResponse>().await?;

        let mut texts = Vec::new();
        let mut tool_calls = Vec::new();
        for block in response.content {
            match block {
                ResponseContentBlock::Text { text } => texts.push(text),
                ResponseContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(base::ToolCall {
                        id,
                        name,
                        arguments: input.to_string(),
                    });
                }
                ResponseContentBlock::Other => {}
            }
        }
        if texts.is_empty() && tool_calls.is_empty() {
            return Err(Error::EmptyResponse);
        }

        Ok(base::ChatOutput {
            content: if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            },
            reasoning: None,
            tool_calls,
            finish_reason: response.stop_reason.unwrap_or_default(),
            usage: base::Usage {
                prompt_tokens: response.usage.input_tokens,
                completion_tokens: response.usage.output_tokens,
                total_tokens: response.usage.input_tokens + response.usage.output_tokens,
            },
        })
    }

    async fn chat_stream(
        &self,
        messages: Vec<AnthropicMessage>,
        user_id: Option<&str>,
        _response_format: Option<&str>,
    ) -> Result<base::ChatStream, Error> {
        let request = self.build_request(messages, user_id, true);
        let response = self.send(&request).await?;

        let chunks = sse_events(response.bytes_stream()).filter_map(|event| async move {
            match event {
                Ok(event) => map_stream_event(event),
                Err(e) => Some(Err(e)),
            }
        });
        Ok(Box::pin(chunks))
    }

    async fn get_models(&mut self) -> Result<(), Error> {
        self.model_list = self
            .client
            .get(format!("{}/v1/models", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json::<ModelsResponse>()
            .await?
            .data
            .into_iter()
            .map(|item| item.id)
            .collect();
        Ok(())
    }
}

/// Anthropic 请求里的工具条目（wire 类型）。
///
/// 由协议中立的 [`Function`] 转换而来，序列化为 Anthropic 格式（无 `type/function`
/// 外层包裹）：`{ name, description, input_schema: {JSON Schema} }`。
#[derive(Serialize, Debug, Clone)]
pub struct AnthropicTool {
    name: String,
    description: String,
    /// 参数的 JSON Schema 对象，由 [`Function::schema_object`] 生成。
    input_schema: serde_json::Value,
}

impl From<Function> for AnthropicTool {
    fn from(f: Function) -> Self {
        Self {
            input_schema: f.schema_object(),
            name: f.name,
            description: f.description,
        }
    }
}

/// Anthropic 预存调用参数。
#[derive(Clone, Debug)]
pub struct AnthropicArgs {
    /// 使用模型的 id。
    pub model: String,

    /// 采样上限 token 数，Anthropic 必填。
    pub max_tokens: u32,

    /// 顶层 system 提示，可选。
    pub system: Option<String>,

    /// 温度参数，内部用 0-200 的 u8 表示 0.0-2.0，序列化时自动除 100。
    pub temperature: u8,

    /// top-p 采样参数，内部用 0-100 的 u8 表示 0.0-1.0，序列化时自动除 100。
    pub top_p: u8,

    /// 工具列表（协议中立的 [`Function`]，构造请求时转成 wire 类型）。
    pub tools: Vec<Function>,
}

impl Default for AnthropicArgs {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system: None,
            temperature: 100,
            top_p: 100,
            tools: Vec::new(),
        }
    }
}

impl AnthropicArgs {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
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

    pub fn tools(mut self, tools: Vec<Function>) -> Self {
        self.tools = tools;
        self
    }
}

// `/v1/messages` 请求结构

#[derive(Serialize, Debug)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,

    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,

    #[serde(with = "percent")]
    temperature: u8,

    #[serde(with = "percent")]
    top_p: u8,

    /// 流式开关；非流式请求不携带该字段。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Metadata>,
}

#[derive(Serialize, Debug)]
struct Metadata {
    user_id: String,
}

// `/v1/messages` 响应结构

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct MessagesResponse {
    id: String,
    model: String,
    role: String,
    content: Vec<ResponseContentBlock>,
    stop_reason: Option<String>,
    usage: Usage,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ResponseContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    /// 工具调用块：`input` 为参数 JSON 对象。
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// 未知或 vendor 扩展 block（如 DeepSeek 的 `thinking`），保留原始 type 字段。
    #[serde(other)]
    Other,
}

// `/v1/messages` 流式响应（SSE 事件）结构
//
// Anthropic 流式按事件类型分发，这里只消费产出增量的三类：
// `content_block_start`（tool_use 开始）、`content_block_delta`（文本/参数/思考碎片）、
// `message_delta`（结束原因）。`message_start` / `content_block_stop` /
// `message_stop` / `ping` 等不产出增量，直接忽略。

#[derive(Deserialize, Debug)]
struct ContentBlockStart {
    index: u32,
    content_block: ContentBlockStartBody,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ContentBlockStartBody {
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct ContentBlockDelta {
    index: u32,
    delta: ContentBlockDeltaBody,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
enum ContentBlockDeltaBody {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Debug)]
struct MessageDelta {
    delta: MessageDeltaBody,
}

#[derive(Deserialize, Debug)]
struct MessageDeltaBody {
    stop_reason: Option<String>,
}

/// 解析流式事件体；失败包成 [`Error::Protocol`]。
fn parse_event<T: serde::de::DeserializeOwned>(data: &str) -> Result<T, Error> {
    serde_json::from_str(data)
        .map_err(|e| Error::Protocol(format!("invalid stream event {data:?}: {e}")))
}

/// 把一条 SSE 事件映射为中立 [`ChatChunk`]；不产出增量的事件返回 `None`。
fn map_stream_event(event: SseEvent) -> Option<Result<base::ChatChunk, Error>> {
    match event.event.as_deref() {
        Some("content_block_start") => match parse_event(&event.data) {
            Ok(ContentBlockStart {
                index,
                content_block: ContentBlockStartBody::ToolUse { id, name },
            }) => Some(Ok(base::ChatChunk {
                tool_calls: vec![base::ToolCallDelta {
                    index,
                    id: Some(id),
                    name: Some(name),
                    arguments: None,
                }],
                ..Default::default()
            })),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        },
        Some("content_block_delta") => match parse_event(&event.data) {
            Ok(ContentBlockDelta { index, delta }) => {
                let chunk = match delta {
                    ContentBlockDeltaBody::Text { text } => base::ChatChunk {
                        content: Some(text),
                        ..Default::default()
                    },
                    ContentBlockDeltaBody::InputJson { partial_json } => base::ChatChunk {
                        tool_calls: vec![base::ToolCallDelta {
                            index,
                            arguments: Some(partial_json),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ContentBlockDeltaBody::Thinking { thinking } => base::ChatChunk {
                        reasoning: Some(thinking),
                        ..Default::default()
                    },
                    ContentBlockDeltaBody::Other => return None,
                };
                Some(Ok(chunk))
            }
            Err(e) => Some(Err(e)),
        },
        Some("message_delta") => match parse_event::<MessageDelta>(&event.data) {
            Ok(MessageDelta { delta }) => delta.stop_reason.map(|reason| {
                Ok(base::ChatChunk {
                    finish_reason: Some(reason),
                    ..Default::default()
                })
            }),
            Err(e) => Some(Err(e)),
        },
        // message_start / content_block_stop / message_stop / ping / 未知事件
        _ => None,
    }
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

// `/v1/models` 响应结构

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct ModelsResponse {
    data: Vec<ModelItem>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct ModelItem {
    id: String,
    display_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 响应反序列化 ----

    #[test]
    fn messages_response_parses_text_and_tool_use_blocks() {
        let body = r#"{
            "id":"msg_1","model":"claude","role":"assistant",
            "content":[
                {"type":"text","text":"let me read that"},
                {"type":"tool_use","id":"toolu_1","name":"read_file",
                 "input":{"file_path":"/tmp/a"}},
                {"type":"thinking","thinking":"hmm"}
            ],
            "stop_reason":"tool_use",
            "usage":{"input_tokens":10,"output_tokens":5}
        }"#;
        let response: MessagesResponse = serde_json::from_str(body).unwrap();
        assert_eq!(response.content.len(), 3);
        assert_eq!(response.stop_reason.as_deref(), Some("tool_use"));
        match &response.content[1] {
            ResponseContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "read_file");
                assert_eq!(input["file_path"], serde_json::json!("/tmp/a"));
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
        assert!(matches!(response.content[2], ResponseContentBlock::Other));
    }

    // ---- 请求构造 ----

    #[test]
    fn build_request_extracts_system_messages() {
        let model = AnthropicModel::new(
            "http://localhost",
            "key",
            AnthropicArgs::new("claude").system("be nice"),
        );
        let messages = vec![
            AnthropicMessage::new("system", "be brief"),
            AnthropicMessage::new("user", "hi"),
        ];
        let request = model.build_request(messages, Some("user-1"), false);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["system"], serde_json::json!("be nice\nbe brief"));
        assert_eq!(json["messages"], serde_json::json!([{"role":"user","content":[{"type":"text","text":"hi"}]}]));
        assert_eq!(json["metadata"], serde_json::json!({"user_id":"user-1"}));
        // 非流式不携带 stream 字段
        assert!(json.get("stream").is_none());
    }

    #[test]
    fn build_request_includes_stream_flag_when_streaming() {
        let model = AnthropicModel::new("http://localhost", "key", AnthropicArgs::new("claude"));
        let request = model.build_request(vec![], None, true);
        assert_eq!(serde_json::to_value(&request).unwrap()["stream"], serde_json::json!(true));
    }

    // ---- 流式事件映射 ----

    fn event(event_type: &str, data: &str) -> SseEvent {
        SseEvent {
            event: Some(event_type.into()),
            data: data.into(),
        }
    }

    #[test]
    fn maps_content_block_start_tool_use() {
        let chunk = map_stream_event(event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,
                "content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        let delta = &chunk.tool_calls[0];
        assert_eq!(delta.index, 1);
        assert_eq!(delta.id.as_deref(), Some("toolu_1"));
        assert_eq!(delta.name.as_deref(), Some("read_file"));
        assert_eq!(delta.arguments, None);
    }

    #[test]
    fn maps_deltas() {
        let chunk = map_stream_event(event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"hello"}}"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(chunk.content.as_deref(), Some("hello"));

        let chunk = map_stream_event(event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"file"}}"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(chunk.tool_calls[0].index, 1);
        assert_eq!(chunk.tool_calls[0].arguments.as_deref(), Some("{\"file"));

        let chunk = map_stream_event(event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,
                "delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(chunk.reasoning.as_deref(), Some("hmm"));
    }

    #[test]
    fn maps_message_delta_stop_reason_and_ignores_other_events() {
        let chunk = map_stream_event(event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(chunk.finish_reason.as_deref(), Some("end_turn"));

        for ignored in ["message_start", "content_block_stop", "message_stop", "ping"] {
            assert!(
                map_stream_event(event(ignored, "{}")).is_none(),
                "{ignored} should produce no chunk"
            );
        }
        // 无 event 行的未知事件同样忽略
        assert!(map_stream_event(SseEvent { event: None, data: "{}".into() }).is_none());
    }

    #[test]
    fn malformed_event_data_is_protocol_error() {
        match map_stream_event(event("content_block_delta", "not json")) {
            Some(Err(Error::Protocol(_))) => {}
            other => panic!("expected protocol error, got {other:?}"),
        }
    }
}
