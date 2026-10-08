//! OpenAI Chat Completions 协议引擎：客户端、线协议（请求/响应/工具）、参数与
//! 原生 OpenAI 预设。
//!
//! 这是 OpenAI 一族的共享实现。DeepSeek 不是独立协议，只是第二层 [`OpenAiVendor`]
//! 选到 `Deepseek` 变体、多带思考字段——其预设在同包的 [`super::deepseek`] 里。

use crate::core::{self as base, check_status, percent, Error, Message, Model};
use crate::providers::sse::{sse_events, SseEvent};
use crate::core::Function;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

/// 含 `/v1` 前缀：请求拼成 `{base}/chat/completions`、`{base}/models`。
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

// `/chat/completions` 请求结构

/// 聊天完成请求体。
///
/// 全部借用自 [`OpenAiArgs`] 与调用方的消息切片，构造请求不拷贝历史与工具定义。
/// 公共字段直接列在这里；vendor 特有的那几个字段由第二层枚举 [`OpenAiVendor`]
/// 承载，`#[serde(flatten)]` 展开到请求顶层。
#[derive(Serialize, Debug)]
pub(crate) struct ChatRequest<'a> {
    messages: Vec<WireMessage<'a>>,

    model: &'a str,

    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,

    stream: bool,

    /// 流式时请求在最后一帧附带用量。
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,

    #[serde(with = "percent")]
    temperature: u8,

    #[serde(with = "percent")]
    top_p: u8,

    #[serde(skip_serializing_if = "no_tools")]
    tools: &'a [OpenaiTool],

    /// 终端用户标识（OpenAI 的 `user` 字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<&'a str>,

    #[serde(flatten)]
    vendor: &'a OpenAiVendor,
}

fn no_tools(tools: &&[OpenaiTool]) -> bool {
    tools.is_empty()
}

#[derive(Serialize, Debug)]
struct StreamOptions {
    include_usage: bool,
}

/// 请求里的一条消息（wire 类型），借用中立 [`Message`] 的字段。
#[derive(Serialize, Debug, PartialEq)]
struct WireMessage<'a> {
    role: &'static str,

    /// assistant 只发起工具调用时为 `null`。
    content: Option<&'a str>,

    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'a str>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall<'a>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Serialize, Debug, PartialEq)]
struct WireToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunctionCall<'a>,
}

#[derive(Serialize, Debug, PartialEq)]
struct WireFunctionCall<'a> {
    name: &'a str,
    arguments: &'a str,
}

/// 中立消息 → OpenAI wire 消息。
///
/// `echo_reasoning`：DeepSeek 思考模式要求工具回合内的 assistant 消息原样带回
/// `reasoning_content`，因此只对“带工具调用的 assistant 消息”回传；纯文本回复的
/// 推理内容不回传（DeepSeek 会忽略或拒绝），原生 OpenAI 一律不回传。
fn wire_messages(messages: &[Message], echo_reasoning: bool) -> Vec<WireMessage<'_>> {
    messages
        .iter()
        .map(|m| {
            fn plain<'a>(role: &'static str, content: &'a str) -> WireMessage<'a> {
                WireMessage {
                    role,
                    content: Some(content),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                }
            }
            match m {
                Message::System { content } => plain("system", content),
                Message::User { content } => plain("user", content),
                Message::Assistant {
                    content,
                    reasoning,
                    tool_calls,
                } => WireMessage {
                    role: "assistant",
                    // 既无文本也无工具调用时仍需给出 content
                    content: match (content.as_deref(), tool_calls.is_empty()) {
                        (None, true) => Some(""),
                        (c, _) => c,
                    },
                    reasoning_content: if echo_reasoning && !tool_calls.is_empty() {
                        reasoning.as_deref()
                    } else {
                        None
                    },
                    tool_calls: tool_calls
                        .iter()
                        .map(|t| WireToolCall {
                            id: &t.id,
                            kind: "function",
                            function: WireFunctionCall {
                                name: &t.name,
                                arguments: &t.arguments,
                            },
                        })
                        .collect(),
                    tool_call_id: None,
                },
                Message::Tool { call_id, content } => WireMessage {
                    tool_call_id: Some(call_id),
                    ..plain("tool", content)
                },
            }
        })
        .collect()
}


