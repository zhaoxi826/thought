//! Runner（ReAct 执行器）的离线测试：mock 模型扮演流式 OpenAI 接口。

mod common;

use agent::{AgentError, AgentEvent, Runner};
use common::{text_reply, tool_reply, MockLlm};
use model::{Function, Message, Model, OpenAiArgs, OpenAiModel};
use plugin::{Registry, Tool};
use std::sync::Arc;
use tokio::sync::broadcast;

/// 固定应答的测试工具，回显参数。
struct Echo;

#[async_trait::async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    fn definition(&self) -> Function {
        Function {
            name: "echo".to_owned(),
            description: "Echo arguments".to_owned(),
            parameters: vec![],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        format!("echo:{arguments}")
    }
}

fn runner(url: &str, max_turns: usize) -> (Runner, broadcast::Receiver<AgentEvent>) {
    let mut registry = Registry::new();
    registry.register(Arc::new(Echo));
    let model = Model::OpenAi(OpenAiModel::new(url, "sk-test", OpenAiArgs::openai("m")));
    let (tx, rx) = broadcast::channel(64);
    (Runner::new("tester", model, registry, max_turns, 12, tx), rx)
}

#[tokio::test]
async fn runs_parallel_tool_calls_then_answers() {
    let llm = MockLlm::start(vec![(
        "m",
        vec![
            tool_reply(&[("c1", "echo", "{\"a\":1}"), ("c2", "nope", "{}")]),
            text_reply("all done"),
        ],
    )])
    .await;
    let (runner, mut events) = runner(&llm.url, 5);

    let mut history = vec![Message::user("go")];
    let answer = runner.run(&mut history).await.unwrap();
    assert_eq!(answer, "all done");

    // user, assistant(tool_calls), tool c1, tool c2, assistant(final)
    assert_eq!(history.len(), 5);
    assert_eq!(history[2], Message::tool("c1", "echo:{\"a\":1}"));
    assert_eq!(history[3], Message::tool("c2", "unknown tool: nope"));

    let requests = llm.requests("m");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("\"name\":\"echo\""), "tools sent: {}", requests[0]);
    assert!(requests[1].contains(r#""tool_call_id":"c1""#), "{}", requests[1]);

    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        seen.push(e);
    }
    assert!(seen.contains(&AgentEvent::TextDelta { agent: "tester".into(), text: "all done".into() }));
    assert!(seen.iter().any(|e| matches!(e, AgentEvent::ToolResult { name, .. } if name == "echo")));
}

#[tokio::test]
async fn truncates_long_tool_output() {
    let long_args = "{\"text\":\"0123456789abcdef\"}";
    let llm = MockLlm::start(vec![(
        "m",
        vec![tool_reply(&[("c1", "echo", long_args)]), text_reply("ok")],
    )])
    .await;
    let (runner, _events) = runner(&llm.url, 5);
    let mut history = vec![Message::user("go")];
    runner.run(&mut history).await.unwrap();
    assert!(history[2].text().contains("[truncated"), "{:?}", history[2]);
}

#[tokio::test]
async fn gives_up_after_max_turns_with_valid_history() {
    let llm = MockLlm::start(vec![(
        "m",
        vec![tool_reply(&[("c1", "echo", "{}")]), tool_reply(&[("c2", "echo", "{}")])],
    )])
    .await;
    let (runner, _events) = runner(&llm.url, 2);
    let mut history = vec![Message::user("loop")];
    assert!(matches!(runner.run(&mut history).await, Err(AgentError::MaxTurnsExceeded)));
    // 每个工具调用都有对应结果
    assert!(matches!(history.last(), Some(Message::Tool { call_id, .. }) if call_id == "c2"));
}
