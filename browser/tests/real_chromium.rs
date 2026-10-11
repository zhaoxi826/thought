//! 用真实 Chromium 跑通全部工具（本地 HTTP 页面，不联网）。没有 chromium 时跳过。

use browser::{register_browser_tools, Browser, BrowserConfig};
use plugin::Registry;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PAGE: &str = r#"<!doctype html><html><head><title>测试页</title></head><body>
<h1>你好，思想</h1>
<label for="q">搜索</label><input id="q" placeholder="搜点什么">
<button id="go" onclick="document.getElementById('out').textContent = '你搜了：' + document.getElementById('q').value">搜索</button>
<p id="out"></p>
<select id="color"><option value="r">红</option><option value="b">蓝</option></select>
<input type="file" id="f" onchange="document.getElementById('fname').textContent = this.files[0].name">
<span id="fname"></span>
<a href="/second" target="_blank">在新标签打开</a>
<div style="height:3000px"></div><p id="bottom">到底了</p>
<script>document.addEventListener('keydown', e => { if (e.key === 'Escape') document.title = 'escaped'; });</script>
</body></html>"#;

/// 极简 HTTP：`/` 返回测试页，`/second` 返回第二页，`/file.txt` 触发下载。
async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (ctype, extra, body) = match path {
                    "/second" => ("text/html; charset=utf-8", "", "<title>第二页</title><p>second</p>".to_owned()),
                    "/file.txt" => ("text/plain", "content-disposition: attachment; filename=\"hello.txt\"\r\n", "hello download".to_owned()),
                    _ => ("text/html; charset=utf-8", "", PAGE.to_owned()),
                };
                let head = format!("HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(body.as_bytes()).await;
            });
        }
    });
    url
}

fn chromium() -> Option<PathBuf> {
    ["chromium", "chromium-browser", "google-chrome"]
        .iter()
        .filter_map(|n| std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(n)).find(|p| p.is_file())))
        .next()
}

async fn call(r: &Registry, tool: &str, args: serde_json::Value) -> String {
    r.call(tool, &args.to_string()).await.unwrap_or_else(|| panic!("no tool {tool}"))
}

