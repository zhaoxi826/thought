//! 三层 agent 会话的离线测试：主控派单 → worker（可派生子 agent）→ 结果回流主控。

mod common;

use agent::{AgentConfig, AgentError, AgentEvent, AgentSystem, Input, SessionHandle};
use common::{text_reply, tool_reply, MockLlm};
use model::{Function, Model, OpenAiArgs, OpenAiModel};
use plugin::{Registry, Tool};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

struct Weather;

#[async_trait::async_trait]
impl Tool for Weather {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn definition(&self) -> Function {
        Function {
            name: "get_weather".to_owned(),
            description: "Get weather".to_owned(),
            parameters: vec![],
            strict: false,
        }
    }

    async fn call(&self, _arguments: &str) -> String {
        "sunny".to_owned()
    }
}

fn config() -> AgentConfig {
    serde_json::from_value(serde_json::json!({
        "controller": {"model": "ctrl"},
        "workers": [{
            "name": "researcher",
            "description": "looks things up",
            "model": "worker",
            "system": "You research.",
            "tools": ["get_weather"]
        }],
        "limits": {"max_concurrent_tasks": 1}
    }))
    .unwrap()
}

fn system(url: &str, config: AgentConfig) -> Result<AgentSystem, AgentError> {
    let model = |id: &str| Model::OpenAi(OpenAiModel::new(url, "sk-test", OpenAiArgs::openai(id)));
    let models = HashMap::from([("ctrl".to_owned(), model("ctrl")), ("worker".to_owned(), model("worker"))]);
    let mut tools = Registry::new();
    tools.register(Arc::new(Weather));
    AgentSystem::new(config, models, tools)
}

