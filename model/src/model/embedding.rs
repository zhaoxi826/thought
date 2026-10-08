//! OpenAI 兼容的 `/embeddings` 客户端（OpenAI、硅基流动、Ollama 等通用）。

use crate::model::base::{check_status, Error};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// 向量化模型客户端。`base_url` 约定包含版本前缀（如 `https://api.openai.com/v1`），
/// 请求发往 `{base_url}/embeddings`。
#[derive(Clone)]
pub struct EmbeddingModel {
    base_url: String,
    api_key: String,
    pub model: String,
    /// 要求服务端输出的维度（支持 Matryoshka 的模型可用）；`None` 用模型默认。
    pub dimensions: Option<u32>,
    client: Client,
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
    encoding_format: &'static str,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

impl EmbeddingModel {
    pub fn new(base_url: &str, api_key: &str, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.into(),
            dimensions: None,
            client: Client::new(),
        }
    }

    pub fn dimensions(mut self, dimensions: u32) -> Self {
        self.dimensions = Some(dimensions);
        self
    }

    /// 批量向量化，输出顺序与输入一致。
    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, Error> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let request = EmbeddingRequest {
            model: &self.model,
            input: inputs,
            dimensions: self.dimensions,
            encoding_format: "float",
        };
        let response = self
            .client
            .post(format!("{}/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await?;
        let mut data = check_status(response).await?.json::<EmbeddingResponse>().await?.data;
        if data.len() != inputs.len() {
            return Err(Error::Protocol(format!(
                "expected {} embeddings, got {}",
                inputs.len(),
                data.len()
            )));
        }
        data.sort_by_key(|item| item.index);
        Ok(data.into_iter().map(|item| item.embedding).collect())
    }
}
