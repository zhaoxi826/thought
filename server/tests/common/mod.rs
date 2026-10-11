//! 测试共享件：脚本化的 OpenAI 兼容 mock 模型服务。
//!
//! 每个连接独立处理（支持并发请求），按请求体里的 `"model"` 字段把请求路由到
//! 对应的应答队列，依次返回预置的 SSE 流；队列耗尽时返回 500。所有请求按模型
//! 名记录下来供断言。

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Queues = Arc<Mutex<HashMap<String, VecDeque<String>>>>;
type Requests = Arc<Mutex<HashMap<String, Vec<String>>>>;

pub struct MockLlm {
    pub url: String,
    requests: Requests,
}

impl MockLlm {
    /// `script`：模型名 → 依次返回的 SSE 响应体。
    pub async fn start(script: Vec<(&str, Vec<String>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let queues: Queues = Arc::new(Mutex::new(
            script
                .into_iter()
                .map(|(model, bodies)| (model.to_owned(), bodies.into()))
                .collect(),
        ));
        let requests: Requests = Arc::default();

        let (q, r) = (Arc::clone(&queues), Arc::clone(&requests));
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (q, r) = (Arc::clone(&q), Arc::clone(&r));
                tokio::spawn(async move {
                    let request = read_request(&mut socket).await;
                    let model = extract_model(&request);
                    r.lock().unwrap().entry(model.clone()).or_default().push(request.clone());
                    let body = q.lock().unwrap().get_mut(&model).and_then(VecDeque::pop_front);
                    // 脚本里的 {{ID:名字}} 替换成请求上下文里"名字（id XXX）"的 id
                    let body = body.map(|b| substitute_ids(&b, &request));
                    let (status, body) = match body {
                        Some(body) => ("200 OK", body),
                        None => ("500 Internal Server Error", format!("script exhausted for {model}")),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body.as_bytes()).await;
                });
            }
        });

        Self { url, requests }
    }

    /// 发给某个模型的全部请求（含 HTTP 头）。
    pub fn requests(&self, model: &str) -> Vec<String> {
        self.requests.lock().unwrap().get(model).cloned().unwrap_or_default()
    }
}

/// 纯文本回复的 SSE 流（含用量帧）。
pub fn text_reply(text: &str) -> String {
    let content = serde_json::to_string(text).unwrap();
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}},\"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\n\
         data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}\n\n\
         data: [DONE]\n\n"
    )
}

/// 工具调用回复的 SSE 流：`calls` 为 (id, name, arguments JSON)。
pub fn tool_reply(calls: &[(&str, &str, &str)]) -> String {
    let mut body = String::new();
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let arguments = serde_json::to_string(arguments).unwrap();
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":\"{id}\",\
             \"function\":{{\"name\":\"{name}\",\"arguments\":{arguments}}}}}]}},\"finish_reason\":null}}]}}\n\n"
        ));
    }
    body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n");
    body
}

fn extract_model(request: &str) -> String {
    let body = &request[request.find("\r\n\r\n").map_or(0, |i| i + 4)..];
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["model"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// 读一个完整 HTTP 请求：读到头结束（\r\n\r\n），再按 content-length 读 body。
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = 0usize;

    loop {
        let n = socket.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);

        if header_end.is_none()
            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            header_end = Some(pos + 4);
            content_length = parse_content_length(&buf[..pos]);
        }
        if let Some(end) = header_end
            && buf.len() >= end + content_length
        {
            break;
        }
    }

    String::from_utf8_lossy(&buf).into_owned()
}

fn parse_content_length(headers: &[u8]) -> usize {
    String::from_utf8_lossy(headers)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

/// 把 `{{ID:name}}` 替换成请求正文中 `name（id XXX）` 里的 XXX。
fn substitute_ids(body: &str, request: &str) -> String {
    let mut out = body.to_owned();
    while let Some(start) = out.find("{{ID:") {
        let Some(end) = out[start..].find("}}") else { break };
        let name = &out[start + 5..start + end];
        let marker = format!("{name}（id ");
        let id = request
            .find(&marker)
            .map(|i| {
                let rest = &request[i + marker.len()..];
                rest[..rest.find('）').unwrap_or(0)].to_owned()
            })
            .unwrap_or_default();
        out.replace_range(start..start + end + 2, &id);
    }
    out
}
