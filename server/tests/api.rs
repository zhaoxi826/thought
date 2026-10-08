//! 后端端到端测试：内存 SurrealDB + mock 模型，走真实 HTTP 监听。

mod common;

use common::{text_reply, tool_reply, MockLlm};
use serde_json::{json, Value};
use server::{build, ServerConfig};
use std::time::Duration;

const ADMIN_PASSWORD: &str = "admin-password-1";

fn config(llm_url: &str) -> ServerConfig {
    serde_json::from_value(json!({
        "auth": {
            "jwt_secret": "0123456789abcdef0123456789abcdef",
            "bootstrap_admin": { "username": "admin", "password": ADMIN_PASSWORD }
        },
        "storage": { "endpoint": "mem://", "namespace": "t", "database": "server" },
        "models": {
            "ctrl": { "provider": "openai", "model": "ctrl", "base_url": llm_url, "api_key": "sk-test" },
            "worker": { "provider": "openai", "model": "worker", "base_url": llm_url, "api_key": "sk-test" }
        },
        "memory": { "embedding_dimension": 8, "per_user": true },
        "agent": {
            "controller": { "model": "ctrl" },
            "workers": [{ "name": "coder", "description": "writes code", "model": "worker", "system": "You code." }]
        }
    }))
    .unwrap()
}

struct App {
    base: String,
    http: reqwest::Client,
}