/// 等待下一条满足条件的事件。
async fn wait_for(
    rx: &mut broadcast::Receiver<AgentEvent>,
    pred: impl Fn(&AgentEvent) -> bool,
) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = rx.recv().await.expect("event channel closed");
            if pred(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

async fn next_reply(rx: &mut broadcast::Receiver<AgentEvent>) -> String {
    match wait_for(rx, |e| matches!(e, AgentEvent::Reply { .. } | AgentEvent::Error { .. })).await {
        AgentEvent::Reply { text } => text,
        other => panic!("controller error: {other:?}"),
    }
}

fn start(system: &AgentSystem) -> (SessionHandle, broadcast::Receiver<AgentEvent>) {
    let session = system.start_session("s1", vec![]);
    let events = session.subscribe();
    (session, events)
}

#[tokio::test]
async fn controller_dispatches_worker_spawns_subagent_and_reports_back() {
    let llm = MockLlm::start(vec![
        (
            "ctrl",
            vec![
                tool_reply(&[(
                    "d1",
                    "dispatch_task",
                    r#"{"worker":"researcher","task":"weather in Paris"}"#,
                )]),
                text_reply("On it."),
                text_reply("Paris is sunny."),
            ],
        ),
        (
            "worker",
            vec![
                // L2：先派生 L3 子 agent
                tool_reply(&[("s1", "spawn_subagent", r#"{"task":"check weather"}"#)]),
                // L3：调真实工具，再汇报
                tool_reply(&[("w1", "get_weather", "{}")]),
                text_reply("sub: sunny"),
                // L2：最终结果
                text_reply("It is sunny in Paris"),
            ],
        ),
    ])
    .await;
    let system = system(&llm.url, config()).unwrap();
    let (session, mut events) = start(&system);

    assert!(session.send_user("What's the weather in Paris?"));
    assert_eq!(next_reply(&mut events).await, "On it.");

    let finished = wait_for(&mut events, |e| matches!(e, AgentEvent::TaskFinished { .. })).await;
    assert_eq!(
        finished,
        AgentEvent::TaskFinished {
            task_id: 1,
            worker: "researcher".into(),
            result: Some("It is sunny in Paris".into()),
            error: None,
        }
    );
    assert_eq!(next_reply(&mut events).await, "Paris is sunny.");
    assert!(session.tasks().is_empty());

    // 主控只拿到元工具，看不到真实工具；第三轮收到 task_result
    let ctrl = llm.requests("ctrl");
    assert_eq!(ctrl.len(), 3);
    assert!(ctrl[0].contains("\"dispatch_task\"") && !ctrl[0].contains("\"get_weather\""));
    assert!(ctrl[0].contains("- researcher: looks things up"));
    assert!(ctrl[2].contains("<task_result task_id=\\\"1\\\" worker=\\\"researcher\\\" status=\\\"ok\\\">"), "{}", ctrl[2]);

    // L2 有 spawn_subagent；L3 有真实工具但没有 spawn_subagent
    let worker = llm.requests("worker");
    assert_eq!(worker.len(), 4);
    assert!(worker[0].contains("\"spawn_subagent\"") && worker[0].contains("\"get_weather\""));
    assert!(worker[1].contains("You are a sub-agent") && !worker[1].contains("\"spawn_subagent\""));
    assert!(worker[3].contains("sub: sunny"), "L2 sees sub-agent report: {}", worker[3]);
}

#[tokio::test]
async fn dispatch_respects_concurrency_limit_and_no_reply_is_silent() {
    let llm = MockLlm::start(vec![
        (
            "ctrl",
            vec![
                tool_reply(&[
                    ("d1", "dispatch_task", r#"{"worker":"researcher","task":"a"}"#),
                    ("d2", "dispatch_task", r#"{"worker":"researcher","task":"b"}"#),
                    ("d3", "dispatch_task", r#"{"worker":"ghost","task":"c"}"#),
                ]),
                text_reply("NO_REPLY"),
                text_reply("done"),
            ],
        ),
        ("worker", vec![text_reply("a done")]),
    ])
    .await;
    let system = system(&llm.url, config()).unwrap();
    let (session, mut events) = start(&system);

    session.send_user("do a and b");
    // NO_REPLY 不产生 Reply；下一条 Reply 来自任务结果那一轮
    assert_eq!(next_reply(&mut events).await, "done");

    let second = &llm.requests("ctrl")[1];
    assert!(second.contains("dispatched as task 1"), "{second}");
    assert!(second.contains("too many running tasks (1)"), "{second}");
    assert!(second.contains("unknown worker \\\"ghost\\\""), "{second}");
}

#[tokio::test]
async fn queued_inputs_are_batched_and_todos_fire() {
    let llm = MockLlm::start(vec![(
        "ctrl",
        vec![
            tool_reply(&[("t1", "add_todo", r#"{"text":"check again","delay_seconds":0}"#)]),
            text_reply("noted"),
            text_reply("checked"),
        ],
    )])
    .await;
    let system = system(&llm.url, config()).unwrap();
    let session = system.start_session("s1", vec![]);
    let mut events = session.subscribe();

    // 主控尚未调度时连发两条，应合并为一批
    session.send(Input::User("first".into()));
    session.send(Input::User("second".into()));
    assert_eq!(next_reply(&mut events).await, "noted");
    assert_eq!(next_reply(&mut events).await, "checked");

    let ctrl = llm.requests("ctrl");
    assert_eq!(ctrl.len(), 3);
    assert!(ctrl[0].contains("first\\n\\nsecond"), "{}", ctrl[0]);
    assert!(ctrl[2].contains("<todo_due>\\ncheck again\\n</todo_due>"), "{}", ctrl[2]);
}

#[tokio::test]
async fn model_failure_surfaces_as_error_event_and_session_survives() {
    // 主控脚本只有一条：第一批成功，第二批 500
    let llm = MockLlm::start(vec![("ctrl", vec![text_reply("hi")])]).await;
    let system = system(&llm.url, config()).unwrap();
    let (session, mut events) = start(&system);

    session.send_user("hello");
    assert_eq!(next_reply(&mut events).await, "hi");
    session.send_user("again");
    let event = wait_for(&mut events, |e| matches!(e, AgentEvent::Error { .. })).await;
    assert!(matches!(event, AgentEvent::Error { message } if message.contains("500")));
    assert!(session.send_user("still alive"));
}

#[test]
fn rejects_config_with_unknown_references() {
    let mut bad = config();
    bad.workers[0].tools.push("rm_rf".into());
    assert!(matches!(system("http://x", bad), Err(AgentError::Config(m)) if m.contains("rm_rf")));

    let mut bad = config();
    bad.controller.model = "nope".into();
    assert!(matches!(system("http://x", bad), Err(AgentError::Config(m)) if m.contains("nope")));

    let mut bad = config();
    bad.workers.push(bad.workers[0].clone());
    assert!(matches!(system("http://x", bad), Err(AgentError::Config(m)) if m.contains("duplicate")));
}
