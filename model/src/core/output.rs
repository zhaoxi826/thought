//! 协议中立的模型输出：完整结果、用量、工具调用、流式 chunk。

use serde::{Deserialize, Serialize};

/// 一次非流式 chat 的结构化结果（协议中立）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatOutput {
    /// 文本内容；模型只回工具调用、没有文本时为 `None`。
    pub content: Option<String>,

    /// 推理内容（DeepSeek 思考模式的 `reasoning_content` 等）。
    pub reasoning: Option<String>,

    /// 模型请求的工具调用。
    pub tool_calls: Vec<ToolCall>,

    /// 结束原因，协议原生字符串（OpenAI `stop` / `tool_calls`，
    /// Anthropic `end_turn` / `tool_use` / `max_tokens` 等）。
    pub finish_reason: String,

    /// token 用量。
    pub usage: Usage,
}

/// token 用量（协议中立；Anthropic 的 input/output 映射为 prompt/completion）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl Usage {
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }

    /// 合并一份流式用量快照。
    ///
    /// 各协议在流里报告的用量都是“截至当前的累计值”，且可能只带部分字段
    /// （Anthropic `message_start` 只有 input、`message_delta` 只有 output），
    /// 因此逐字段取较大值，`total` 至少为两者之和。
    pub fn merge(&mut self, other: Usage) {
        self.prompt_tokens = self.prompt_tokens.max(other.prompt_tokens);
        self.completion_tokens = self.completion_tokens.max(other.completion_tokens);
        self.total_tokens = self
            .total_tokens
            .max(other.total_tokens)
            .max(self.prompt_tokens + self.completion_tokens);
    }
}

/// 一次完整的工具调用（协议中立）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// 调用 id，回传工具结果时用它对应（OpenAI 的 tool_call_id / Anthropic 的 tool_use_id）。
    pub id: String,

    /// 工具名。
    pub name: String,

    /// 原始 JSON 参数串。
    pub arguments: String,
}

/// 流式工具调用增量。
///
/// 同一 `index` 的片段属于同一次调用：`id` / `name` 只在首个片段出现，
/// `arguments` 是 JSON 参数串的碎片，需要按序拼接（见 [`StreamAccumulator`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// 流式 chunk（协议中立）。
///
/// `content` / `reasoning` / `tool_calls` 为增量碎片；`finish_reason` 只在结束帧
/// 出现；`usage` 为截至当前的累计用量快照（可能只含部分字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatChunk {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}