impl App {
    async fn start(llm_url: &str) -> Self {
        let built = build(&config(llm_url), plugin::Registry::new()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, built.router).await.unwrap() });
        Self { base, http: reqwest::Client::new() }
    }

    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method.parse().unwrap(), format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    async fn login(&self, username: &str, password: &str) -> String {
        let (status, body) = self
            .call("POST", "/api/auth/login", None, Some(json!({"username": username, "password": password})))
            .await;
        assert_eq!(status, 200, "{body}");
        body["token"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn auth_and_user_management() {
    let llm = MockLlm::start(vec![]).await;
    let app = App::start(&llm.url).await;

    assert_eq!(app.call("GET", "/health", None, None).await.0, 200);
    assert_eq!(app.call("GET", "/api/me", None, None).await.0, 401);
    assert_eq!(app.call("GET", "/api/me", Some("garbage"), None).await.0, 401);
    let (status, _) = app
        .call("POST", "/api/auth/login", None, Some(json!({"username": "admin", "password": "wrong"})))
        .await;
    assert_eq!(status, 401);

    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let (_, me) = app.call("GET", "/api/me", Some(&admin), None).await;
    assert_eq!(me["role"], "admin");

    // 模型配置不泄露密钥
    let (status, models) = app.call("GET", "/api/models", Some(&admin), None).await;
    assert_eq!(status, 200);
    assert_eq!(models["ctrl"]["model"], "ctrl");
    assert!(!models.to_string().contains("sk-test"));

    let (status, _) = app
        .call("POST", "/api/users", Some(&admin), Some(json!({"username": "bob", "password": "bob-password"})))
        .await;
    assert_eq!(status, 201);
    let (status, _) = app
        .call("POST", "/api/users", Some(&admin), Some(json!({"username": "bob", "password": "bob-password"})))
        .await;
    assert_eq!(status, 409);
    let (status, _) = app
        .call("POST", "/api/users", Some(&admin), Some(json!({"username": "eve", "password": "short"})))
        .await;
    assert_eq!(status, 400);

    let bob = app.login("bob", "bob-password").await;
    assert_eq!(app.call("GET", "/api/users", Some(&bob), None).await.0, 403);
    assert_eq!(app.call("GET", "/api/models", Some(&bob), None).await.0, 403);
    let (status, agents) = app.call("GET", "/api/agents", Some(&bob), None).await;
    assert_eq!(status, 200);
    assert_eq!(agents["workers"][0]["name"], "coder");
}

/// 读 SSE 流直到出现 `reply` 事件，返回其文本以及沿途见到的事件名。
async fn read_until_reply(resp: &mut reqwest::Response) -> (String, Vec<String>) {
    let mut buf = String::new();
    let mut names = Vec::new();
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
            let data = frame.lines().find_map(|l| l.strip_prefix("data: ")).unwrap_or("");
            if name == "reply" {
                let v: Value = serde_json::from_str(data).unwrap();
                return (v["text"].as_str().unwrap().to_owned(), names);
            }
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
}

#[tokio::test]
async fn chat_session_flow_with_memory_and_isolation() {
    let llm = MockLlm::start(vec![(
        "ctrl",
        vec![
            tool_reply(&[("m1", "memory_save", r#"{"content":"用户的项目叫 thought"}"#)]),
            text_reply("记住了"),
        ],
    )])
    .await;
    let app = App::start(&llm.url).await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    app.call("POST", "/api/users", Some(&admin), Some(json!({"username": "bob", "password": "bob-password"})))
        .await;
    let bob = app.login("bob", "bob-password").await;

    let (status, session) = app.call("POST", "/api/sessions", Some(&admin), Some(json!({"title": "t1"}))).await;
    assert_eq!(status, 201);
    let sid = session["id"].as_str().unwrap().to_owned();

    // 订阅事件后再发消息
    let mut events = app
        .http
        .get(format!("{}/api/sessions/{sid}/events", app.base))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), 200);
    let (status, _) = app
        .call("POST", &format!("/api/sessions/{sid}/messages"), Some(&admin), Some(json!({"content": "记住我的项目叫 thought"})))
        .await;
    assert_eq!(status, 202);

    let (reply, seen) = read_until_reply(&mut events).await;
    assert_eq!(reply, "记住了");
    assert!(seen.contains(&"tool_call".to_owned()) && seen.contains(&"text_delta".to_owned()), "{seen:?}");

    // 主控请求里带了会话专属的记忆工具
    assert!(llm.requests("ctrl")[0].contains("\"memory_search\""));

    // 记忆写进了 admin 自己的空间
    let (_, hits) = app.call("GET", "/api/memory/search?q=项目%20thought", Some(&admin), None).await;
    assert_eq!(hits[0]["content"], "用户的项目叫 thought", "{hits}");
    let (_, bob_hits) = app.call("GET", "/api/memory/search?q=项目%20thought", Some(&bob), None).await;
    assert_eq!(bob_hits, json!([]));

    // 历史已持久化（不含 system）
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, detail) = app.call("GET", &format!("/api/sessions/{sid}"), Some(&admin), None).await;
    let history = detail["history"].as_array().unwrap();
    assert_eq!(history.first().unwrap()["role"], "user");
    assert_eq!(history.last().unwrap()["content"], "记住了");

    // 他人会话一律 404
    assert_eq!(app.call("GET", &format!("/api/sessions/{sid}"), Some(&bob), None).await.0, 404);
    let (status, _) = app
        .call("POST", &format!("/api/sessions/{sid}/messages"), Some(&bob), Some(json!({"content": "hi"})))
        .await;
    assert_eq!(status, 404);

    let (_, list) = app.call("GET", "/api/sessions", Some(&admin), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(app.call("DELETE", &format!("/api/sessions/{sid}"), Some(&admin), None).await.0, 204);
    assert_eq!(app.call("GET", &format!("/api/sessions/{sid}"), Some(&admin), None).await.0, 404);
}

#[tokio::test]
async fn memory_endpoints() {
    let llm = MockLlm::start(vec![]).await;
    let app = App::start(&llm.url).await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;

    let (status, saved) = app
        .call("POST", "/api/memory", Some(&admin), Some(json!({"content": "部署在 Debian 13", "tags": ["infra"]})))
        .await;
    assert_eq!(status, 201);
    let id = saved["id"].as_str().unwrap().to_owned();
    let (status, _) = app
        .call("POST", "/api/memory", Some(&admin), Some(json!({"content": "部署在 Debian 13"})))
        .await;
    assert_eq!(status, 200, "duplicate is not re-created");

    let (_, recent) = app.call("GET", "/api/memory?limit=5", Some(&admin), None).await;
    assert_eq!(recent[0]["tags"], json!(["infra"]));

    assert_eq!(app.call("DELETE", &format!("/api/memory/{id}"), Some(&admin), None).await.0, 204);
    assert_eq!(app.call("DELETE", &format!("/api/memory/{id}"), Some(&admin), None).await.0, 404);
}
