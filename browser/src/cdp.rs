//! 最小 CDP 客户端：一条 WebSocket 连到浏览器端点，用 flat session（`sessionId`）
//! 同时驱动所有标签页。命令按 id 配对响应；事件广播给订阅者。

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// CDP 错误：一段人能读懂的话（会作为工具输出交给模型）。
#[derive(Debug, Clone)]
pub struct CdpError(pub String);

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CdpError {}

pub type CdpResult<T> = Result<T, CdpError>;

/// 一个 CDP 事件。
#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<CdpResult<Value>>>>>;

/// 到浏览器的一条 CDP 连接。`Clone` 廉价；连接断开后所有命令返回错误。
#[derive(Clone)]
pub struct Cdp {
    inner: Arc<std::sync::RwLock<Conn>>,
}

/// 一条具体的连接；浏览器重启后整体替换。
#[derive(Clone)]
struct Conn {
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    events: broadcast::Sender<CdpEvent>,
    alive: Arc<AtomicBool>,
    timeout: Duration,
}

impl Cdp {
    /// 连接 `ws://…/devtools/browser/…`。`timeout` 是单条命令的默认超时。
    pub async fn connect(ws_url: &str, timeout: Duration) -> CdpResult<Self> {
        Ok(Self { inner: Arc::new(std::sync::RwLock::new(Conn::connect(ws_url, timeout).await?)) })
    }

    /// 换成一条新连接（浏览器重启后）。旧连接的订阅者会收到 Closed。
    pub async fn reconnect(&self, ws_url: &str) -> CdpResult<()> {
        let timeout = self.conn().timeout;
        let conn = Conn::connect(ws_url, timeout).await?;
        *self.inner.write().expect("cdp poisoned") = conn;
        Ok(())
    }

    fn conn(&self) -> Conn {
        self.inner.read().expect("cdp poisoned").clone()
    }

    pub fn is_alive(&self) -> bool {
        self.conn().alive.load(Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.conn().events.subscribe()
    }

    /// 发一条命令。`session` 为 `None` 时发给浏览器本身。
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> CdpResult<Value> {
        let conn = self.conn();
        let timeout = conn.timeout;
        conn.call_timeout(session, method, params, timeout).await
    }

    pub async fn call_timeout(&self, session: Option<&str>, method: &str, params: Value, timeout: Duration) -> CdpResult<Value> {
        self.conn().call_timeout(session, method, params, timeout).await
    }
}

impl Conn {
    async fn connect(ws_url: &str, timeout: Duration) -> CdpResult<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CdpError(format!("connect {ws_url}: {e}")))?;
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        let (events, _) = broadcast::channel(4096);
        let alive = Arc::new(AtomicBool::new(true));

        tokio::spawn(async move {
            while let Some(text) = rx.recv().await {
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });

        let (p, ev, a) = (Arc::clone(&pending), events.clone(), Arc::clone(&alive));
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    let result = match v.get("error") {
                        Some(err) => Err(CdpError(format!(
                            "{} ({})",
                            err["message"].as_str().unwrap_or("cdp error"),
                            err["data"].as_str().unwrap_or("")
                        ))),
                        None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    if let Some(reply) = p.lock().expect("pending poisoned").remove(&id) {
                        let _ = reply.send(result);
                    }
                } else if let Some(method) = v.get("method").and_then(Value::as_str) {
                    let _ = ev.send(CdpEvent {
                        method: method.to_owned(),
                        params: v.get("params").cloned().unwrap_or(Value::Null),
                        session_id: v.get("sessionId").and_then(Value::as_str).map(str::to_owned),
                    });
                }
            }
            a.store(false, Ordering::Relaxed);
            // 连接断了：让所有等待中的命令立刻失败
            for (_, reply) in p.lock().expect("pending poisoned").drain() {
                let _ = reply.send(Err(CdpError("browser connection closed".into())));
            }
        });

        Ok(Self { tx, pending, next_id: Arc::new(AtomicU64::new(1)), events, alive, timeout })
    }

    async fn call_timeout(&self, session: Option<&str>, method: &str, params: Value, timeout: Duration) -> CdpResult<Value> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err(CdpError("browser connection closed".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (reply, rx) = oneshot::channel();
        self.pending.lock().expect("pending poisoned").insert(id, reply);
        if self.tx.send(msg.to_string()).is_err() {
            self.pending.lock().expect("pending poisoned").remove(&id);
            return Err(CdpError("browser connection closed".into()));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CdpError("browser connection closed".into())),
            Err(_) => {
                self.pending.lock().expect("pending poisoned").remove(&id);
                Err(CdpError(format!("{method} timed out after {}s", timeout.as_secs())))
            }
        }
    }
}
