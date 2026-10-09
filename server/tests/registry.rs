//! 任务注册表与配置读写的端到端测试。

mod common;

use common::{text_reply, tool_reply, MockLlm};
use serde_json::{json, Value};
use std::time::Duration;

const ADMIN_PASSWORD: &str = "admin-password-1";

fn config_toml(llm_url: &str, web_dir: &str) -> String {
    format!(
        r#"
[http]
bind = "127.0.0.1:0"
web_dir = "{web_dir}"

[auth]
jwt_secret = "0123456789abcdef0123456789abcdef"

[auth.bootstrap_admin]
username = "admin"
password = "{ADMIN_PASSWORD}"

[storage]
endpoint = "mem://"
namespace = "t"
database = "server"

[models.ctrl]
provider = "openai"
model = "ctrl"
base_url = "{llm_url}"
api_key = "sk-ctrl-secret"

[models.worker]
provider = "openai"
model = "worker"
base_url = "{llm_url}"
api_key = "sk-worker-secret"

[agent.controller]
model = "ctrl"

[[agent.workers]]
name = "coder"
description = "writes code"
model = "worker"
system = "You code."
can_spawn = false

[agent.limits]
max_concurrent_tasks = 3
"#
    )
}

struct App {
    base: String,
    http: reqwest::Client,
    dir: std::path::PathBuf,
}

