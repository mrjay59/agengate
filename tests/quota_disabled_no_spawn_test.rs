//! Regresi: model yang sudah quota-disabled TIDAK boleh diteruskan ke
//! opencode CLI (tidak ada spawn / send_chat_completion), baik jalur non-stream
//! maupun stream. Fail fast dengan 429 quota_exceeded / error code quota_exceeded.

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::reasoning::{FinishReason, RunEvent};
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::mpsc;

/// Provider spy: menghitung setiap kali CLI dipanggil.
struct SpyProvider { calls: Arc<AtomicUsize> }

#[async_trait]
impl CliProvider for SpyProvider {
    fn id(&self) -> String { "opencode".into() }
    fn bin(&self) -> Option<String> { Some("opencode".into()) }
    fn models(&self) -> Vec<String> { vec!["opencode/beam-free".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _v: bool) {}
    async fn check_available(&self) -> bool { true }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> { Ok(self.models()) }
    async fn send_chat_completion(
        &self, _m: &str, _r: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ProviderResponse::Success {
            content: "should-not-run".into(), reasoning: None, tool_calls: None,
            session_id: None, mcp_session_id: None,
        })
    }
    async fn send_chat_completion_stream(
        &self, _m: &str, _r: ChatCompletionRequest, tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = tx.send(RunEvent::Text { text: "should-not-run".into() }).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::Stop }).await;
        Ok(())
    }
}

fn registry_with(calls: Arc<AtomicUsize>) -> ServiceRegistry {
    let mut registry = ServiceRegistry::new();
    registry.first_token_timeout = std::time::Duration::from_secs(600);
    let _ = calls;
    registry
}

#[tokio::test]
async fn quota_disabled_non_stream_does_not_call_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = registry_with(calls.clone());
    registry.register(Box::new(SpyProvider { calls: calls.clone() })).await;
    registry.mark_model_quota_disabled("opencode", "opencode/beam-free", "quota_exhausted").await;
    // Also disable the hard fallback so reroute has no healthy target.
    registry.mark_model_quota_disabled("opencode", "opencode/9router/free-forever", "quota_exhausted").await;

    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "opencode/beam-free",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": false,
    });
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap();
    let resp: Response = tower::util::ServiceExt::oneshot(app, req).await.unwrap();

    // Guard must fire before any CLI call.
    assert_eq!(calls.load(Ordering::SeqCst), 0, "CLI was called for a quota-disabled model");
    // Either 429 from handler or an error body — but never a successful turn.
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("should-not-run"), "disabled model produced content: {text}");
}

#[tokio::test]
async fn quota_disabled_stream_does_not_call_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = registry_with(calls.clone());
    registry.register(Box::new(SpyProvider { calls: calls.clone() })).await;
    registry.mark_model_quota_disabled("opencode", "opencode/beam-free", "quota_exhausted").await;
    // Also disable the hard fallback so reroute has no healthy target.
    registry.mark_model_quota_disabled("opencode", "opencode/9router/free-forever", "quota_exhausted").await;

    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "opencode/beam-free",
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": true,
    });
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap();
    let resp: Response = tower::util::ServiceExt::oneshot(app, req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);

    assert_eq!(calls.load(Ordering::SeqCst), 0, "CLI was called for a quota-disabled model (stream)");
    assert!(!text.contains("should-not-run"), "disabled model produced content: {text}");
    assert!(text.contains("quota_exceeded"), "expected quota_exceeded code, got: {text}");
}
