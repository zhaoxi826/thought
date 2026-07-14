//! OpenAI Chat Completions 协议引擎：客户端、线协议（请求/响应/工具）、参数与
//! 原生 OpenAI 预设。
//!
//! 这是 OpenAI 一族的共享实现。DeepSeek 不是独立协议，只是第二层 [`OpenAiVendor`]
//! 选到 `Deepseek` 变体、多带思考字段——其预设在同包的 [`super::deepseek`] 里。

use crate::model::base::{percent, Error, Model, OpenAIMessage};
use reqwest::Client;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};

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

    /// 工具列表。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,

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
}

impl Model for OpenAiModel {
    type Message = OpenAIMessage;

    async fn chat(
        &self,
        messages: Vec<OpenAIMessage>,
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> Result<String, Error> {
        let mut request = self.args.build_request(messages);
        request.user_id = user_id.map(|s| s.to_owned());
        request.response_format = response_format.map(ResponseFormat::parse);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<ChatResponse>()
            .await?;

        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or(Error::EmptyResponse)
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

    /// 工具列表。
    pub tools: Vec<Tool>,

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

    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
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
            tools: self.tools.clone(),
            user_id: None,
            vendor: self.vendor.clone(),
        }
    }
}
