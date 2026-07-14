//! DeepSeek 的 OpenAI 兼容预设。
//!
//! DeepSeek 不是独立协议/类型：它复用同包 [`super::openai`] 的 `OpenAiModel` 与
//! `OpenAiArgs`，只是把第二层 [`OpenAiVendor`] 选成 `Deepseek` 变体（多带思考字段），
//! 并把默认 base URL 换成 [`DEEPSEEK_BASE_URL`]。这里只提供便捷构造/设值入口。

use super::openai::{
    OpenAiArgs, OpenAiModel, OpenAiVendor, Thinking, ThinkingConfig, DEEPSEEK_BASE_URL,
};

impl OpenAiModel {
    /// DeepSeek 预设客户端：`base_url` 为空时回退到 [`DEEPSEEK_BASE_URL`]。
    ///
    /// 与 OpenAI 仅 base URL 不同；DeepSeek 特有参数由 `args` 携带，
    /// 通常用 [`OpenAiArgs::deepseek`] 构造。
    pub fn deepseek(base_url: &str, api_key: &str, args: OpenAiArgs) -> Self {
        Self::with_default_url(base_url, api_key, args, DEEPSEEK_BASE_URL)
    }
}

impl OpenAiArgs {
    /// DeepSeek 预设参数：vendor 为 DeepSeek，默认开启思考模式、强度 high。
    pub fn deepseek(model: impl Into<String>) -> Self {
        Self {
            vendor: OpenAiVendor::Deepseek {
                thinking: Thinking::enabled(),
                reasoning_effort: ThinkingConfig::High,
            },
            ..Self::openai(model)
        }
    }

    /// 设置 DeepSeek 思考模式/强度；vendor 非 DeepSeek 时为空操作。
    pub fn thinking(mut self, t: Thinking, effort: ThinkingConfig) -> Self {
        if let OpenAiVendor::Deepseek {
            thinking,
            reasoning_effort,
        } = &mut self.vendor
        {
            *thinking = t;
            *reasoning_effort = effort;
        }
        self
    }
}