/// OpenAI 兼容家族的第二层枚举：同一协议下各 vendor 差异的那几个字段。
///
/// 构造请求（message）时选定具体 vendor，`#[serde(untagged)] + flatten` 使其字段
/// 直接出现在请求体顶层：OpenAI 请求不会出现 `thinking`，DeepSeek 请求也不会出现
/// `frequency_penalty`，从类型上杜绝非法组合。
#[derive(Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum OpenAiVendor {
    /// 原生 OpenAI。
    OpenAi {
        #[serde(skip_serializing_if = "Option::is_none")]
        frequency_penalty: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        presence_penalty: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_choice: Option<ToolChoice>,
    },

    /// DeepSeek（OpenAI 兼容），多出思考相关字段。
    Deepseek {
        thinking: Thinking,
        reasoning_effort: ThinkingConfig,
    },
}

/// 思考模式配置（DeepSeek）。
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

/// 思考强度配置（DeepSeek）。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub enum ThinkingConfig {
    #[serde(rename = "high")]
    #[default]
    High,

    #[serde(rename = "max")]
    Max,
}

/// 响应格式配置。
///
/// wire 格式为 `{"type": "json_object"}`，内部标签枚举直接产出这一结构。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
}

impl ResponseFormat {
    /// 从字符串解析响应格式，未知值回退到 `Text`。
    pub fn parse(fmt: &str) -> Self {
        match fmt {
            "json_object" => ResponseFormat::JsonObject,
            _ => ResponseFormat::Text,
        }
    }
}

/// 工具选择策略。
///
/// wire 格式不是单一的枚举标签：简单模式是字符串（`"auto"` / `"required"` /
/// `"none"`），指定工具是对象 `{"type": "function", "function": {"name": ...}}`，
/// 因此手写序列化/反序列化。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    /// 模型自行决定是否调用工具。
    Auto,

    /// 模型必须调用至少一个工具。
    Required,

    /// 禁止调用任何工具。
    None,

    /// 强制调用指定工具。
    Function { name: String },
}

impl Serialize for ToolChoice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ToolChoice::Auto => serializer.serialize_str("auto"),
            ToolChoice::Required => serializer.serialize_str("required"),
            ToolChoice::None => serializer.serialize_str("none"),
            ToolChoice::Function { name } => {
                #[derive(Serialize)]
                struct FunctionChoice<'a> {
                    r#type: &'static str,
                    function: FunctionName<'a>,
                }
                #[derive(Serialize)]
                struct FunctionName<'a> {
                    name: &'a str,
                }
                FunctionChoice {
                    r#type: "function",
                    function: FunctionName { name },
                }
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => match s.as_str() {
                "auto" => Ok(ToolChoice::Auto),
                "required" => Ok(ToolChoice::Required),
                "none" => Ok(ToolChoice::None),
                other => Err(serde::de::Error::unknown_variant(
                    other,
                    &["auto", "required", "none"],
                )),
            },
            serde_json::Value::Object(_) => {
                #[derive(Deserialize)]
                struct FunctionChoice {
                    function: FunctionName,
                }
                #[derive(Deserialize)]
                struct FunctionName {
                    name: String,
                }
                let choice = FunctionChoice::deserialize(value).map_err(serde::de::Error::custom)?;
                Ok(ToolChoice::Function {
                    name: choice.function.name,
                })
            }
            other => Err(serde::de::Error::custom(format!(
                "invalid tool_choice: {other}"
            ))),
        }
    }
}

/// OpenAI 请求里的工具条目（wire 类型）。
///
/// 由协议中立的 [`Function`] 转换而来，序列化为 OpenAI 格式：
/// `{ "type": "function", "function": { name, description, parameters, strict } }`。
#[derive(Serialize, Debug, Clone)]
pub struct OpenaiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenaiFunctionDef,
}

