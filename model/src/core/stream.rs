//! 流式结果：chunk 流类型与累加器。

use super::error::Error;
use super::output::{ChatChunk, ChatOutput, ToolCall, Usage};

/// 流式 chat 的 chunk 流。
pub type ChatStream = futures::stream::BoxStream<'static, Result<ChatChunk, Error>>;

/// 流式累加器：把一串 [`ChatChunk`] 拼回完整的 [`ChatOutput`]。
///
/// 调用方可以边转发 chunk（如推给前端）边 [`push`](Self::push)，流结束后
/// [`finish`](Self::finish) 得到与非流式 `chat` 同构的结果。工具调用按 `index`
/// 归并，与碎片到达顺序无关。
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: std::collections::BTreeMap<u32, PartialToolCall>,
    finish_reason: Option<String>,
    usage: Usage,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 并入一个 chunk。
    pub fn push(&mut self, chunk: &ChatChunk) {
        if let Some(text) = &chunk.content {
            self.content.push_str(text);
        }
        if let Some(text) = &chunk.reasoning {
            self.reasoning.push_str(text);
        }
        for delta in &chunk.tool_calls {
            let call = self.tool_calls.entry(delta.index).or_default();
            if let Some(id) = &delta.id {
                call.id.clone_from(id);
            }
            if let Some(name) = &delta.name {
                call.name.clone_from(name);
            }
            if let Some(args) = &delta.arguments {
                call.arguments.push_str(args);
            }
        }
        if let Some(reason) = &chunk.finish_reason {
            self.finish_reason = Some(reason.clone());
        }
        if let Some(usage) = chunk.usage {
            self.usage.merge(usage);
        }
    }

    /// 结束累加，产出完整结果。
    ///
    /// 没有任何文本也没有工具调用时返回 [`Error::EmptyResponse`]；工具调用缺少
    /// 名字视为协议错误。空参数串规范化为 `{}`。
    pub fn finish(self) -> Result<ChatOutput, Error> {
        let mut tool_calls = Vec::with_capacity(self.tool_calls.len());
        for (index, call) in self.tool_calls {
            if call.name.is_empty() {
                return Err(Error::Protocol(format!("tool call #{index} has no name")));
            }
            tool_calls.push(ToolCall {
                id: call.id,
                name: call.name,
                arguments: if call.arguments.is_empty() {
                    "{}".to_owned()
                } else {
                    call.arguments
                },
            });
        }
        if self.content.is_empty() && tool_calls.is_empty() {
            return Err(Error::EmptyResponse);
        }
        Ok(ChatOutput {
            content: non_empty(self.content),
            reasoning: non_empty(self.reasoning),
            tool_calls,
            finish_reason: self.finish_reason.unwrap_or_default(),
            usage: self.usage,
        })
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// 把整条流消费完并拼成 [`ChatOutput`]；`on_chunk` 在每个 chunk 到达时回调
/// （用于实时转发），不需要时传 `|_| {}`。
pub async fn collect_stream(
    mut stream: ChatStream,
    mut on_chunk: impl FnMut(&ChatChunk),
) -> Result<ChatOutput, Error> {
    use futures::StreamExt;

    let mut acc = StreamAccumulator::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        on_chunk(&chunk);
        acc.push(&chunk);
    }
    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::output::ToolCallDelta;

    #[test]
    fn usage_merge_takes_partial_snapshots() {
        let mut usage = Usage::default();
        usage.merge(Usage { prompt_tokens: 11, completion_tokens: 1, total_tokens: 0 });
        usage.merge(Usage { prompt_tokens: 0, completion_tokens: 7, total_tokens: 0 });
        assert_eq!(usage, Usage::new(11, 7));
    }

    #[test]
    fn accumulator_merges_text_tool_calls_and_usage() {
        let chunks = [
            ChatChunk { content: Some("he".into()), ..Default::default() },
            ChatChunk { reasoning: Some("think".into()), content: Some("llo".into()), ..Default::default() },
            // 两个工具调用交错到达，index 1 先于 0 出现
            ChatChunk {
                tool_calls: vec![ToolCallDelta { index: 1, id: Some("b".into()), name: Some("g".into()), arguments: None }],
                ..Default::default()
            },
            ChatChunk {
                tool_calls: vec![
                    ToolCallDelta { index: 0, id: Some("a".into()), name: Some("f".into()), arguments: Some("{\"x\"".into()) },
                    ToolCallDelta { index: 0, arguments: Some(":1}".into()), ..Default::default() },
                ],
                ..Default::default()
            },
            ChatChunk {
                finish_reason: Some("tool_calls".into()),
                usage: Some(Usage::new(3, 4)),
                ..Default::default()
            },
        ];
        let mut acc = StreamAccumulator::new();
        for c in &chunks {
            acc.push(c);
        }
        let out = acc.finish().unwrap();
        assert_eq!(out.content.as_deref(), Some("hello"));
        assert_eq!(out.reasoning.as_deref(), Some("think"));
        assert_eq!(out.finish_reason, "tool_calls");
        assert_eq!(out.usage.total_tokens, 7);
        assert_eq!(
            out.tool_calls,
            vec![
                ToolCall { id: "a".into(), name: "f".into(), arguments: "{\"x\":1}".into() },
                // 没有参数碎片的调用规范化为 {}
                ToolCall { id: "b".into(), name: "g".into(), arguments: "{}".into() },
            ]
        );
    }

    #[test]
    fn accumulator_rejects_empty_and_nameless() {
        assert!(matches!(StreamAccumulator::new().finish(), Err(Error::EmptyResponse)));

        let mut acc = StreamAccumulator::new();
        acc.push(&ChatChunk {
            tool_calls: vec![ToolCallDelta { index: 0, arguments: Some("{}".into()), ..Default::default() }],
            ..Default::default()
        });
        assert!(matches!(acc.finish(), Err(Error::Protocol(_))));
    }
}
