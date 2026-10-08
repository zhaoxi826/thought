use model::{Message, ModelTrait, OpenAiArgs, OpenAiModel};
use std::env;

const BASE_URL: &str = "https://api.deepseek.com";

fn api_key() -> String {
    let key = env::var("API_KEY").expect("API_KEY environment variable must be set");
    assert!(
        !key.is_empty(),
        "API_KEY environment variable must not be empty"
    );
    assert!(
        key.starts_with("sk-"),
        "API_KEY should start with 'sk-', got prefix: {:?}",
        &key[..key.len().min(10)]
    );
    key
}

#[tokio::test]
#[ignore = "needs network and API_KEY"]
async fn test_deepseek_get_models() {
    let mut deepseek_model =
        OpenAiModel::deepseek(BASE_URL, &api_key(), OpenAiArgs::deepseek("deepseek-v4-pro"));
    deepseek_model.get_models().await.unwrap();

    assert!(
        deepseek_model
            .model_list
            .contains(&"deepseek-v4-pro".to_string()),
        "model list should contain deepseek-v4-pro"
    );
    assert!(
        deepseek_model
            .model_list
            .contains(&"deepseek-v4-flash".to_string()),
        "model list should contain deepseek-v4-flash"
    );
}

#[tokio::test]
#[ignore = "needs network and API_KEY"]
async fn test_deepseek_chat() {
    let deepseek_model =
        OpenAiModel::deepseek(BASE_URL, &api_key(), OpenAiArgs::deepseek("deepseek-v4-pro"));

    let messages: Vec<Message> = vec![
        Message::system("You are a helpful assistant."),
        Message::user("Hi"),
    ];

    let output = deepseek_model
        .chat(&messages, None, None)
        .await
        .expect("chat should succeed");
    let content = output.content.expect("chat reply should have content");
    assert!(!content.is_empty(), "chat reply should not be empty");
    assert!(output.usage.total_tokens > 0, "usage should be reported");
    println!("{content}");
}

#[tokio::test]
#[ignore = "needs network and API_KEY"]
async fn test_deepseek_chat_stream() {
    use futures::StreamExt;

    let deepseek_model =
        OpenAiModel::deepseek(BASE_URL, &api_key(), OpenAiArgs::deepseek("deepseek-v4-pro"));

    let messages: Vec<Message> = vec![Message::user("Hi")];
    let mut stream = deepseek_model
        .chat_stream(&messages, None, None)
        .await
        .expect("chat_stream should connect");

    let mut text = String::new();
    let mut finish_reason = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk should parse");
        if let Some(content) = chunk.content {
            text.push_str(&content);
        }
        if chunk.finish_reason.is_some() {
            finish_reason = chunk.finish_reason;
        }
    }

    assert!(!text.is_empty(), "streamed reply should not be empty");
    assert!(finish_reason.is_some(), "stream should carry a finish reason");
    println!("{text}");
}