/// OpenAI `tool.function` 子对象。
#[derive(Serialize, Debug, Clone)]
struct OpenaiFunctionDef {
    name: String,
    description: String,
    /// 参数的 JSON Schema 对象，由 [`Function::schema_object`] 生成。
    parameters: serde_json::Value,

    #[serde(skip_serializing_if = "std::ops::Not::not")]
    strict: bool,
}

impl From<&Function> for OpenaiTool {
    fn from(f: &Function) -> Self {
        Self {
            kind: "function",
            function: OpenaiFunctionDef {
                parameters: f.schema_object(),
                name: f.name.clone(),
                description: f.description.clone(),
                strict: f.strict,
            },
        }
    }
}

// `/models` 响应结构

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct ModelsResponse {
    object: String,
    pub data: Vec<ModelItem>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct ModelItem {
    pub id: String,
    object: String,
    owned_by: String,
}

// `/chat/completions` 响应结构

/// 聊天完成响应体。
///
/// 兼容 DeepSeek 与 OpenAI：两者字段不同的部分（缓存 token、`system_fingerprint`
/// 等）统一用 `Option` 兜底。
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct ChatResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub system_fingerprint: Option<String>,
    pub choices: Vec<Choice>,
    pub usage: Usage,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Choice {
    pub index: i32,
    pub message: ResponseMessage,
    pub finish_reason: String,
    pub logprobs: Option<Logprobs>,
}

/// 响应中的对话消息。
///
/// `content` 为 `Option`：模型只回工具调用时它是 `null`。
/// `reasoning_content` 为 DeepSeek 推理模型特有，`tool_calls` 为工具调用场景使用。
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: ToolCallFunction,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Logprobs {
    pub content: Option<Vec<LogprobContent>>,
    /// DeepSeek 推理 token 的 logprobs。
    pub reasoning_content: Option<Vec<LogprobContent>>,
    /// OpenAI 拒答 token 的 logprobs。
    pub refusal: Option<Vec<LogprobContent>>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct LogprobContent {
    pub token: String,
    pub logprob: f64,
    pub bytes: Option<Vec<u8>>,
    pub top_logprobs: Vec<TopLogprob>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct TopLogprob {
    pub token: String,
    pub logprob: f64,
    pub bytes: Option<Vec<u8>>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct Usage {
    pub completion_tokens: u32,
    pub prompt_tokens: u32,
    pub total_tokens: u32,
    /// DeepSeek 特有：命中缓存的 prompt token 数。
    pub prompt_cache_hit_tokens: Option<u32>,
    /// DeepSeek 特有：未命中缓存的 prompt token 数。
    pub prompt_cache_miss_tokens: Option<u32>,
    pub completion_tokens_details: Option<CompletionTokensDetails>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct CompletionTokensDetails {
    pub reasoning_tokens: u32,
}

// `/chat/completions` 流式响应（SSE chunk）结构

#[derive(Deserialize, Debug)]
struct StreamResponse {
    /// 开启 `include_usage` 时最后一帧的 choices 为空数组，只带 usage。
    #[serde(default)]
    choices: Vec<StreamChoice>,
    usage: Option<StreamUsage>,
}

#[derive(Deserialize, Debug)]
struct StreamUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize, Debug)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct StreamDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Deserialize, Debug)]
struct StreamToolCall {
    index: Option<u32>,
    id: Option<String>,
    function: Option<StreamToolCallFunction>,
}

#[derive(Deserialize, Debug)]
struct StreamToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

/// 把一条 SSE 事件映射为中立 [`ChatChunk`]。`data: [DONE]` 与既无 choice
/// 又无 usage 的事件返回 `None`。
fn map_stream_event(event: SseEvent) -> Option<Result<base::ChatChunk, Error>> {
    if event.data.trim() == "[DONE]" {
        return None;
    }
    let response: StreamResponse = match serde_json::from_str(&event.data) {
        Ok(r) => r,
        Err(e) => {
            return Some(Err(Error::Protocol(format!(
                "invalid stream chunk {:?}: {e}",
                event.data
            ))));
        }
    };
    let usage = response.usage.map(|u| base::Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });
    let Some(choice) = response.choices.into_iter().next() else {
        return usage.map(|usage| {
            Ok(base::ChatChunk {
                usage: Some(usage),
                ..Default::default()
            })
        });
    };
    let tool_calls = choice
        .delta
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|t| {
            let function = t.function.unwrap_or(StreamToolCallFunction {
                name: None,
                arguments: None,
            });
            base::ToolCallDelta {
                index: t.index.unwrap_or(0),
                id: t.id,
                name: function.name,
                arguments: function.arguments,
            }
        })
        .collect();
    Some(Ok(base::ChatChunk {
        content: choice.delta.content,
        reasoning: choice.delta.reasoning_content,
        tool_calls,
        finish_reason: choice.finish_reason,
        usage,
    }))
}

