//! Anthropic Messages API 适配。
//!
//! 与 OpenAI 兼容接口不同：走 `/v1/messages`，鉴权用 `x-api-key` +
//! `anthropic-version` 头，`system` 提示是顶层字段，消息 content 是 block 数组。
//! 因此不复用 OpenAI 协议的客户端，自带一套线协议，但实现同一个 [`Model`] trait
//! （`type Message = AnthropicMessage`）。

use crate::model::base::{percent, AnthropicMessage, Error, Message, Model};
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

impl Model for AnthropicModel {
    type Message = AnthropicMessage;

    async fn chat(
        &self,
        messages: Vec<AnthropicMessage>,
        user_id: Option<&str>,
        // Anthropic 没有 OpenAI 式的 response_format，结构化输出走工具，此处忽略。
        _response_format: Option<&str>,
    ) -> Result<String, Error> {
        // role == "system" 的消息提取为顶层 system 字段，其余留在 messages 里。
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

        let request = MessagesRequest {
            model: &self.args.model,
            max_tokens: self.args.max_tokens,
            messages: chat_messages,
            system,
            temperature: self.args.temperature,
            top_p: self.args.top_p,
            metadata: user_id.map(|id| Metadata {
                user_id: id.to_owned(),
            }),
        };

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.version)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<MessagesResponse>()
            .await?;

        response
            .content
            .into_iter()
            .filter_map(|block| match block {
                ResponseContentBlock::Text { text } => Some(text),
                ResponseContentBlock::Other => None,
            })
            .next()
            .ok_or(Error::EmptyResponse)
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
}

impl Default for AnthropicArgs {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_tokens: DEFAULT_MAX_TOKENS,
            system: None,
            temperature: 100,
            top_p: 100,
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

    /// 未知或 vendor 扩展 block（如 DeepSeek 的 `thinking`），保留原始 type 字段。
    #[serde(other)]
    Other,
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
