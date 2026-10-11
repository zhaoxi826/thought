//! 浏览器会话：连接 / 启动、标签页管理、在当前标签页执行 JS。

use crate::browser::{Browser, BrowserConfig, State, TabInfo};
use crate::cdp::{Cdp, CdpError, CdpResult};
use crate::launch::{launch, ws_url};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

fn err(e: impl std::fmt::Display) -> CdpError {
    CdpError(e.to_string())
}

impl Browser {
    /// 按配置连接已有浏览器，或启动她自己的 Chromium。
    pub async fn start(config: BrowserConfig) -> CdpResult<Self> {
        let timeout = Duration::from_secs(config.command_timeout_secs.max(1));
        let (http, child) = match &config.connect {
            Some(url) => (url.clone(), None),
            None => (format!("http://127.0.0.1:{}", config.port), Some(launch(&config)?)),
        };
        // 冷启动在负载高时可能要十几秒（测试并行时尤其如此）
        let mut ws = Err(err("browser did not start"));
        for _ in 0..300 {
            ws = ws_url(&http).await;
            if ws.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let cdp = Cdp::connect(&ws?, timeout).await?;
        // 下载落到她的下载目录
        let _ = cdp
            .call(
                None,
                "Browser.setDownloadBehavior",
                json!({ "behavior": "allow", "downloadPath": config.download_dir, "eventsEnabled": true }),
            )
            .await;
        let browser = Self {
            cdp,
            state: Arc::new(Mutex::new(State { tabs: BTreeMap::new(), active: None })),
            config: Arc::new(config),
            child: Arc::new(Mutex::new(child)),
        };
        // 新标签（含页面自己弹出的）都能被发现
        let _ = browser.cdp.call(None, "Target.setDiscoverTargets", json!({ "discover": true })).await;
        // 接管已有的页面标签；一个都没有就开一个
        let targets = browser.cdp.call(None, "Target.getTargets", json!({})).await?;
        let pages: Vec<String> = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == "page")
            .filter_map(|t| t["targetId"].as_str().map(str::to_owned))
            .collect();
        for id in &pages {
            browser.attach(id).await?;
        }
        if pages.is_empty() {
            browser.new_tab("about:blank").await?;
        }
        Ok(browser)
    }

    pub fn is_alive(&self) -> bool {
        self.cdp.is_alive()
    }

