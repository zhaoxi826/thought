//! 测试共享件：一次性迷你 HTTP server。
//!
//! 绑定随机端口，接受一个连接，读完整请求（头 + body），写回预置响应，
//! 然后把捕获到的原始请求文本通过 oneshot 通道交还给测试断言。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub struct MockResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: &'static str,
}

impl MockResponse {
    pub fn json(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body,
        }
    }

    pub fn sse(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body,
        }
    }

    #[allow(dead_code)]
    pub fn error(status: u16, body: &'static str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body,
        }
    }
}

/// 启动一次性 server，返回 (URL, 请求接收通道)。
pub async fn serve_once(response: MockResponse) -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;

        let head = format!(
            "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.status,
            response.content_type,
            response.body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(response.body.as_bytes()).await.unwrap();

        let _ = tx.send(request);
    });

    (url, rx)
}

/// 读一个完整 HTTP 请求：读到头结束（\r\n\r\n），再按 content-length 读 body。
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = 0usize;

    loop {
        let n = socket.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);

        if header_end.is_none()
            && let Some(pos) = find_subslice(&buf, b"\r\n\r\n")
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

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
