use model::{AnthropicArgs, AnthropicMessage, AnthropicModel, Message, ModelTrait};
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
async fn test_anthropic_chat() {
    let anthropic_model = AnthropicModel::new(
        BASE_URL,
        &api_key(),
        AnthropicArgs::new("deepseek-v4-pro"),
    );

    let messages: Vec<AnthropicMessage> = vec![
        AnthropicMessage::new("system", "You are a helpful assistant."),
        AnthropicMessage::new("user", "Hi"),
    ];

    let reply = anthropic_model
        .chat(messages, None, None)
        .await
        .expect("chat should succeed");
    assert!(!reply.is_empty(), "chat reply should not be empty");
    println!("{}", reply);
}
