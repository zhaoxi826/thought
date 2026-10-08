//! Agent 循环的离线集成测试：本地 mock server 按顺序扮演模型，
//! 验证「模型调工具 → agent 执行 → 回传结果 → 模型给出最终回复」全链路。

mod common;

use common::serve_sequence;
use std::sync::Arc;

use agent::{Agent, AgentError};
use model::{Function, Model, OpenAiArgs, OpenAiModel};
use plugin::{Registry, Tool};

/// 固定应答的测试工具，避免依赖文件系统。
struct GetWeather;

#[async_trait::async_trait]
impl Tool for GetWeather {
    fn name(&self) -> &str {
        "get_weather"
    }

    fn definition(&self) -> Function {
        Function {
            name: "get_weather".to_owned(),
            description: "Get the weather of a city".to_owned(),
            parameters: vec![],
            strict: false,
        }
    }

    async fn call(&self, _arguments: &str) -> String {
        "sunny, 25C".to_owned()
    }
}

fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(Arc::new(GetWeather));
    registry
}

fn openai_model(url: &str) -> Model {
    Model::OpenAi(OpenAiModel::new(url, "sk-test", OpenAiArgs::openai("gpt-5")))
}

fn chat_response(content: Option<&str>, tool_calls: &str, finish_reason: &str) -> String {
    format!(
        r#"{{"id":"x","object":"chat.completion","created":1,"model":"gpt-5",
            "system_fingerprint":null,
            "choices":[{{"index":0,"finish_reason":"{finish_reason}","logprobs":null,
                "message":{{"role":"assistant","content":{},"reasoning_content":null,
                    "tool_calls":{tool_calls}}}}}],
            "usage":{{"completion_tokens":1,"prompt_tokens":1,"total_tokens":2,
                "prompt_cache_hit_tokens":null,"prompt_cache_miss_tokens":null,
                "completion_tokens_details":null}}}}"#,
        content.map_or("null".to_owned(), |c| format!("\"{c}\""))
    )
}

const TOOL_CALL_RESPONSE: &str = r#"{"id":"x","object":"chat.completion","created":1,"model":"gpt-5",
    "system_fingerprint":null,
    "choices":[{"index":0,"finish_reason":"tool_calls","logprobs":null,
        "message":{"role":"assistant","content":null,"reasoning_content":null,
            "tool_calls":[{"id":"call_1","type":"function",
                "function":{"name":"get_weather","arguments":"{}"}}]}}],
    "usage":{"completion_tokens":1,"prompt_tokens":1,"total_tokens":2,
        "prompt_cache_hit_tokens":null,"prompt_cache_miss_tokens":null,
        "completion_tokens_details":null}}"#;

#[tokio::test]
async fn agent_runs_tool_loop_to_final_answer() {
    let final_response = chat_response(Some("It is sunny, 25C."), "null", "stop");
    let (url, requests_rx) =
        serve_sequence(vec![TOOL_CALL_RESPONSE.to_owned(), final_response]).await;

    let mut agent = Agent::new(openai_model(&url), registry());
    let answer = agent.run("weather in Shanghai?").await.unwrap();

    assert_eq!(answer, "It is sunny, 25C.");

    let requests = requests_rx.await.unwrap();
    assert_eq!(requests.len(), 2, "expect tool call round + final round");

    // 第一轮请求应带上工具定义
    assert!(requests[0].contains("\"get_weather\""), "request 0: {}", requests[0]);
    // 第二轮请求应包含工具结果（以包裹文本回传）
    assert!(
        requests[1].contains("[tool result: get_weather]"),
        "request 1: {}",
        requests[1]
    );
    assert!(requests[1].contains("sunny, 25C"), "request 1: {}", requests[1]);
}

#[tokio::test]
async fn agent_returns_direct_answer_without_tools() {
    let (url, requests_rx) =
        serve_sequence(vec![chat_response(Some("just hi"), "null", "stop")]).await;

    let mut agent = Agent::new(openai_model(&url), registry());
    let answer = agent.run("hi").await.unwrap();

    assert_eq!(answer, "just hi");
    assert_eq!(requests_rx.await.unwrap().len(), 1);
}

#[tokio::test]
async fn agent_gives_up_after_max_turns() {
    // 模型每次都要求调工具，永不给出最终回复
    let (url, _requests_rx) = serve_sequence(vec![TOOL_CALL_RESPONSE.to_owned(), TOOL_CALL_RESPONSE.to_owned()]).await;

    let mut agent = Agent::new(openai_model(&url), registry()).with_max_turns(2);
    let result = agent.run("loop forever").await;

    assert!(matches!(result, Err(AgentError::MaxTurnsExceeded)));
}

#[tokio::test]
async fn agent_reports_unknown_tool_back_to_model() {
    let unknown_call = chat_response(
        None,
        r#"[{"id":"call_1","type":"function","function":{"name":"no_such_tool","arguments":"{}"}}]"#,
        "tool_calls",
    );
    let final_response = chat_response(Some("gave up"), "null", "stop");
    let (url, requests_rx) = serve_sequence(vec![unknown_call, final_response])
    .await;

    let mut agent = Agent::new(openai_model(&url), registry());
    let answer = agent.run("hi").await.unwrap();

    assert_eq!(answer, "gave up");
    let requests = requests_rx.await.unwrap();
    assert!(
        requests[1].contains("unknown tool: no_such_tool"),
        "request 1: {}",
        requests[1]
    );
}
