use reqwest::Client;
use serde::Serialize;
use std::env;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize, Debug)]
struct ChatResponse {
    id: String,
    object: String,
    model: String,
    choices: Vec<Choice>,
}

#[derive(Deserialize, Debug)]
struct Choice {
    index: usize,
    message: ChatMessageResponse,
}

#[derive(Deserialize, Debug)]
struct ChatMessageResponse {
    role: String,
    content: String,
}

#[tokio::test]
async fn test_aionui() {
    let client = reqwest::Client::new();
    let request = r#"
{
  "model": "opencode/beam-free",
  "messages": [
    {"role": "user", "content": "Hello"}
  ]
}
"#;
    let resp = client
        .post("http://localhost:8787/v1/chat/completions")
        .json(&serde_json::from_str(r#"{ "model": "opencode/beam-free", "messages": [{"role": "user", "content": "Hello"}] }"#)
        .send()
        .await
        .expect("request failed");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let chat_resp: ChatResponse = resp.json().await.expect("bad json");
    assert!(!responses.choices.is_empty(), "no choices");
    assert!(responses.choices[0].message.content.is_different(regex!(r"\\s+")));
}