// 客户端

/// OpenAI Chat Completions 客户端（DeepSeek 亦复用之）。
///
/// `Clone` 很廉价：`reqwest::Client` 内部是 `Arc`，工具定义也是共享切片，
/// 适合给多个 agent 各持一份再分别换工具。
#[derive(Clone)]
pub struct OpenAiModel {
    base_url: String,
    api_key: String,
    pub model_list: Vec<String>,
    client: Client,
    pub args: OpenAiArgs,
}

impl OpenAiModel {
    /// 创建客户端。`base_url` 为空时回退到 [`OPENAI_BASE_URL`]；否则去掉末尾 `/`。
    pub fn new(base_url: &str, api_key: &str, args: OpenAiArgs) -> Self {
        Self::with_default_url(base_url, api_key, args, OPENAI_BASE_URL)
    }

    /// 供同包 vendor 预设复用：`base_url` 为空时回退到 `default`。
    pub(super) fn with_default_url(
        base_url: &str,
        api_key: &str,
        args: OpenAiArgs,
        default: &str,
    ) -> Self {
        let base_url = if base_url.is_empty() {
            default.to_owned()
        } else {
            base_url.trim_end_matches('/').to_owned()
        };
        Self {
            base_url,
            api_key: api_key.to_owned(),
            model_list: Vec::new(),
            client: Client::new(),
            args,
        }
    }
    /// 构造请求（`stream` 由调用方决定）并发送，返回原始响应。
    async fn send_chat(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
        stream: bool,
    ) -> Result<reqwest::Response, Error> {
        let request = self
            .args
            .build_request(messages, user_id, response_format.map(ResponseFormat::parse), stream);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await?;
        check_status(response).await
    }
}

impl Model for OpenAiModel {
    async fn chat(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<base::ChatOutput, Error> {
        let response = self
            .send_chat(messages, user_id, response_format, false)
            .await?
            .json::<ChatResponse>()
            .await?;

        let choice = response.choices.into_iter().next().ok_or(Error::EmptyResponse)?;
        let message = choice.message;
        Ok(base::ChatOutput {
            content: message.content,
            reasoning: message.reasoning_content,
            tool_calls: message
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|t| base::ToolCall {
                    id: t.id,
                    name: t.function.name,
                    arguments: t.function.arguments,
                })
                .collect(),
            finish_reason: choice.finish_reason,
            usage: base::Usage {
                prompt_tokens: response.usage.prompt_tokens,
                completion_tokens: response.usage.completion_tokens,
                total_tokens: response.usage.total_tokens,
            },
        })
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<base::ChatStream, Error> {
        let response = self
            .send_chat(messages, user_id, response_format, true)
            .await?;

        let chunks = sse_events(response.bytes_stream()).filter_map(|event| async move {
            match event {
                Ok(event) => map_stream_event(event),
                Err(e) => Some(Err(e)),
            }
        });
        Ok(Box::pin(chunks))
    }

    async fn get_models(&mut self) -> Result<(), Error> {
        let response = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Accept", "application/json")
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        self.model_list = check_status(response)
            .await?
            .json::<ModelsResponse>()
            .await?
            .data
            .into_iter()
            .map(|item| item.id)
            .collect();
        Ok(())
    }
}

// 参数

/// OpenAI 一族的预存调用参数。
///
/// 公共参数（`model` / `temperature` / `top_p` / `tools`）直接持有；vendor 特有
/// 的那几个字段由第二层枚举 [`OpenAiVendor`] 承载，构造时即选定 vendor，从类型上
/// 杜绝非法字段组合。DeepSeek 预设见 [`super::deepseek`]。
#[derive(Clone, Debug)]
pub struct OpenAiArgs {
    /// 使用模型的 id。
    pub model: String,

