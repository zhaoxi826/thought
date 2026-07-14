use model::model::base::{Message, Model, OpenAIMessage};
use model::model::deepseek::{DeepSeekModel, DeepseekArgs};
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
async fn test_deepseek_get_models() {
    let mut deepseek_model =
        DeepSeekModel::new(BASE_URL, &api_key(), DeepseekArgs::new("deepseek-v4-pro"));
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
async fn test_deepseek_chat() {
    let deepseek_model =
        DeepSeekModel::new(BASE_URL, &api_key(), DeepseekArgs::new("deepseek-v4-pro"));

    let messages: Vec<OpenAIMessage> = vec![
        OpenAIMessage::new("system", "You are a helpful assistant."),
        OpenAIMessage::new("user", "Hi"),
    ];

    let reply = deepseek_model
        .chat(messages, None, None)
        .await
        .expect("chat should succeed");
    assert!(!reply.is_empty(), "chat reply should not be empty");
    println!("{}", reply);
}
