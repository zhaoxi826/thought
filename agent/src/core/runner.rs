//! ReAct 执行器：三层 agent 共用的「模型 → 工具 → 模型」循环。

use crate::core::event::{AgentEvent, EventSender};
use crate::AgentError;
use futures::future::join_all;
use model::{collect_stream, ChatChunk, Message, Model, ToolCall};
use plugin::Registry;

/// 一个 agent 的执行器：持有（已挂好工具的）模型与工具注册表。
#[derive(Clone)]
pub struct Runner {
    /// 事件里的发出者标签，如 `controller` / `coder#3/sub1`。
    pub label: String,
    model: Model,
    registry: Registry,
    max_turns: usize,
    max_tool_output_chars: usize,
    events: EventSender,
}

impl Runner {
    /// 创建执行器：把注册表里的工具定义装到模型上。
    pub fn new(
        label: impl Into<String>,
        mut model: Model,
        registry: Registry,
        max_turns: usize,
        max_tool_output_chars: usize,
        events: EventSender,
    ) -> Self {
        model.set_tools(&registry.definitions());
        Self {
            label: label.into(),
            model,
            registry,
            max_turns,
            max_tool_output_chars,
            events,
        }
    }

    /// 在 `history` 上跑循环，直到模型给出不带工具调用的回复，返回其文本。
    ///
    /// 每轮流式调用模型（文本碎片实时作为事件发出），assistant 消息与工具结果
    /// 依次追加进 `history`。同一轮的多个工具调用并发执行，结果按调用顺序回传。
    /// 即使中途出错，`history` 也保持协议合法（不会留下没有结果的工具调用）。
    pub async fn run(&self, history: &mut Vec<Message>) -> Result<String, AgentError> {
        for _ in 0..self.max_turns {
            let stream = self.model.chat_stream(history, None, None).await?;
            let output = collect_stream(stream, |chunk| self.forward(chunk)).await?;
            self.emit(AgentEvent::Usage {
                agent: self.label.clone(),
                usage: output.usage.into(),
            });

            if output.tool_calls.is_empty() {
                let text = output.content.clone().unwrap_or_default();
                history.push(output.into());
                return Ok(text);
            }

            let calls = output.tool_calls.clone();
            history.push(output.into());
            let results = join_all(calls.iter().map(|call| self.call_tool(call))).await;
            for (call, result) in calls.into_iter().zip(results) {
                history.push(Message::tool(call.id, result));
            }
        }
        Err(AgentError::MaxTurnsExceeded)
    }

    async fn call_tool(&self, call: &ToolCall) -> String {
        self.emit(AgentEvent::ToolCall {
            agent: self.label.clone(),
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
        let output = match self.registry.call(&call.name, &call.arguments).await {
            Some(output) => truncate(output, self.max_tool_output_chars),
            None => format!("unknown tool: {}", call.name),
        };
        self.emit(AgentEvent::ToolResult {
            agent: self.label.clone(),
            id: call.id.clone(),
            name: call.name.clone(),
            output: output.clone(),
        });
        output
    }

    fn forward(&self, chunk: &ChatChunk) {
        if let Some(text) = &chunk.content {
            self.emit(AgentEvent::TextDelta {
                agent: self.label.clone(),
                text: text.clone(),
            });
        }
        if let Some(text) = &chunk.reasoning {
            self.emit(AgentEvent::ReasoningDelta {
                agent: self.label.clone(),
                text: text.clone(),
            });
        }
    }

    pub(crate) fn emit(&self, event: AgentEvent) {
        self.events.send(event);
    }
}

/// 按字符数截断（不会切坏 UTF-8），并注明截掉了多少。
pub(crate) fn truncate(mut text: String, max_chars: usize) -> String {
    let Some((cut, _)) = text.char_indices().nth(max_chars) else {
        return text;
    };
    let dropped = text[cut..].chars().count();
    text.truncate(cut);
    text.push_str(&format!("\n...[truncated {dropped} chars]"));
    text
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("你好世界".into(), 10), "你好世界");
        assert_eq!(truncate("你好世界".into(), 2), "你好\n...[truncated 2 chars]");
    }
}
