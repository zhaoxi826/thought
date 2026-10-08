//! 极简 SSE（Server-Sent Events）增量解析与字节流 → 事件流的桥接。
//!
//! OpenAI 与 Anthropic 的流式响应都是 SSE：事件以空行分隔，每条事件由可选的
//! `event:` 行与一行或多行 `data:` 组成。这里只实现协议子集，不含重连与
//! `id:` / `retry:` 字段。

use crate::model::base::Error;
use futures::stream::{Stream, StreamExt};
use std::collections::VecDeque;

/// 一条 SSE 事件。`event` 缺省（OpenAI 协议不发 event 行）时为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// 增量 SSE 解析器：持续 [`push`](SseParser::push) 字节，
/// 用 [`next_event`](SseParser::next_event) 取出已完整的事件。
pub(crate) struct SseParser {
    buf: Vec<u8>,
    pending: VecDeque<SseEvent>,
    cur_event: Option<String>,
    cur_data: String,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            pending: VecDeque::new(),
            cur_event: None,
            cur_data: String::new(),
            has_data: false,
        }
    }

    /// 喂入一段字节。按 `\n` 切行——UTF-8 多字节序列中不会出现 `0x0A`，
    /// 所以按行切不会切断字符。
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            self.process_line(line.trim_end_matches(['\n', '\r']));
        }
    }

    /// 流结束时冲刷最后一行（无换行结尾）与未分发的事件。
    pub fn finish(&mut self) {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            self.process_line(line.trim_end_matches('\r'));
        }
        self.dispatch();
    }

    /// 取出下一条完整事件。
    pub fn next_event(&mut self) -> Option<SseEvent> {
        self.pending.pop_front()
    }

    fn process_line(&mut self, line: &str) {
        if line.is_empty() {
            // 空行 = 事件边界
            self.dispatch();
        } else if let Some(v) = line.strip_prefix("event:") {
            self.cur_event = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("data:") {
            // SSE 规范：去掉 data: 后的至多一个空格；多行 data 以 \n 拼接
            if self.has_data {
                self.cur_data.push('\n');
            }
            self.cur_data.push_str(v.strip_prefix(' ').unwrap_or(v));
            self.has_data = true;
        }
        // 其余行（comment、id:、retry: 等）忽略
    }

    fn dispatch(&mut self) {
        if self.has_data {
            self.pending.push_back(SseEvent {
                event: self.cur_event.take(),
                data: std::mem::take(&mut self.cur_data),
            });
            self.has_data = false;
        } else {
            // 没有 data 的“事件”不算数，但 event 行状态要清掉
            self.cur_event = None;
        }
    }
}

/// 把 HTTP 字节流桥接成 SSE 事件流。
///
/// `finish` 之后流耗尽即结束；单条事件解析失败不会中断后续事件
/// （解析失败体现在上层把 `data` 反序列化为协议结构时）。
pub(crate) fn sse_events<B, S>(byte_stream: S) -> impl Stream<Item = Result<SseEvent, Error>> + Send
where
    B: AsRef<[u8]> + Send,
    S: Stream<Item = Result<B, reqwest::Error>> + Send + 'static,
{
    let byte_stream = Box::pin(byte_stream);
    futures::stream::try_unfold(
        (byte_stream, SseParser::new(), false),
        |(mut bytes, mut parser, mut eof)| async move {
            loop {
                if let Some(event) = parser.next_event() {
                    return Ok(Some((event, (bytes, parser, eof))));
                }
                if eof {
                    return Ok(None);
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => parser.push(chunk.as_ref()),
                    Some(Err(e)) => return Err(Error::Http(e)),
                    None => {
                        parser.finish();
                        eof = true;
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(mut parser: SseParser) -> Vec<SseEvent> {
        let mut out = Vec::new();
        while let Some(ev) = parser.next_event() {
            out.push(ev);
        }
        out
    }

    #[test]
    fn parses_openai_style_events() {
        let mut p = SseParser::new();
        p.push(b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n");
        p.finish();
        let events = collect(p);
        assert_eq!(
            events,
            vec![
                SseEvent { event: None, data: "{\"a\":1}".into() },
                SseEvent { event: None, data: "{\"b\":2}".into() },
                SseEvent { event: None, data: "[DONE]".into() },
            ]
        );
    }

    #[test]
    fn parses_event_lines_and_ignores_comments() {
        let mut p = SseParser::new();
        p.push(b"event: message_start\ndata: {}\n\n: ping comment\nevent: content_block_delta\ndata: {\"x\":1}\n\n");
        p.finish();
        let events = collect(p);
        assert_eq!(
            events,
            vec![
                SseEvent { event: Some("message_start".into()), data: "{}".into() },
                SseEvent { event: Some("content_block_delta".into()), data: "{\"x\":1}".into() },
            ]
        );
    }

    #[test]
    fn handles_fragmented_input_and_crlf() {
        let mut p = SseParser::new();
        p.push(b"data: {\"a\"");
        assert!(p.next_event().is_none());
        p.push(b":1}\r\n");
        assert!(p.next_event().is_none());
        p.push(b"\r\n");
        p.finish();
        assert_eq!(collect(p), vec![SseEvent { event: None, data: "{\"a\":1}".into() }]);
    }

    #[test]
    fn multibyte_utf8_across_chunks() {
        let mut p = SseParser::new();
        let full = "data: 你好\n\n".as_bytes();
        // 7 落在"你"的三字节序列中间，验证按行解码不会切断多字节字符
        let split = 7;
        p.push(&full[..split]);
        p.push(&full[split..]);
        p.finish();
        assert_eq!(collect(p), vec![SseEvent { event: None, data: "你好".into() }]);
    }

    #[test]
    fn flushes_final_event_without_trailing_blank_line() {
        let mut p = SseParser::new();
        p.push(b"data: last");
        p.finish();
        assert_eq!(collect(p), vec![SseEvent { event: None, data: "last".into() }]);
    }
}
