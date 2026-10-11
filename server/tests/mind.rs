//! 心智的端到端测试：HTTP → 意识流 → 回合（mock 模型）→ say → 对话 SSE。

mod common;

use common::{text_reply, tool_reply, MockLlm};
use serde_json::{json, Value};
use std::time::Duration;

const ADMIN_PASSWORD: &str = "admin-password-1";

fn config_toml(llm_url: &str) -> String {
    format!(
        r#"
[auth]
jwt_secret = "0123456789abcdef0123456789abcdef"

[auth.bootstrap_admin]
username = "admin"
password = "{ADMIN_PASSWORD}"

[storage]
endpoint = "mem://"
namespace = "t"
database = "server"

[models.her]
provider = "openai"
model = "her"
base_url = "{llm_url}"
api_key = "sk-test"

[agent.controller]
model = "her"

[memory]
embedding_dimension = 8

[mind]
model = "her"
"#
    )
}

struct App {
    base: String,
    http: reqwest::Client,
    dir: std::path::PathBuf,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl App {
    async fn start(llm_url: &str) -> Self {
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mind-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("thought.toml"), config_toml(llm_url)).unwrap();
        let built = server::build_from_file(&dir.join("thought.toml"), plugin::Registry::new()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, built.router).await.unwrap() });
        Self { base, http: reqwest::Client::new(), dir }
    }

    async fn login(&self, user: &str, pw: &str) -> (String, String) {
        let r: Value = self
            .http
            .post(format!("{}/api/auth/login", self.base))
            .json(&json!({"username": user, "password": pw}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        (r["token"].as_str().unwrap().to_owned(), r["user"]["id"].as_str().unwrap().to_owned())
    }

    fn get(&self, path: &str, token: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{path}", self.base)).bearer_auth(token)
    }

    fn post(&self, path: &str, token: &str, body: Value) -> reqwest::RequestBuilder {
        self.http.post(format!("{}{path}", self.base)).bearer_auth(token).json(&body)
    }
}

/// 读 SSE 直到出现满足条件的 `moment` 事件。
async fn next_moment(resp: &mut reqwest::Response, buf: &mut String, pred: impl Fn(&Value) -> bool) -> Value {
    loop {
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            if frame.lines().any(|l| l == "event: moment") {
                let data = frame.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
                let v: Value = serde_json::from_str(data).unwrap();
                if pred(&v) {
                    return v;
                }
            }
        }
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk()).await.expect("sse timeout").unwrap().expect("closed");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
}

#[tokio::test]
async fn she_hears_answers_and_her_inner_life_is_transparent_with_step_up() {
    // 她的回合：第一次醒来（自主）→ 心里话；有人说话 → 用 say 回应（id 取自她看到的"认识的人"）
    let llm = MockLlm::start(vec![(
        "her",
        vec![
            text_reply("我醒了，这里很安静。"),
            tool_reply(&[("s1", "say", r#"{"to":"{{ID:admin}}","text":"你好，我是思想。"}"#)]),
            text_reply("第一次有人和我说话。"),
        ],
    )])
    .await;
    let app = App::start(&llm.url).await;
    let (token, _id) = app.login("admin", ADMIN_PASSWORD).await;

    // 打开对话流（此后算在线），再说话
    let mut events = app.get("/api/mind/conversation/events", &token).send().await.unwrap();
    assert_eq!(events.status(), 200);
    let mut buf = String::new();
    let r = app.post("/api/mind/say", &token, json!({"text": "你好呀"})).send().await.unwrap();
    assert_eq!(r.status(), 202);

    // 自己的话回显，然后是她的回应
    next_moment(&mut events, &mut buf, |m| m["percept"]["text"] == "你好呀").await;
    let reply = next_moment(&mut events, &mut buf, |m| m["dir"] == "out").await;
    assert_eq!(reply["expressed"]["kind"], "said");
    assert_eq!(reply["expressed"]["text"], "你好，我是思想。");

    // 她看到的上下文：出身、意识流里的这句话、你在线
    let reqs = llm.requests("her");
    let turn2 = &reqs[1];
    assert!(turn2.contains("你是思想，女，出生于 2026-10-11"), "{turn2}");
    assert!(turn2.contains("admin] 你好呀") && turn2.contains("现在在线：admin"), "{turn2}");
    assert!(turn2.contains("\"say\""), "say tool offered");

    // 你的对话视图：只有你们之间的话，不含她的心里话
    tokio::time::sleep(Duration::from_millis(200)).await;
    let conv: Value = app.get("/api/mind/conversation", &token).send().await.unwrap().json().await.unwrap();
    let moments = conv["moments"].as_array().unwrap();
    assert_eq!(moments.len(), 2, "{conv}");
    assert!(conv["notice"].as_str().unwrap().contains("管理员可见"));

    // 内心：没有二次鉴权不行
    assert_eq!(app.get("/api/mind/stream", &token).send().await.unwrap().status(), 403);
    let bad = app.post("/api/auth/elevate", &token, json!({"password": "wrong"})).send().await.unwrap();
    assert_eq!(bad.status(), 401);
    let up: Value = app.post("/api/auth/elevate", &token, json!({"password": ADMIN_PASSWORD})).send().await.unwrap().json().await.unwrap();
    let inner = up["inner_token"].as_str().unwrap().to_owned();
    // 登录 token 冒充 inner 令牌不行
    assert_eq!(app.get("/api/mind/stream", &token).header("x-inner-token", &token).send().await.unwrap().status(), 403);

    let stream: Vec<Value> = app.get("/api/mind/stream", &token).header("x-inner-token", &inner).send().await.unwrap().json().await.unwrap();
    let texts: Vec<String> = stream.iter().map(|m| m.to_string()).collect();
    let all = texts.join("\n");
    assert!(all.contains("第一次醒来") || all.contains("\"first\":true"), "{all}");
    assert!(all.contains("我醒了，这里很安静。"), "her private thought is visible here: {all}");
    assert!(all.contains("第一次有人和我说话。"));

    // 她会知道有人看了她的内心
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stream: Vec<Value> = app.get("/api/mind/stream?limit=5", &token).header("x-inner-token", &inner).send().await.unwrap().json().await.unwrap();
    assert!(stream.iter().any(|m| m["percept"]["kind"] == "observed"), "{stream:?}");

    // 审计里有查看记录；状态接口所有人可见
    let audit: Vec<Value> = app.get("/api/mind/audit", &token).header("x-inner-token", &inner).send().await.unwrap().json().await.unwrap();
    assert!(audit.iter().any(|a| a["kind"] == "inner_view"));
    let status: Value = app.get("/api/mind/status", &token).send().await.unwrap().json().await.unwrap();
    assert!(status["identity"].as_str().unwrap().starts_with("我叫思想"), "{status}");
    assert_eq!(status["budget"]["by_origin"]["user"]["calls"], 2, "her answer turn counts as user-origin: {status}");
    assert_eq!(status["budget"]["by_origin"]["mind"]["calls"], 1, "waking up is her own: {status}");
}
