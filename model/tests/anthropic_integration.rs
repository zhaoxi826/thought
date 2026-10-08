use model::{AnthropicArgs, AnthropicModel, Message, ModelTrait};
use std::env;

const BASE_URL: &str = "https://api.deepseek.com/anthropic";

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
async fn test_anthropic_chat() {
    let anthropic_model = AnthropicModel::new(
        BASE_URL,
        &api_key(),
        AnthropicArgs::new("deepseek-v4-pro"),
    );

    let messages: Vec<Message> = vec![
        Message::system("You are a helpful assistant."),
        Message::user("Hi"),
    ];

    let output = anthropic_model
        .chat(&messages, None, None)
        .await
        .expect("chat should succeed");
    let content = output.content.expect("chat reply should have content");
    assert!(!content.is_empty(), "chat reply should not be empty");
    println!("{content}");
}