    /// 温度参数，内部用 0-200 的 u8 表示 0.0-2.0，序列化时自动除 100。
    pub temperature: u8,

    /// top-p 采样参数，内部用 0-100 的 u8 表示 0.0-1.0，序列化时自动除 100。
    pub top_p: u8,

    /// 工具列表，设置时一次性翻译成 wire 类型并共享，之后每轮请求只借用。
    pub(super) tools: Arc<[OpenaiTool]>,

    /// 第二层 vendor 及其特有字段。
    pub vendor: OpenAiVendor,
}

impl OpenAiArgs {
    /// OpenAI 预设：temperature / top_p 默认 1.0，vendor 为原生 OpenAI。
    pub fn openai(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            temperature: 100,
            top_p: 100,
            tools: Arc::new([]),
            vendor: OpenAiVendor::OpenAi {
                frequency_penalty: None,
                presence_penalty: None,
                tool_choice: None,
            },
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

    pub fn tools(mut self, tools: &[Function]) -> Self {
        self.set_tools(tools);
        self
    }

    /// 覆盖工具列表（翻译成 wire 类型后缓存）。
    pub fn set_tools(&mut self, tools: &[Function]) {
        self.tools = tools.iter().map(OpenaiTool::from).collect();
    }

    /// 设置 OpenAI 工具选择策略；vendor 非 OpenAI 时为空操作。
    pub fn tool_choice(mut self, choice: ToolChoice) -> Self {
        if let OpenAiVendor::OpenAi { tool_choice, .. } = &mut self.vendor {
            *tool_choice = Some(choice);
        }
        self
    }

    /// 设置 OpenAI 频率/存在惩罚；vendor 非 OpenAI 时为空操作。
    pub fn penalties(mut self, frequency: Option<f32>, presence: Option<f32>) -> Self {
        if let OpenAiVendor::OpenAi {
            frequency_penalty,
            presence_penalty,
            ..
        } = &mut self.vendor
        {
            *frequency_penalty = frequency;
            *presence_penalty = presence;
        }
        self
    }

    /// 用预存参数与传入消息构造请求体（全程借用）。
    pub(crate) fn build_request<'a>(
        &'a self,
        messages: &'a [Message],
        user: Option<&'a str>,
        response_format: Option<ResponseFormat>,
        stream: bool,
    ) -> ChatRequest<'a> {
        let echo_reasoning = matches!(self.vendor, OpenAiVendor::Deepseek { .. });
        ChatRequest {
            messages: wire_messages(messages, echo_reasoning),
            model: &self.model,
            response_format,
            stream,
            stream_options: stream.then_some(StreamOptions {
                include_usage: true,
            }),
            temperature: self.temperature,
            top_p: self.top_p,
            tools: &self.tools,
            user,
            vendor: &self.vendor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ToolChoice wire format ----

    #[test]
    fn tool_choice_serializes_simple_modes_as_strings() {
        assert_eq!(serde_json::to_string(&ToolChoice::Auto).unwrap(), "\"auto\"");
        assert_eq!(
            serde_json::to_string(&ToolChoice::Required).unwrap(),
            "\"required\""
        );
        assert_eq!(serde_json::to_string(&ToolChoice::None).unwrap(), "\"none\"");
    }

    #[test]
    fn tool_choice_serializes_function_as_object() {
        let json = serde_json::to_value(&ToolChoice::Function {
            name: "read_file".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "function", "function": {"name": "read_file"}})
        );
    }

