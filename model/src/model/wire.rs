//! OpenAI 兼容线协议：请求体、响应体、工具定义，以及 vendor 参数抽象 [`ChatArgs`]。
//!
//! DeepSeek 与 OpenAI 共享这一整套结构，差异部分通过 [`ChatRequest`] 的
//! `extra` 字段（`#[serde(flatten)]` 展开）注入。

use crate::model::base::OpenAIMessage;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

/// vendor 参数抽象。
///
/// 每个 vendor 用一个 `Args` 结构预存调用参数，并声明自身的默认 base URL
/// 与 vendor 特有字段（`Extra`）。客户端据此构造统一的 [`ChatRequest`]。
pub trait ChatArgs {
    /// vendor 特有的请求字段，序列化时展开到请求体顶层。
    type Extra: Serialize + Send + Sync;

    /// 该 vendor 的默认 base URL，在 `new` 时未显式传入则使用它。
    fn default_base_url() -> &'static str;

    /// 预存的模型 id。
    fn model(&self) -> &str;

    /// 用预存参数与传入消息构造请求体。
    ///
    /// `response_format`、`user_id` 由客户端在调用时补充，这里一律置空；
    /// `stream` 暂时恒为 `false`（流式尚未实现）。
    fn build_request(&self, messages: Vec<OpenAIMessage>) -> ChatRequest<Self::Extra>;
}

// `/chat/completions` 请求结构

/// 聊天完成请求体，`E` 为 vendor 特有字段。
#[derive(Serialize, Debug)]
pub struct ChatRequest<E> {
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

    /// 工具列表。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,

    /// 用户标识，按需使用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// vendor 特有字段，序列化时展开到顶层。
    #[serde(flatten)]
    pub extra: E,
}

/// 将 u8 以百分比形式与 f32 互转。
///
/// 例如 100 序列化为 1.0，150 序列化为 1.5。
pub mod percent {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u8, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f32(*value as f32 / 100.0)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
        let v = f32::deserialize(deserializer)?;
        Ok((v.clamp(0.0, 2.0) * 100.0).round() as u8)
    }
}

/// 响应格式配置。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ResponseFormat {
    #[serde(rename = "text")]
    Text,

    #[serde(rename = "json_object")]
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
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ToolChoice {
    /// 模型自行决定是否调用工具。
    #[serde(rename = "auto")]
    Auto,

    /// 模型必须调用至少一个工具。
    #[serde(rename = "required")]
    Required,

    /// 禁止调用任何工具。
    #[serde(rename = "none")]
    None,

    /// 强制调用指定工具。
    #[serde(rename = "function")]
    Function { name: String },
}

/// 工具定义。
#[derive(Deserialize, Debug, Clone)]
pub enum Tool {
    Function(Function),
}

impl Serialize for Tool {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Tool::Function(func) = self;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("type", "function")?;
        map.serialize_entry("function", &func.to_tool_function())?;
        map.end()
    }
}

/// 工具函数定义。
///
/// 序列化时自动展开为 OpenAI 兼容的 JSON Schema 格式：
/// ```json
/// {
///   "type": "object",
///   "properties": { ... },
///   "required": [ ... ]
/// }
/// ```
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Function {
    pub name: String,
    pub description: String,
    pub parameters: Vec<Parameter>,

    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

/// 工具函数参数定义。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    pub r#type: ParameterType,

    /// 是否必填。默认 true。
    ///
    /// 在 `parameters.required` 数组中，只有 `required = true` 的参数名会被加入。
    #[serde(skip)]
    pub required: bool,
}

impl Default for Parameter {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            r#type: ParameterType::String,
            required: true,
        }
    }
}

/// 参数类型。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ParameterType {
    #[serde(rename = "string")]
    String,

    #[serde(rename = "integer")]
    Integer,

    #[serde(rename = "number")]
    Number,

    #[serde(rename = "boolean")]
    Boolean,

    #[serde(rename = "array")]
    Array,

    #[serde(rename = "object")]
    Object,
}

impl Function {
    /// 转换为 OpenAI 兼容的 `tool` 条目：包含 `type` 和 `function`。
    pub fn to_tool(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": self.to_tool_function(),
        })
    }

    /// 转换为 OpenAI 兼容的 function 对象。
    pub fn to_tool_function(&self) -> serde_json::Value {
        let mut properties = std::collections::HashMap::<String, serde_json::Value>::new();
        let mut required = Vec::<String>::new();

        for param in &self.parameters {
            let type_str = serde_json::to_value(&param.r#type)
                .unwrap_or_else(|_| serde_json::Value::String("string".to_owned()))
                .as_str()
                .unwrap_or("string")
                .to_owned();

            properties.insert(
                param.name.clone(),
                serde_json::json!({
                    "type": type_str,
                    "description": param.description,
                }),
            );

            if param.required {
                required.push(param.name.clone());
            }
        }

        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            },
            "strict": self.strict,
        })
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
/// `reasoning_content` 为 DeepSeek 推理模型特有，`tool_calls` 为工具调用场景使用。
#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone)]
pub struct ResponseMessage {
    pub role: String,
    pub content: String,
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