#[tokio::test]
async fn she_can_drive_a_real_chromium() {
    let Some(exe) = chromium() else {
        eprintln!("no chromium; skipping");
        return;
    };
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("browser-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = BrowserConfig {
        executable: exe,
        profile_dir: root.join("profile"),
        download_dir: root.join("downloads"),
        port: 19333 + (std::process::id() % 500) as u16,
        // 容器 / root 环境下 chromium 需要
        args: vec!["--no-sandbox".into()],
        ..Default::default()
    };
    let browser = Browser::start(config).await.expect("start chromium");
    let mut tools = Registry::new();
    register_browser_tools(&mut tools, browser.clone());
    let site = serve().await;

    // 导航 + 快照
    let r = call(&tools, "browser_navigate", serde_json::json!({ "url": site })).await;
    assert!(r.contains("测试页") && r.contains("# 你好，思想"), "{r}");
    let input_ref = r.lines().find(|l| l.contains("input:text")).and_then(|l| l.split(['[', ']']).nth(1)).unwrap().to_owned();
    let button_ref = r.lines().find(|l| l.contains("button \"搜索\"")).and_then(|l| l.split(['[', ']']).nth(1)).unwrap().to_owned();

    // 输入 + 点击（真实事件）
    call(&tools, "browser_type", serde_json::json!({ "ref": input_ref.parse::<u64>().unwrap(), "text": "星星" })).await;
    let r = call(&tools, "browser_click", serde_json::json!({ "ref": button_ref.parse::<u64>().unwrap() })).await;
    assert!(r.contains("你搜了：星星"), "{r}");

    // 选择器、select、按键
    let r = call(&tools, "browser_select", serde_json::json!({ "selector": "#color", "value": "蓝" })).await;
    assert!(r.contains("selected 蓝"), "{r}");
    call(&tools, "browser_press", serde_json::json!({ "key": "Escape" })).await;
    assert_eq!(call(&tools, "browser_eval", serde_json::json!({ "script": "document.title" })).await, "escaped");

    // 滚动、文本、等待
    let r = call(&tools, "browser_scroll", serde_json::json!({ "dy": 5000 })).await;
    assert!(!r.contains("scroll 0/"), "{r}");
    assert!(call(&tools, "browser_text", serde_json::json!({ "selector": "#bottom" })).await.contains("到底了"));
    assert!(call(&tools, "browser_wait", serde_json::json!({ "text": "到底了" })).await.starts_with("found"));

    // 上传
    let file = root.join("upload-me.txt");
    std::fs::write(&file, "x").unwrap();
    call(&tools, "browser_upload", serde_json::json!({ "selector": "#f", "files": [file] })).await;
    assert_eq!(call(&tools, "browser_eval", serde_json::json!({ "script": "document.getElementById('fname').textContent" })).await, "upload-me.txt");

    // 截图（视口 / 元素）
    let r = call(&tools, "browser_screenshot", serde_json::json!({})).await;
    let path = r.trim_start_matches("saved ").to_owned();
    assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG"), "{r}");
    assert!(call(&tools, "browser_screenshot", serde_json::json!({ "selector": "h1" })).await.starts_with("saved"));

    // cookie
    call(&tools, "browser_cookies", serde_json::json!({ "action": "set", "cookie": { "name": "k", "value": "v", "url": site } })).await;
    assert!(call(&tools, "browser_cookies", serde_json::json!({ "action": "get" })).await.contains("\"name\":\"k\""));

    // 页面弹出的新标签被接管；多标签切换
    call(&tools, "browser_scroll", serde_json::json!({ "dy": -10000 })).await;
    call(&tools, "browser_click", serde_json::json!({ "selector": "a" })).await;
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    let tabs = call(&tools, "browser_tabs", serde_json::json!({})).await;
    assert!(tabs.lines().count() >= 2, "{tabs}");
    let r = call(&tools, "browser_tab_new", serde_json::json!({ "url": format!("{site}/second") })).await;
    assert!(r.contains("第二页"), "{r}");
    // 测试页那个标签（url 以 / 结尾）
    let first = tabs.lines().find(|l| l.trim_end().ends_with(&format!("({site}/)"))).unwrap().split_whitespace().nth(1).unwrap().to_owned();
    let r = call(&tools, "browser_tab_switch", serde_json::json!({ "id": first })).await;
    assert!(r.contains(&format!("— {site}/")), "{r}");

    // 下载落到她的下载目录
    call(&tools, "browser_navigate", serde_json::json!({ "url": format!("{site}/file.txt") })).await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let d = call(&tools, "browser_downloads", serde_json::json!({})).await;
    assert!(d.contains("hello.txt"), "{d}");

    // 原始 CDP：未封装的能力
    let r = call(&tools, "browser_cdp", serde_json::json!({ "method": "Browser.getVersion", "target": "browser" })).await;
    assert!(r.contains("protocolVersion"), "{r}");
    let r = call(&tools, "browser_cdp", serde_json::json!({ "method": "Page.printToPDF", "params": {} })).await;
    assert!(r.contains("\"data\""), "pdf via raw cdp: {}", &r[..r.len().min(80)]);

    // 页面自己关掉当前标签：下一次操作自动换到别的标签，不会一直报 session 失效
    call(&tools, "browser_tab_new", serde_json::json!({ "url": format!("{site}/second") })).await;
    let _ = call(&tools, "browser_eval", serde_json::json!({ "script": "setTimeout(() => window.close(), 50); 1" })).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let r = call(&tools, "browser_snapshot", serde_json::json!({})).await;
    assert!(!r.contains("failed"), "recovered from closed tab: {r}");

    // 错误是可读的，不是 panic
    let r = call(&tools, "browser_click", serde_json::json!({ "ref": 9999 })).await;
    assert!(r.contains("element not found"), "{r}");

    // Chromium 崩溃（被杀）：下一次工具调用自动重启，cookie / 登录态在 profile 里还在
    call(&tools, "browser_cookies", serde_json::json!({ "action": "set", "cookie": { "name": "keep", "value": "1", "url": site, "expires": 4_000_000_000u64 } })).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let pid = browser.cdp().call(None, "SystemInfo.getProcessInfo", serde_json::json!({})).await.unwrap()["processInfo"]
        .as_array().unwrap().iter().find(|p| p["type"] == "browser").map(|p| p["id"].as_i64().unwrap()).unwrap();
    let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!browser.is_alive());
    let r = call(&tools, "browser_navigate", serde_json::json!({ "url": site })).await;
    assert!(r.contains("测试页"), "restarted: {r}");
    assert!(browser.is_alive());

    drop(tools);
    drop(browser);
    let _ = std::fs::remove_dir_all(&root);
}
