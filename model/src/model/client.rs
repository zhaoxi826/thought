//! OpenAI 兼容客户端。
//!
//! DeepSeek 与 OpenAI 共用同一套 HTTP 调用逻辑，差异仅在于各自的 [`ChatArgs`]
//! 实现（默认 base URL、vendor 特有请求字段）。

use crate::model::base::{Error, Model, OpenAIMessage};
use crate::model::wire::{ChatArgs, ChatResponse, ModelsResponse, ResponseFormat};
use reqwest::Client;

/// 泛型 OpenAI 兼容模型客户端。
///
/// `A` 为 vendor 参数类型，决定默认 base URL 与请求体的 vendor 特有字段。
pub struct OpenAiCompatModel<A: ChatArgs> {
    base_url: String,
    api_key: String,
    pub model_list: Vec<String>,
    client: Client,
    pub args: A,
}

impl<A: ChatArgs> OpenAiCompatModel<A> {
    /// 创建客户端。`base_url` 为空时回退到 vendor 默认 URL；否则去掉末尾 `/`。
    pub fn new(base_url: &str, api_key: &str, args: A) -> Self {
        let base_url = if base_url.is_empty() {
            A::default_base_url().to_owned()
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

impl<A: ChatArgs + Send + Sync> Model for OpenAiCompatModel<A> {
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