    #[test]
    fn tool_choice_deserialize_roundtrip() {
        assert_eq!(
            serde_json::from_str::<ToolChoice>("\"auto\"").unwrap(),
            ToolChoice::Auto
        );
        assert_eq!(
            serde_json::from_str::<ToolChoice>(
                "{\"type\": \"function\", \"function\": {\"name\": \"f\"}}"
            )
            .unwrap(),
            ToolChoice::Function { name: "f".into() }
        );
        assert!(serde_json::from_str::<ToolChoice>("\"bogus\"").is_err());
    }

    // ---- ResponseFormat wire format ----

    #[test]
    fn response_format_serializes_as_typed_object() {
        assert_eq!(
            serde_json::to_value(&ResponseFormat::JsonObject).unwrap(),
            serde_json::json!({"type": "json_object"})
        );
        assert_eq!(
            serde_json::to_value(&ResponseFormat::Text).unwrap(),
            serde_json::json!({"type": "text"})
        );
        assert_eq!(
            serde_json::from_str::<ResponseFormat>("{\"type\": \"json_object\"}").unwrap(),
            ResponseFormat::JsonObject
        );
    }

    // ---- 响应反序列化 ----

    #[test]
    fn chat_response_accepts_null_content_with_tool_calls() {
        // 模型只回工具调用时 content 为 null，这是合法响应
        let body = r#"{
            "id":"x","object":"chat.completion","created":1,"model":"m",
            "system_fingerprint":null,
            "choices":[{"index":0,"finish_reason":"tool_calls","logprobs":null,
                "message":{"role":"assistant","content":null,"reasoning_content":null,
                    "tool_calls":[{"id":"1","type":"function",
                        "function":{"name":"read_file","arguments":"{\"file_path\":\"a\"}"}}]}}],
            "usage":{"completion_tokens":1,"prompt_tokens":2,"total_tokens":3,
                "prompt_cache_hit_tokens":null,"prompt_cache_miss_tokens":null,
                "completion_tokens_details":null}
        }"#;
        let response: ChatResponse = serde_json::from_str(body).unwrap();
        let message = &response.choices[0].message;
        assert_eq!(message.content, None);
        let tool_calls = message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].function.name, "read_file");
        assert_eq!(tool_calls[0].function.arguments, "{\"file_path\":\"a\"}");
    }

    // ---- 请求序列化（build_request 全链路） ----

    fn request_json(args: &OpenAiArgs, messages: &[Message], stream: bool) -> serde_json::Value {
        serde_json::to_value(args.build_request(messages, None, None, stream)).unwrap()
    }

    #[test]
    fn request_serializes_percent_fields_as_decimals() {
        let args = OpenAiArgs::openai("gpt-5").temperature(70).top_p(95);
        let json = request_json(&args, &[], false);
        assert_eq!(json["temperature"], serde_json::json!(0.7));
        assert_eq!(json["top_p"], serde_json::json!(0.95));
        assert_eq!(json["stream"], serde_json::json!(false));
        assert!(json.get("stream_options").is_none());
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn streaming_request_asks_for_usage() {
        let json = request_json(&OpenAiArgs::openai("gpt-5"), &[], true);
        assert_eq!(json["stream"], serde_json::json!(true));
        assert_eq!(json["stream_options"], serde_json::json!({"include_usage": true}));
    }

    #[test]
    fn request_carries_tools_once_translated() {
        let args = OpenAiArgs::openai("gpt-5").tools(&[Function {
            name: "read_file".into(),
            description: "read".into(),
            parameters: vec![],
            strict: false,
        }]);
        let json = request_json(&args, &[], false);
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "read_file");
        // clone 共享同一份工具切片
        assert!(Arc::ptr_eq(&args.tools, &args.clone().tools));
    }

    #[test]
    fn request_maps_tool_round_messages() {
        let messages = [
            Message::system("sys"),
            Message::user("hi"),
            Message::Assistant {
                content: None,
                reasoning: Some("think".into()),
                tool_calls: vec![base::ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: "{\"file_path\":\"a\"}".into(),
                }],
            },
            Message::tool("call_1", "file body"),
            Message::Assistant {
                content: Some("done".into()),
                reasoning: Some("final think".into()),
                tool_calls: vec![],
            },
        ];
        let json = request_json(&OpenAiArgs::openai("gpt-5"), &messages, false);
        assert_eq!(
            json["messages"],
            serde_json::json!([
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"file_path\":\"a\"}"}}
                ]},
                {"role": "tool", "content": "file body", "tool_call_id": "call_1"},
                {"role": "assistant", "content": "done"},
            ])
        );

        // DeepSeek 只在工具回合回传 reasoning_content
        let json = request_json(&OpenAiArgs::deepseek("deepseek-v4-pro"), &messages, false);
        assert_eq!(json["messages"][2]["reasoning_content"], "think");
        assert!(json["messages"][4].get("reasoning_content").is_none());
    }

    #[test]
    fn request_flattens_vendor_fields_without_cross_contamination() {
        let openai = request_json(&OpenAiArgs::openai("gpt-5").penalties(Some(0.5), None), &[], false);
        assert_eq!(openai["frequency_penalty"], serde_json::json!(0.5));
        assert!(openai.get("presence_penalty").is_none());
        assert!(openai.get("thinking").is_none());
        assert!(openai.get("reasoning_effort").is_none());

        let deepseek = request_json(&OpenAiArgs::deepseek("deepseek-v4-pro"), &[], false);
        assert_eq!(deepseek["thinking"], serde_json::json!({"type": "enabled"}));
        assert_eq!(deepseek["reasoning_effort"], serde_json::json!("high"));
        assert!(deepseek.get("frequency_penalty").is_none());
        assert!(deepseek.get("tool_choice").is_none());
    }

    // ---- 流式事件映射 ----

    #[test]
    fn map_stream_event_maps_content_and_done() {
        let event = SseEvent {
            event: None,
            data: "{\"choices\":[{\"delta\":{\"content\":\"he\",\"reasoning_content\":null,\"tool_calls\":null},\"finish_reason\":null}]}".into(),
        };
        let chunk = map_stream_event(event).unwrap().unwrap();
        assert_eq!(chunk.content.as_deref(), Some("he"));
        assert_eq!(chunk.finish_reason, None);

        let done = SseEvent { event: None, data: "[DONE]".into() };
        assert!(map_stream_event(done).is_none());
    }

    #[test]
    fn map_stream_event_maps_tool_call_deltas() {
        let event = SseEvent {
            event: None,
            data: "{\"choices\":[{\"delta\":{\"content\":null,\"reasoning_content\":null,
                     \"tool_calls\":[{\"index\":0,\"id\":\"call_1\",
                        \"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"file\"}}]},
                     \"finish_reason\":null}]}".into(),
        };
        let chunk = map_stream_event(event).unwrap().unwrap();
        assert_eq!(chunk.tool_calls.len(), 1);
        let delta = &chunk.tool_calls[0];
        assert_eq!(delta.index, 0);
        assert_eq!(delta.id.as_deref(), Some("call_1"));
        assert_eq!(delta.name.as_deref(), Some("read_file"));
        assert_eq!(delta.arguments.as_deref(), Some("{\"file"));
    }

    #[test]
    fn map_stream_event_maps_usage_only_chunk() {
        let event = SseEvent {
            event: None,
            data: "{\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4,\"total_tokens\":7}}".into(),
        };
        let chunk = map_stream_event(event).unwrap().unwrap();
        assert_eq!(chunk.usage, Some(base::Usage::new(3, 4)));
        assert_eq!(chunk.content, None);

        let empty = SseEvent { event: None, data: "{\"choices\":[]}".into() };
        assert!(map_stream_event(empty).is_none());
    }

    #[test]
    fn map_stream_event_surfaces_malformed_data() {
        let event = SseEvent { event: None, data: "not json".into() };
        match map_stream_event(event) {
            Some(Err(Error::Protocol(_))) => {}
            other => panic!("expected protocol error, got {other:?}"),
        }
    }
}
