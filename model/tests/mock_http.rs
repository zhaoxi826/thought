//! 离线 HTTP 集成测试：用本地迷你 server 验证 chat / chat_stream 全链路
//! （请求构造 → 发送 → 响应/事件流解析 → 中立输出），不需要真实 API key。

mod common;

use common::{serve_once, MockResponse};
use futures::StreamExt;
use model::{
    AnthropicArgs, AnthropicModel, ChatChunk, Message, ModelTrait, OpenAIMessage, OpenAiArgs,
    OpenAiModel,
};

fn messages() -> Vec<OpenAIMessage> {
    vec![OpenAIMessage::new("user", "hi")]
}

// ---- OpenAI 协议 ----

const OPENAI_CHAT_BODY: &str = r#"{
    "id":"chatcmpl-1","object":"chat.completion","created":1,"model":"gpt-5",
    "system_fingerprint":null,
    "choices":[{"index":0,"finish_reason":"tool_calls","logprobs":null,
        "message":{"role":"assistant","content":null,"reasoning_content":null,
            "tool_calls":[{"id":"call_1","type":"function",
                "function":{"name":"read_file","arguments":"{\"file_path\":\"/tmp/a\"}"}}]}}],
    "usage":{"completion_tokens":7,"prompt_tokens":11,"total_tokens":18,
        "prompt_cache_hit_tokens":null,"prompt_cache_miss_tokens":null,
        "completion_tokens_details":null}
}"#;

#[tokio::test]
async fn openai_chat_returns_structured_output() {
    let (url, request_rx) = serve_once(MockResponse::json(OPENAI_CHAT_BODY)).await;
    let model = OpenAiModel::new(&url, "sk-test", OpenAiArgs::openai("gpt-5"));

    let output = model.chat(messages(), None, None).await.unwrap();

    assert_eq!(output.content, None);
    assert_eq!(output.finish_reason, "tool_calls");
    assert_eq!(output.usage.prompt_tokens, 11);
    assert_eq!(output.usage.completion_tokens, 7);
    assert_eq!(output.usage.total_tokens, 18);
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].id, "call_1");
    assert_eq!(output.tool_calls[0].name, "read_file");
    assert_eq!(output.tool_calls[0].arguments, "{\"file_path\":\"/tmp/a\"}");

    let request = request_rx.await.unwrap();
    assert!(request.starts_with("POST /chat/completions "), "request line: {request}");
    assert!(request.contains("authorization: Bearer sk-test"), "auth header: {request}");
    assert!(request.contains("\"stream\":false"), "body: {request}");
}

const OPENAI_STREAM_BODY: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"he\",\"reasoning_content\":null,\"tool_calls\":null},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":\"llo\",\"reasoning_content\":null,\"tool_calls\":null},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":null,\"reasoning_content\":null,\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":null,\"reasoning_content\":null,\"tool_calls\":[{\"index\":0,\"id\":null,\"function\":{\"name\":null,\"arguments\":\"{\\\"file\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
    "data: {\"choices\":[{\"delta\":{\"content\":null,\"reasoning_content\":null,\"tool_calls\":null},\"finish_reason\":\"tool_calls\"}]}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn openai_chat_stream_yields_chunks_in_order() {
    let (url, request_rx) = serve_once(MockResponse::sse(OPENAI_STREAM_BODY)).await;
    let model = OpenAiModel::new(&url, "sk-test", OpenAiArgs::openai("gpt-5"));

    let mut stream = model.chat_stream(messages(), None, None).await.unwrap();
    let mut chunks: Vec<ChatChunk> = Vec::new();
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.unwrap());
    }

    assert_eq!(chunks.len(), 5);
    assert_eq!(chunks[0].content.as_deref(), Some("he"));
    assert_eq!(chunks[1].content.as_deref(), Some("llo"));

    let delta = &chunks[2].tool_calls[0];
    assert_eq!(delta.id.as_deref(), Some("call_1"));
    assert_eq!(delta.name.as_deref(), Some("read_file"));

    let delta = &chunks[3].tool_calls[0];
    assert_eq!(delta.arguments.as_deref(), Some("{\"file\"}"));

    assert_eq!(chunks[4].finish_reason.as_deref(), Some("tool_calls"));

    let request = request_rx.await.unwrap();
    assert!(request.contains("\"stream\":true"), "body: {request}");
}

// ---- Anthropic 协议 ----

const ANTHROPIC_CHAT_BODY: &str = r#"{
    "id":"msg_1","model":"claude","role":"assistant",
    "content":[
        {"type":"text","text":"let me read that"},
        {"type":"tool_use","id":"toolu_1","name":"read_file","input":{"file_path":"/tmp/a"}}
    ],
    "stop_reason":"tool_use",
    "usage":{"input_tokens":11,"output_tokens":7}
}"#;

#[tokio::test]
async fn anthropic_chat_returns_structured_output() {
    let (url, request_rx) = serve_once(MockResponse::json(ANTHROPIC_CHAT_BODY)).await;
    let model = AnthropicModel::new(&url, "sk-ant-test", AnthropicArgs::new("claude"));

    let output = model
        .chat(
            vec![model::AnthropicMessage::new("user", "hi")],
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(output.content.as_deref(), Some("let me read that"));
    assert_eq!(output.finish_reason, "tool_use");
    assert_eq!(output.usage.prompt_tokens, 11);
    assert_eq!(output.usage.completion_tokens, 7);
    assert_eq!(output.usage.total_tokens, 18);
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].id, "toolu_1");
    assert_eq!(output.tool_calls[0].name, "read_file");
    assert_eq!(output.tool_calls[0].arguments, "{\"file_path\":\"/tmp/a\"}");

    let request = request_rx.await.unwrap();
    assert!(request.starts_with("POST /v1/messages "), "request line: {request}");
    assert!(request.contains("x-api-key: sk-ant-test"), "auth header: {request}");
}

const ANTHROPIC_STREAM_BODY: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":1}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"he\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"llo\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"file\\\"}\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

#[tokio::test]
async fn anthropic_chat_stream_yields_chunks_in_order() {
    let (url, request_rx) = serve_once(MockResponse::sse(ANTHROPIC_STREAM_BODY)).await;
    let model = AnthropicModel::new(&url, "sk-ant-test", AnthropicArgs::new("claude"));

    let mut stream = model
        .chat_stream(vec![model::AnthropicMessage::new("user", "hi")], None, None)
        .await
        .unwrap();
    let mut chunks: Vec<ChatChunk> = Vec::new();
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.unwrap());
    }

    assert_eq!(chunks.len(), 5);
    assert_eq!(chunks[0].content.as_deref(), Some("he"));
    assert_eq!(chunks[1].content.as_deref(), Some("llo"));

    let start = &chunks[2].tool_calls[0];
    assert_eq!(start.index, 1);
    assert_eq!(start.id.as_deref(), Some("toolu_1"));
    assert_eq!(start.name.as_deref(), Some("read_file"));

    let delta = &chunks[3].tool_calls[0];
    assert_eq!(delta.index, 1);
    assert_eq!(delta.arguments.as_deref(), Some("{\"file\"}"));

    assert_eq!(chunks[4].finish_reason.as_deref(), Some("tool_use"));

    let request = request_rx.await.unwrap();
    assert!(request.contains("\"stream\":true"), "body: {request}");
}