    /// 连接断了（Chromium 崩溃 / 被杀）：重新拉起她自己的 Chromium 并重连，标签页重新接管。
    /// 连接的是外部浏览器（`connect`）时只尝试重连。profile 在磁盘上，登录态不丢。
    pub async fn ensure_alive(&self) -> CdpResult<()> {
        if self.cdp.is_alive() {
            return Ok(());
        }
        let mut child = self.child.lock().await;
        if self.cdp.is_alive() {
            return Ok(()); // 别的调用已经重启好了
        }
        tracing::warn!("browser connection lost; restarting");
        let http = match &self.config.connect {
            Some(url) => url.clone(),
            None => {
                if let Some(mut old) = child.take() {
                    let _ = old.kill().await;
                }
                *child = Some(launch(&self.config)?);
                format!("http://127.0.0.1:{}", self.config.port)
            }
        };
        let mut ws = Err(err("browser did not restart"));
        for _ in 0..300 {
            ws = ws_url(&http).await;
            if ws.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.cdp.reconnect(&ws?).await?;
        drop(child);
        let _ = self
            .cdp
            .call(
                None,
                "Browser.setDownloadBehavior",
                json!({ "behavior": "allow", "downloadPath": self.config.download_dir, "eventsEnabled": true }),
            )
            .await;
        let _ = self.cdp.call(None, "Target.setDiscoverTargets", json!({ "discover": true })).await;
        {
            let mut st = self.state.lock().await;
            st.tabs.clear();
            st.active = None;
        }
        Ok(())
    }

    pub fn cdp(&self) -> &Cdp {
        &self.cdp
    }

    async fn attach(&self, target: &str) -> CdpResult<String> {
        let r = self.cdp.call(None, "Target.attachToTarget", json!({ "targetId": target, "flatten": true })).await?;
        let session = r["sessionId"].as_str().ok_or_else(|| err("attach: no sessionId"))?.to_owned();
        for domain in ["Page.enable", "Runtime.enable", "Network.enable", "DOM.enable"] {
            let _ = self.cdp.call(Some(&session), domain, json!({})).await;
        }
        let _ = self
            .cdp
            .call(
                Some(&session),
                "Emulation.setDeviceMetricsOverride",
                json!({ "width": self.config.width, "height": self.config.height, "deviceScaleFactor": 1, "mobile": false }),
            )
            .await;
        let mut st = self.state.lock().await;
        st.tabs.insert(target.to_owned(), session.clone());
        st.active.get_or_insert_with(|| target.to_owned());
        Ok(session)
    }

    /// 新开标签页并设为当前，返回 target id。
    pub async fn new_tab(&self, url: &str) -> CdpResult<String> {
        let r = self.cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" })).await?;
        let id = r["targetId"].as_str().ok_or_else(|| err("createTarget: no targetId"))?.to_owned();
        self.attach(&id).await?;
        self.state.lock().await.active = Some(id.clone());
        if url != "about:blank" {
            self.navigate(url).await?;
        }
        Ok(id)
    }

    pub async fn switch_tab(&self, id: &str) -> CdpResult<()> {
        let mut st = self.state.lock().await;
        if !st.tabs.contains_key(id) {
            return Err(err(format!("no tab {id}")));
        }
        st.active = Some(id.to_owned());
        drop(st);
        let _ = self.cdp.call(None, "Target.activateTarget", json!({ "targetId": id })).await;
        Ok(())
    }

    pub async fn close_tab(&self, id: &str) -> CdpResult<()> {
        self.cdp.call(None, "Target.closeTarget", json!({ "targetId": id })).await?;
        let mut st = self.state.lock().await;
        st.tabs.remove(id);
        if st.active.as_deref() == Some(id) {
            st.active = st.tabs.keys().next().cloned();
        }
        let none_left = st.tabs.is_empty();
        drop(st);
        if none_left {
            self.new_tab("about:blank").await?;
        }
        Ok(())
    }

    pub async fn tabs(&self) -> CdpResult<Vec<TabInfo>> {
        let targets = self.cdp.call(None, "Target.getTargets", json!({})).await?;
        let st = self.state.lock().await;
        Ok(targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == "page")
            .filter_map(|t| {
                let id = t["targetId"].as_str()?.to_owned();
                st.tabs.contains_key(&id).then(|| TabInfo {
                    active: st.active.as_deref() == Some(&id),
                    url: t["url"].as_str().unwrap_or("").to_owned(),
                    title: t["title"].as_str().unwrap_or("").to_owned(),
                    id,
                })
            })
            .collect())
    }

    /// 当前标签页的 session。顺带同步标签页：接管页面自己弹出的新标签，
    /// 移除已经关闭 / 崩溃的标签（当前标签没了就换到另一个，一个都没有就新开一个）。
    pub async fn session(&self) -> CdpResult<String> {
        self.ensure_alive().await?;
        self.sync_tabs().await;
        {
            let st = self.state.lock().await;
            if let Some(active) = &st.active
                && let Some(session) = st.tabs.get(active)
            {
                return Ok(session.clone());
            }
        }
        // 一个标签都不剩：开一个空白页（不经 new_tab，避免 navigate → session 递归）
        let r = self.cdp.call(None, "Target.createTarget", json!({ "url": "about:blank" })).await?;
        let id = r["targetId"].as_str().ok_or_else(|| err("createTarget: no targetId"))?.to_owned();
        let session = self.attach(&id).await?;
        self.state.lock().await.active = Some(id);
        Ok(session)
    }

    async fn sync_tabs(&self) {
        let Ok(targets) = self.cdp.call(None, "Target.getTargets", json!({})).await else { return };
        let live: Vec<String> = targets["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == "page")
            .filter_map(|t| t["targetId"].as_str().map(str::to_owned))
            .collect();
        let known: Vec<String> = {
            let mut st = self.state.lock().await;
            st.tabs.retain(|id, _| live.contains(id));
            if st.active.as_ref().is_some_and(|a| !st.tabs.contains_key(a)) {
                st.active = st.tabs.keys().next().cloned();
            }
            st.tabs.keys().cloned().collect()
        };
        for id in live.iter().filter(|id| !known.contains(id)) {
            let _ = self.attach(id).await;
        }
    }

    /// 在当前页面求值；返回 JSON 值。页面异常作为错误返回。
    pub async fn eval(&self, expression: &str) -> CdpResult<Value> {
        let session = self.session().await?;
        let r = self
            .cdp
            .call(
                Some(&session),
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let msg = ex["exception"]["description"].as_str().or(ex["text"].as_str()).unwrap_or("exception");
            return Err(err(format!("page error: {msg}")));
        }
        Ok(r["result"]["value"].clone())
    }
}
