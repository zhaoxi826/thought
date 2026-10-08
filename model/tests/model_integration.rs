use model::{Message, Model, OpenAiArgs, OpenAiModel};
use std::env;
#[tokio::test]
#[ignore = "needs network and API_KEY"]
async fn model_test() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("API_KEY").unwrap();
    let ds =OpenAiModel::new("https://api.deepseek.com", &api_key, OpenAiArgs::deepseek("deepseek-v4-pro"));
    let deepseek = Model::OpenAi(ds);
    let messages = vec![
        Message::system("You are a helpful assistant."),
        Message::user("Hi"),
    ];
    let reply = deepseek.chat(&messages, None, None).await?;
    println!("{:?}", reply);
    Ok(())
}