//! 可序列化的模型配置：从配置文件（TOML / JSON）描述一个模型，再
//! [`build`](ModelConfig::build) 成运行时的 [`Model`](crate::Model)。

use crate::providers::anthropic::{AnthropicArgs, AnthropicModel};
use crate::providers::openai::{OpenAiArgs, OpenAiModel, Thinking, ThinkingConfig};
use serde::{Deserialize, Serialize};

/// 协议 / 预设。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// 原生 OpenAI（以及任意 OpenAI 兼容服务）。
    Openai,
    /// DeepSeek（OpenAI 协议 + 思考字段）。
    Deepseek,
    /// Anthropic Messages（以及兼容服务，如 DeepSeek 的 `/anthropic` 端点）。
    Anthropic,
}

/// 一个模型的配置。
///
/// `temperature` / `top_p` 在配置里写小数（`0.7`），内部换算成百分之一定点数。
/// `api_key` 与 `api_key_env` 二选一：后者从环境变量读取，避免把密钥写进配置文件。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ModelConfig {
    pub provider: Provider,

    /// 模型 id，如 `deepseek-v4-pro`。
    pub model: String,

    /// 留空用各协议默认地址。
    #[serde(default)]
    pub base_url: String,

    /// 明文密钥。序列化时跳过，避免经接口或日志泄露。
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,

    /// 存放密钥的环境变量名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// 仅 Anthropic：采样上限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// 仅 DeepSeek：是否开启思考模式（默认开启）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
}

/// 配置错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// 既没有 `api_key` 也没有可读的 `api_key_env`。
    MissingApiKey(String),
    /// 采样参数越界。
    OutOfRange(&'static str),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::MissingApiKey(model) => write!(f, "model {model}: missing api key"),
            ConfigError::OutOfRange(field) => write!(f, "{field} must be within 0.0..=2.0"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// 小数 → 百分之一定点数（0.7 → 70）。
fn to_percent(value: f32, field: &'static str) -> Result<u8, ConfigError> {
    if !(0.0..=2.0).contains(&value) {
        return Err(ConfigError::OutOfRange(field));
    }
    Ok((value * 100.0).round() as u8)
}

impl ModelConfig {
    /// 解析密钥：`api_key` 优先，其次 `api_key_env`。
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        if let Some(key) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            return Ok(key.clone());
        }
        self.api_key_env
            .as_deref()
            .and_then(|var| std::env::var(var).ok())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| ConfigError::MissingApiKey(self.model.clone()))
    }

    /// 构造运行时模型（尚未挂工具）。
    pub fn build(&self) -> Result<crate::Model, ConfigError> {
        let api_key = self.resolve_api_key()?;
        let temperature = self.temperature.map(|v| to_percent(v, "temperature")).transpose()?;
        let top_p = self.top_p.map(|v| to_percent(v, "top_p")).transpose()?;

        Ok(match self.provider {
            Provider::Openai | Provider::Deepseek => {
                let mut args = if self.provider == Provider::Deepseek {
                    let args = OpenAiArgs::deepseek(&self.model);
                    match self.thinking {
                        Some(false) => args.thinking(Thinking::disabled(), ThinkingConfig::High),
                        _ => args,
                    }
                } else {
                    OpenAiArgs::openai(&self.model)
                };
                if let Some(t) = temperature {
                    args = args.temperature(t);
                }
                if let Some(p) = top_p {
                    args = args.top_p(p);
                }
                let model = if self.provider == Provider::Deepseek {
                    OpenAiModel::deepseek(&self.base_url, &api_key, args)
                } else {
                    OpenAiModel::new(&self.base_url, &api_key, args)
                };
                crate::Model::OpenAi(model)
            }
            Provider::Anthropic => {
                let mut args = AnthropicArgs::new(&self.model);
                if let Some(t) = temperature {
                    args = args.temperature(t);
                }
                if let Some(p) = top_p {
                    args = args.top_p(p);
                }
                if let Some(m) = self.max_tokens {
                    args = args.max_tokens(m);
                }
                crate::Model::Anthropic(AnthropicModel::new(&self.base_url, &api_key, args))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: serde_json::Value) -> ModelConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn builds_each_provider() {
        let ds = config(serde_json::json!({
            "provider": "deepseek", "model": "deepseek-v4-pro", "api_key": "sk-x",
            "temperature": 0.7, "thinking": false
        }));
        match ds.build().unwrap() {
            crate::Model::OpenAi(m) => {
                assert_eq!(m.args.temperature, 70);
                assert!(matches!(
                    &m.args.vendor,
                    crate::OpenAiVendor::Deepseek { thinking, .. }
                        if matches!(thinking.r#type, crate::ThinkingType::Disabled)
                ));
            }
            _ => panic!("expected openai protocol"),
        }

        let claude = config(serde_json::json!({
            "provider": "anthropic", "model": "claude", "api_key": "k", "max_tokens": 4096
        }));
        match claude.build().unwrap() {
            crate::Model::Anthropic(m) => assert_eq!(m.args.max_tokens, 4096),
            _ => panic!("expected anthropic"),
        }
    }

    #[test]
    fn api_key_never_serialized_and_required() {
        let c = config(serde_json::json!({"provider": "openai", "model": "gpt-5", "api_key": "sk-secret"}));
        assert!(!serde_json::to_string(&c).unwrap().contains("sk-secret"));

        let missing = config(serde_json::json!({
            "provider": "openai", "model": "gpt-5", "api_key_env": "THOUGHT_TEST_SURELY_UNSET"
        }));
        assert_eq!(missing.build().err(), Some(ConfigError::MissingApiKey("gpt-5".into())));
    }

    #[test]
    fn rejects_out_of_range_sampling() {
        let c = config(serde_json::json!({"provider": "openai", "model": "m", "api_key": "k", "temperature": 3.0}));
        assert_eq!(c.build().err(), Some(ConfigError::OutOfRange("temperature")));
    }
}
