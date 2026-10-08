//! 协议中立的对话消息与模型 trait。

use super::error::Error;
use super::output::{ChatOutput, ToolCall};
use super::stream::ChatStream;
use serde::{Deserialize, Serialize};

/// 协议中立的对话消息。
///
/// 覆盖完整的工具调用回合：assistant 发起 `tool_calls`，随后每个调用对应一条
/// [`Message::Tool`] 回传结果。各协议在构造请求时把它翻译成自己的 wire 格式
/// （OpenAI 的 `tool` 角色 / Anthropic 的 `tool_use` + `tool_result` block），
/// 序列化时只借用、不拷贝历史。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,

        /// 推理内容；DeepSeek 思考模式在工具回合内要求原样回传。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,

        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        /// 对应 [`ToolCall::id`]。
        call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Message::User {
            content: content.into(),
        }
    }

    /// 纯文本的 assistant 回复。
    pub fn assistant(content: impl Into<String>) -> Self {
        Message::Assistant {
            content: Some(content.into()),
            reasoning: None,
            tool_calls: Vec::new(),
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message::Tool {
            call_id: call_id.into(),
            content: content.into(),
        }
    }

    /// 角色名（与 OpenAI wire 一致）。
    pub fn role(&self) -> &'static str {
        match self {
            Message::System { .. } => "system",
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
            Message::Tool { .. } => "tool",
        }
    }

    /// 文本内容；只有工具调用的 assistant 消息返回空串。
    pub fn text(&self) -> &str {
        match self {
            Message::System { content }
            | Message::User { content }
            | Message::Tool { content, .. } => content,
            Message::Assistant { content, .. } => content.as_deref().unwrap_or(""),
        }
    }
}

impl From<ChatOutput> for Message {
    /// 把模型输出转成可回放进历史的 assistant 消息。
    fn from(output: ChatOutput) -> Self {
        Message::Assistant {
            content: output.content,
            reasoning: output.reasoning,
            tool_calls: output.tool_calls,
        }
    }
}

/// 模型适配器统一 trait。
///
/// 所有协议都收同一份中立的 [`Message`] 切片，内部借用翻译成 wire 格式。
pub trait Model {
    /// 非流式 chat，返回结构化结果（文本 + 工具调用 + 结束原因 + 用量）。
    fn chat(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ChatOutput, Error>> + Send;

    /// 流式 chat：建立连接后返回 chunk 流，逐帧产出增量（含累计用量）。
    fn chat_stream(
        &self,
        messages: &[Message],
        user_id: Option<&str>,
        response_format: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ChatStream, Error>> + Send;

    /// 拉取可用模型列表，写入自身 `model_list`。
    fn get_models(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{Message, ToolCall};

    #[test]
    fn message_serde_roundtrip_is_role_tagged() {
        let msg = Message::Assistant {
            content: None,
            reasoning: None,
            tool_calls: vec![ToolCall { id: "1".into(), name: "f".into(), arguments: "{}".into() }],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "assistant");
        assert!(json.get("content").is_none());
        assert_eq!(serde_json::from_value::<Message>(json).unwrap(), msg);
        assert_eq!(Message::tool("1", "ok").role(), "tool");
        assert_eq!(msg.text(), "");
    }
}
