//! OpenAI Chat Completions 协议引擎：客户端、线协议（请求/响应/工具）、参数与
//! 原生 OpenAI 预设。
//!
//! 这是 OpenAI 一族的共享实现。DeepSeek 不是独立协议，只是第二层 [`OpenAiVendor`]
//! 选到 `Deepseek` 变体、多带思考字段——其预设在同包的 [`super::deepseek`] 里。

use crate::model::base::{self, percent, Error, Model, OpenAIMessage};
use crate::model::sse::{sse_events, SseEvent};
use crate::model::tool::Function;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const OPENAI_BASE_URL: &str = "https://api.openai.com";
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

// `/chat/completions` 请求结构

/// 聊天完成请求体。
///
/// 公共字段直接列在这里；vendor 特有的那几个字段由第二层枚举 [`OpenAiVendor`]
/// 承载，`#[serde(flatten)]` 展开到请求顶层。
#[derive(Serialize, Debug)]
pub struct ChatRequest {
    /// 发送的消息列表。
    pub messages: Vec<OpenAIMessage>,

    /// 使用模型的 id。
    pub model: String,

    /// 响应格式配置。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// 是否流式输出。暂时恒为 `false`。
    pub stream: bool,

    /// 温度参数，内部用 0-200 的 u8 表示 0.0-2.0，序列化时自动除 100。
    #[serde(with = "percent")]
    pub temperature: u8,

    /// top-p 采样参数，内部用 0-100 的 u8 表示 0.0-1.0，序列化时自动除 100。
    #[serde(with = "percent")]
    pub top_p: u8,

    /// 工具列表（wire 类型）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OpenaiTool>,

    /// 用户标识，按需使用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// 第二层：vendor 特有字段（OpenAI vs DeepSeek），展开到顶层。
    #[serde(flatten)]
    pub vendor: OpenAiVendor,
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

impl From<Function> for OpenaiTool {
    fn from(f: Function) -> Self {
        Self {
            kind: "function",
            function: OpenaiFunctionDef {
                parameters: f.schema_object(),
                name: f.name,
                description: f.description,
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
    choices: Vec<StreamChoice>,
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

/// 把一条 SSE 事件映射为中立 [`ChatChunk`]。`data: [DONE]` 与无增量
/// 事件（如 usage-only chunk）返回 `None`。
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
    let choice = response.choices.into_iter().next()?;
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
    }))
}

// 客户端

/// OpenAI Chat Completions 客户端（DeepSeek 亦复用之）。
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
        messages: Vec<OpenAIMessage>,
        user_id: Option<&str>,
        response_format: Option<&str>,
        stream: bool,
    ) -> Result<reqwest::Response, Error> {
        let mut request = self.args.build_request(messages);
        request.user_id = user_id.map(|s| s.to_owned());
        request.response_format = response_format.map(ResponseFormat::parse);
        request.stream = stream;

        Ok(self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?
            .error_for_status()?)
    }
}

impl Model for OpenAiModel {
    type Message = OpenAIMessage;

    async fn chat(
        &self,
        messages: Vec<OpenAIMessage>,
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
        messages: Vec<OpenAIMessage>,
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
        self.model_list = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Accept", "application/json")
            .bearer_auth(&self.api_key)
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

    /// 工具列表（协议中立的 [`Function`]，构造请求时转成 wire 类型）。
    pub tools: Vec<Function>,

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
            tools: Vec::new(),
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

    pub fn tools(mut self, tools: Vec<Function>) -> Self {
        self.tools = tools;
        self
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

    /// 用预存参数与传入消息构造请求体。
    ///
    /// `response_format` / `user_id` 由客户端在调用时补充，这里置空；
    /// `stream` 暂时恒为 `false`（流式尚未实现）。
    pub(super) fn build_request(&self, messages: Vec<OpenAIMessage>) -> ChatRequest {
        ChatRequest {
            messages,
            model: self.model.clone(),
            response_format: None,
            stream: false,
            temperature: self.temperature,
            top_p: self.top_p,
            tools: self.tools.iter().cloned().map(OpenaiTool::from).collect(),
            user_id: None,
            vendor: self.vendor.clone(),
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

    #[test]
    fn request_serializes_percent_fields_as_decimals() {
        let args = OpenAiArgs::openai("gpt-5").temperature(70).top_p(95);
        let request = args.build_request(vec![]);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["temperature"], serde_json::json!(0.7));
        assert_eq!(json["top_p"], serde_json::json!(0.95));
        assert_eq!(json["stream"], serde_json::json!(false));
    }

    #[test]
    fn request_flattens_vendor_fields_without_cross_contamination() {
        let openai = serde_json::to_value(
            OpenAiArgs::openai("gpt-5")
                .penalties(Some(0.5), None)
                .build_request(vec![]),
        )
        .unwrap();
        assert_eq!(openai["frequency_penalty"], serde_json::json!(0.5));
        assert!(openai.get("presence_penalty").is_none());
        assert!(openai.get("thinking").is_none());
        assert!(openai.get("reasoning_effort").is_none());

        let deepseek = serde_json::to_value(OpenAiArgs::deepseek("deepseek-v4-pro").build_request(vec![])).unwrap();
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
    fn map_stream_event_surfaces_malformed_data() {
        let event = SseEvent { event: None, data: "not json".into() };
        match map_stream_event(event) {
            Some(Err(Error::Protocol(_))) => {}
            other => panic!("expected protocol error, got {other:?}"),
        }
    }
}
