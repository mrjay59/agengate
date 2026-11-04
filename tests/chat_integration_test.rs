use agentgate::types::{ChatCompletionRequest, ChatCompletionResponse, ChatMessage, MessageRole};
use reqwest::Client;
use tokio;

#[tokio::test]
#[ignore = "requires a running agentgate server (AGENTGATE_URL); run with --ignored"]
async fn test_chat_completion() {
    let base = std::env::var("AGENTGATE_URL").unwrap_or_else(|_| "http://localhost:8787".to_string());
    let client = Client::new();
    let req = ChatCompletionRequest {
        model: "9router/ag-poli".to_string(),
        provider: None,
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: Some("Hello world".to_string()),
            reasoning_content: None,
            ..Default::default()
        }],
        temperature: None,
        max_tokens: None,
        top_p: None,
        stream: Some(false),
        ..Default::default()
    };
    let resp = client
        .post(&format!("{}/v1/chat/completions", base))
        .json(&req)
        .send()
        .await
        .expect("request failed");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    let chat: ChatCompletionResponse = resp.json().await.expect("invalid json");
    println!("Response: {:#?}", chat);
    assert!(!chat.choices.is_empty(), "no choices");
    assert!(
        !chat.choices[0]
            .message
            .content
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty(),
        "empty reply"
    );
}
