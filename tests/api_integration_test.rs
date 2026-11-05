use agentgate::api::api;
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use agentgate::provider::CliProvider;
use async_trait::async_trait;
use axum::{body::Body, http::{Request, StatusCode}, response::Response};
use std::sync::Arc;
use tower::util::ServiceExt;

struct DummyProvider;
#[async_trait]
impl CliProvider for DummyProvider {
    fn id(&self) -> String { "dummy".into() }
    fn bin(&self) -> Option<String> { Some("dummy".into()) }
    fn models(&self) -> Vec<String> { vec!["m1".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool { true }
    fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["m1".into()])
    }
    async fn send_chat_completion(&self, _model: &str, _request: ChatCompletionRequest) -> Result<agentgate::provider::ProviderResponse, anyhow::Error> {
        Ok(agentgate::provider::ProviderResponse::Success {
            content: "answer".to_string(),
            reasoning: Some("thinking about it".to_string()),
            tool_calls: None,
            session_id: None,
        })
    }
}

#[tokio::test]
async fn test_models_endpoint() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(DummyProvider)).await;
    let app = api().with_state(Arc::new(registry));
    let req = Request::builder()
        .uri("/v1/models")
        .method(axum::http::Method::GET)
        .body(Body::empty())
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body_str = std::str::from_utf8(&bytes).unwrap();
    assert!(body_str.contains("m1"));
}

#[tokio::test]
async fn test_chat_missing_model() {
    let registry = ServiceRegistry::new();
    let app = api().with_state(Arc::new(registry));
    let request_body = serde_json::json!({"model":"unknown","messages":[]});
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(request_body.to_string()))
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_health_check() {
    let registry = ServiceRegistry::new();
    let app = api().with_state(Arc::new(registry));
    let req = Request::builder()
        .uri("/health")
        .method(axum::http::Method::GET)
        .body(Body::empty())
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

async fn chat_stream(
    headers: &[(&str, &str)],
    request_body: serde_json::Value,
) -> (axum::http::HeaderMap, String) {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(DummyProvider)).await;
    let app = api().with_state(Arc::new(registry));
    let mut builder = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(Body::from(request_body.to_string())).unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let response_headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK, "body: {body}");
    (response_headers, body)
}

#[tokio::test]
async fn test_streaming_emits_reasoning_then_text_then_done() {
    let (_headers, body) = chat_stream(
        &[],
        serde_json::json!({
            "model": "m1",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;

    assert!(body.contains("\"reasoning_content\":\"thinking about it\""), "{body}");
    assert!(body.contains("answer"), "{body}");
    assert!(body.contains("\"finish_reason\":\"stop\""), "{body}");
    assert!(body.contains("[DONE]"), "{body}");
    // Reasoning must precede the answer.
    let reasoning = body.find("thinking about it").unwrap();
    let answer = body.find("answer").unwrap();
    assert!(reasoning < answer, "reasoning must precede text");
}

#[tokio::test]
async fn test_streaming_none_format_suppresses_reasoning() {
    let (headers, body) = chat_stream(
        &[("x-agentgate-reasoning", "none")],
        serde_json::json!({
            "model": "m1",
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }),
    )
    .await;

    assert_eq!(headers["x-agentgate-reasoning-format"], "none");
    assert!(!body.contains("reasoning_content"), "{body}");
    assert!(!body.contains("thinking about it"), "{body}");
    assert!(body.contains("answer"), "{body}");
}