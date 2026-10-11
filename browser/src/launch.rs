//! 启动她自己的 Chromium 进程。

use crate::browser::BrowserConfig;
use crate::cdp::{CdpError, CdpResult};
use serde_json::Value;
use std::process::Stdio;

/// 启动 Chromium：只监听 127.0.0.1，profile / 下载目录在她的工作区。
pub fn launch(config: &BrowserConfig) -> CdpResult<tokio::process::Child> {
    std::fs::create_dir_all(&config.profile_dir).map_err(|e| CdpError(e.to_string()))?;
    std::fs::create_dir_all(&config.download_dir).map_err(|e| CdpError(e.to_string()))?;
    let mut cmd = tokio::process::Command::new(&config.executable);
    cmd.arg(format!("--remote-debugging-port={}", config.port))
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--user-data-dir={}", config.profile_dir.display()))
        .arg(format!("--window-size={},{}", config.width, config.height))
        .args(["--no-first-run", "--no-default-browser-check", "--disable-dev-shm-usage", "--password-store=basic"]);
    if config.headless {
        cmd.arg("--headless=new");
    }
    cmd.args(&config.args).arg("about:blank");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| CdpError(format!("launch {}: {e}", config.executable.display())))
}

/// 从 `http://host:port` 读出浏览器的 WebSocket 地址。
pub async fn ws_url(http: &str) -> CdpResult<String> {
    let v: Value = reqwest::get(format!("{}/json/version", http.trim_end_matches('/')))
        .await
        .map_err(|e| CdpError(e.to_string()))?
        .json()
        .await
        .map_err(|e| CdpError(e.to_string()))?;
    v["webSocketDebuggerUrl"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| CdpError("no webSocketDebuggerUrl".into()))
}
