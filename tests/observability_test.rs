//! P2 Observabilitas (spec §15): metrics endpoint + X-Agentgate-* headers +
//! structured event counters.

use agentgate::api::api;
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use agentgate::provider::ProviderResponse;
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
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> { Ok(vec!["m1".into()]) }
    async fn send_chat_completion(&self, _model: &str, _request: ChatCompletionRequest) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Success {
            content: "answer".into(),
            reasoning: None,
            tool_calls: None,
            session_id: None,
            mcp_session_id: None,
        })
    }
}

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text() {
    let registry = Arc::new(ServiceRegistry::new());
    let app = api().with_state(registry.clone());
    let req = Request::builder()
        .uri("/_metrics")
        .method(axum::http::Method::GET)
        .body(Body::empty())
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains("agentgate_runs_active"), "body: {}", s);
    assert!(s.contains("agentgate_resume_total{result=\"ok\"}"), "body: {}", s);
    assert!(s.contains("agentgate_tool_wait_seconds_sum"), "body: {}", s);
    assert!(s.contains("agentgate_ttft_seconds_count"), "body: {}", s);
}

#[tokio::test]
async fn chat_response_carries_x_agentgate_headers() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(DummyProvider)).await;
    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "dummy/m1",
        "provider": "dummy",
        "messages": []
    });
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let run = resp.headers().get("x-agentgate-run").cloned();
    let mode = resp.headers().get("x-agentgate-mode").cloned();
    assert!(run.is_some(), "missing x-agentgate-run");
    let mode = mode.expect("missing x-agentgate-mode");
    assert_eq!(mode.to_str().unwrap(), "chat_only");
}
