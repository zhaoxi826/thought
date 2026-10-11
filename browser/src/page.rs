//! 导航、等待、快照。

use crate::actions::{Snapshot, SNAPSHOT_JS};
use crate::browser::Browser;
use crate::cdp::{CdpError, CdpEvent, CdpResult};
use serde_json::json;
use std::time::Duration;
use tokio::sync::broadcast::Receiver;

fn err(e: impl std::fmt::Display) -> CdpError {
    CdpError(e.to_string())
}

/// 等某个 session 的 `Page.loadEventFired`，最多 `limit`。
async fn wait_load(events: &mut Receiver<CdpEvent>, session: &str, limit: Duration) {
    let _ = tokio::time::timeout(limit, async {
        while let Ok(ev) = events.recv().await {
            if ev.method == "Page.loadEventFired" && ev.session_id.as_deref() == Some(session) {
                return;
            }
        }
    })
    .await;
}

impl Browser {
    /// 导航并等待加载完成（超时不算失败：很多页面永远在加载）。
    pub async fn navigate(&self, url: &str) -> CdpResult<String> {
        let session = self.session().await?;
        let mut events = self.cdp().subscribe();
        let r = self.cdp().call(Some(&session), "Page.navigate", json!({ "url": url })).await?;
        if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            return Err(err(format!("navigation failed: {e}")));
        }
        wait_load(&mut events, &session, Duration::from_secs(self.config.load_timeout_secs)).await;
        Ok(self.eval("location.href").await?.as_str().unwrap_or(url).to_owned())
    }

    /// 后退 / 前进 / 刷新：`back` / `forward` / `reload`。
    pub async fn history(&self, action: &str) -> CdpResult<String> {
        let session = self.session().await?;
        let mut events = self.cdp().subscribe();
        let js = match action {
            "back" => "history.back()",
            "forward" => "history.forward()",
            "reload" => "location.reload()",
            other => return Err(err(format!("unknown action {other:?}"))),
        };
        self.eval(js).await?;
        wait_load(&mut events, &session, Duration::from_secs(5)).await;
        Ok(self.eval("location.href").await?.as_str().unwrap_or("").to_owned())
    }

    /// 页面快照：可交互元素编号 + 可读结构。
    pub async fn snapshot(&self) -> CdpResult<Snapshot> {
        let js = SNAPSHOT_JS.replace("__MAX__", &self.config.max_text_chars.to_string());
        let v = self.eval(&js).await?;
        serde_json::from_value(v).map_err(err)
    }

    /// 等待：CSS 选择器出现、页面文本包含某段、或固定时长（毫秒）。
    pub async fn wait(&self, selector: Option<&str>, text: Option<&str>, ms: Option<u64>) -> CdpResult<String> {
        let limit = Duration::from_secs(self.config.load_timeout_secs);
        let started = std::time::Instant::now();
        if let Some(ms) = ms {
            tokio::time::sleep(Duration::from_millis(ms.min(60_000))).await;
        }
        if selector.is_none() && text.is_none() {
            return Ok("waited".into());
        }
        let probe = match (selector, text) {
            (Some(s), _) => format!("!!document.querySelector({})", json!(s)),
            (None, Some(t)) => format!("(document.body?.innerText || '').includes({})", json!(t)),
            _ => unreachable!(),
        };
        while started.elapsed() < limit {
            if self.eval(&probe).await?.as_bool() == Some(true) {
                return Ok("found".into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(err(format!("not found within {}s", limit.as_secs())))
    }
}