impl App {
    async fn start(llm_url: &str, tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("thought-registry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // web_dir 用相对路径（相对配置文件目录），覆盖路径还原 / 解析的往返
        let web = concat!(env!("CARGO_MANIFEST_DIR"), "/../web");
        std::os::unix::fs::symlink(web, dir.join("web")).unwrap();
        std::fs::write(dir.join("thought.toml"), config_toml(llm_url, "web")).unwrap();
        let built = server::build_from_file(&dir.join("thought.toml"), plugin::Registry::new()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, built.router).await.unwrap() });
        Self { base, http: reqwest::Client::new(), dir }
    }

    async fn call(&self, method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method.parse().unwrap(), format!("{}{path}", self.base)).bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    async fn login(&self) -> String {
        let resp: Value = self
            .http
            .post(format!("{}/api/auth/login", self.base))
            .json(&json!({"username": "admin", "password": ADMIN_PASSWORD}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        resp["token"].as_str().unwrap().to_owned()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 读 SSE 直到出现名为 `until` 的事件，返回途中所有 (事件名, data)。
async fn read_events(resp: &mut reqwest::Response, until: &str) -> Vec<(String, Value)> {
    let mut buf = String::new();
    let mut out = Vec::new();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("sse timeout")
            .unwrap()
            .expect("sse closed");
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            let name = frame.lines().find_map(|l| l.strip_prefix("event: ")).unwrap_or("").to_owned();
            let data = frame.lines().find_map(|l| l.strip_prefix("data: ")).unwrap_or("null");
            if name.is_empty() {
                continue;
            }
            let done = name == until;
            out.push((name, serde_json::from_str(data).unwrap_or(Value::Null)));
            if done {
                return out;
            }
        }
    }
}

#[tokio::test]
async fn task_registry_persists_history_and_streams_globally() {
    let llm = MockLlm::start(vec![
        (
            "ctrl",
            vec![
                tool_reply(&[("d1", "dispatch_task", r#"{"worker":"coder","task":"fix the bug"}"#)]),
                text_reply("on it"),
                text_reply("fixed"),
            ],
        ),
        ("worker", vec![text_reply("bug fixed in fusion.rs")]),
    ])
    .await;
    let app = App::start(&llm.url, "tasks").await;
    let token = app.login().await;

    let mut global = app
        .http
        .get(format!("{}/api/tasks/events", app.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(global.status(), 200);

    let (_, session) = app.call("POST", "/api/sessions", &token, Some(json!({"title": "t"}))).await;
    let sid = session["id"].as_str().unwrap().to_owned();
    let (status, _) = app
        .call("POST", &format!("/api/sessions/{sid}/messages"), &token, Some(json!({"content": "fix it"})))
        .await;
    assert_eq!(status, 202);

    let events = read_events(&mut global, "task_finished").await;
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names.first(), Some(&"task_started"), "{names:?}");
    assert!(names.contains(&"text_delta") && names.contains(&"usage"), "{names:?}");
    assert!(events.iter().all(|(_, d)| d["session_id"] == sid.as_str()), "{events:?}");
    let finished = &events.last().unwrap().1;
    assert_eq!(finished["result"], "bug fixed in fusion.rs");

    // 写库是异步的，稍等
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (status, list) = app.call("GET", "/api/tasks", &token, None).await;
    assert_eq!(status, 200, "{list}");
    let task = &list["tasks"][0];
    assert_eq!(task["session_id"], sid.as_str());
    assert_eq!(task["task_id"], 1);
    assert_eq!(task["status"], "done");
    assert_eq!(task["usage"]["total_tokens"], 2);
    assert!(task["finished_at_ms"].as_i64().unwrap() >= task["started_at_ms"].as_i64().unwrap());
    assert_eq!(list["next_cursor"], Value::Null);

    let (_, running) = app.call("GET", "/api/tasks?status=running", &token, None).await;
    assert_eq!(running["tasks"], json!([]));
    assert_eq!(app.call("GET", "/api/tasks?status=bogus", &token, None).await.0, 400);
    assert_eq!(app.call("GET", "/api/tasks?cursor=zz", &token, None).await.0, 400);

    let (status, detail) = app.call("GET", &format!("/api/tasks/{sid}/1"), &token, None).await;
    assert_eq!(status, 200, "{detail}");
    let kinds: Vec<&str> = detail["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["text", "usage"], "{detail}");
    assert_eq!(detail["events"][0]["text"], "bug fixed in fusion.rs");
    assert_eq!(detail["events"][0]["agent"], "coder#1");

    assert_eq!(app.call("GET", &format!("/api/tasks/{sid}/99"), &token, None).await.0, 404);
    assert_eq!(app.call("POST", &format!("/api/tasks/{sid}/1/cancel"), &token, None).await.0, 409);
    assert_eq!(app.call("POST", &format!("/api/tasks/{sid}/99/cancel"), &token, None).await.0, 404);
}

#[tokio::test]
async fn config_read_write_with_etag_and_secrets_preserved() {
    let llm = MockLlm::start(vec![]).await;
    let app = App::start(&llm.url, "config").await;
    let token = app.login().await;

    let resp = app.http.get(format!("{}/api/config", app.base)).bearer_auth(&token).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let etag = resp.headers()["etag"].to_str().unwrap().to_owned();
    let mut config: Value = resp.json().await.unwrap();
    let text = config.to_string();
    for secret in ["sk-ctrl-secret", "sk-worker-secret", "0123456789abcdef0123456789abcdef", ADMIN_PASSWORD] {
        assert!(!text.contains(secret), "secret leaked: {text}");
    }
    assert_eq!(config["agent"]["limits"]["max_concurrent_tasks"], 3);
    assert_eq!(config["http"]["web_dir"], "web", "shown as written, not resolved");

    // 原样写回：什么都没变
    let resp = app
        .http
        .put(format!("{}/api/config", app.base))
        .bearer_auth(&token)
        .header("if-match", &etag)
        .json(&config)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let noop: Value = resp.json().await.unwrap();
    assert_eq!(noop["restart_required"], json!([]), "{noop}");
    assert_eq!(noop["hot_applied"], json!([]), "{noop}");

    let (_, toml_text) = app.call("GET", "/api/config?format=toml", &token, None).await;
    let toml_text = toml_text.as_str().unwrap();
    assert!(toml_text.contains("[agent.controller]") && !toml_text.contains("sk-ctrl-secret"));

    // /api/agents 带出 system 与 limits
    let (_, agents) = app.call("GET", "/api/agents", &token, None).await;
    assert_eq!(agents["workers"][0]["system"], "You code.");
    assert_eq!(agents["limits"]["max_concurrent_tasks"], 3);

    // 改 limits（热生效）与 http.bind（需重启）
    config["agent"]["limits"]["max_concurrent_tasks"] = json!(5);
    config["http"]["bind"] = json!("127.0.0.1:9999");
    let put = |etag: &str, body: &Value| {
        app.http
            .put(format!("{}/api/config", app.base))
            .bearer_auth(&token)
            .header("if-match", etag)
            .json(body)
            .send()
    };
    let resp = put(&etag, &config).await.unwrap();
    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["applied"], true);
    assert_eq!(result["restart_required"], json!(["http"]));
    assert_eq!(result["hot_applied"], json!(["agent.limits"]));

    let (_, agents) = app.call("GET", "/api/agents", &token, None).await;
    assert_eq!(agents["limits"]["max_concurrent_tasks"], 5, "hot applied");

    // 写回的文件保留了明文密钥、相对路径不被改写、有 .bak
    let on_disk = std::fs::read_to_string(app.dir.join("thought.toml")).unwrap();
    assert!(on_disk.contains("sk-ctrl-secret") && on_disk.contains(ADMIN_PASSWORD), "{on_disk}");
    assert!(on_disk.contains("max_concurrent_tasks = 5"), "{on_disk}");
    assert!(on_disk.contains("web_dir = \"web\""), "relative path kept: {on_disk}");
    assert!(app.dir.join("thought.toml.bak").exists());
    let reparsed: server::ServerConfig = toml::from_str(&on_disk).unwrap();
    assert_eq!(reparsed.http.bind, "127.0.0.1:9999");

    // 旧 ETag → 412；缺 If-Match → 428；校验失败 → 422
    assert_eq!(put(&etag, &config).await.unwrap().status(), 412);
    let fresh = app.http.get(format!("{}/api/config", app.base)).bearer_auth(&token).send().await.unwrap();
    let fresh_tag = fresh.headers()["etag"].to_str().unwrap().to_owned();
    let mut bad = config.clone();
    bad["agent"]["controller"]["model"] = json!("nope");
    let resp = put(&fresh_tag, &bad).await.unwrap();
    assert_eq!(resp.status(), 422);
    assert!(resp.text().await.unwrap().contains("nope"));
    let mut bad_bind = config.clone();
    bad_bind["http"]["bind"] = json!("not an addr");
    assert_eq!(put(&fresh_tag, &bad_bind).await.unwrap().status(), 422);
    let no_match = app.http.put(format!("{}/api/config", app.base)).bearer_auth(&token).json(&config).send().await.unwrap();
    assert_eq!(no_match.status(), 428);
}
