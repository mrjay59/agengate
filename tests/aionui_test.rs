use serde::Deserialize;

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ChatResponse {
    id: String,
    object: String,
    model: String,
    choices: Vec<Choice>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct Choice {
    index: usize,
    message: ChatMessageResponse,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct ChatMessageResponse {
    role: String,
    content: String,
}

#[tokio::test]
#[ignore = "requires a running agentgate server on localhost:8787; run with --ignored"]
async fn test_aionui() {
    let client = reqwest::Client::new();
    let resp = client
        .post("http://localhost:8787/v1/chat/completions")
        .json(&serde_json::from_str::<serde_json::Value>(r#"{ "model": "9router/free-forever", "messages": [{"role": "user", "content": "Hello"}] }"#).unwrap())
        .send()
        .await
        .expect("request failed");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let chat_resp: ChatResponse = resp.json().await.expect("bad json");
    assert!(!chat_resp.choices.is_empty(), "no choices");
    assert!(!chat_resp.choices[0].message.content.trim().is_empty(), "empty reply");
}