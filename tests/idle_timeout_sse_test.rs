//! Regresi: saat SSE idle/first-token timeout, gateway harus mengirim warning
//! sebagai content biasa + [DONE], BUKAN chunk JSON dengan top-level "error"
//! (yang dibaca client Aionrs sebagai UNKNOWN_UPSTREAM_ERROR / "JSON error
//! response without an HTTP status").

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::reasoning::{FinishReason, RunEvent};
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{body::Body, http::Request, response::Response};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Provider yang mengirim satu token lalu diam lebih lama dari idle timeout.
struct IdleThenSilent;

#[async_trait]
impl CliProvider for IdleThenSilent {
    fn id(&self) -> String { "idle-silent".into() }
    fn bin(&self) -> Option<String> { Some("idle-silent".into()) }
    fn models(&self) -> Vec<String> { vec!["idle-model".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _v: bool) {}
    async fn check_available(&self) -> bool { true }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> { Ok(vec!["idle-model".into()]) }
    async fn send_chat_completion(
        &self, _m: &str, _r: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Success {
            content: String::new(), reasoning: None, tool_calls: None,
            session_id: None, mcp_session_id: None,
        })
    }
    async fn send_chat_completion_stream(
        &self, _m: &str, _r: ChatCompletionRequest, tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let _ = tx.send(RunEvent::Text { text: "first".into() }).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        Ok(())
    }
}

async fn sse_from_idle() -> String {
    // Set env SEBELUM ServiceRegistry::new() (ia membaca idle timeout saat init).
    std::env::set_var("AGENTGATE_STREAM_IDLE_TIMEOUT_SECS", "1");
    let mut registry = ServiceRegistry::new();
    registry.first_token_timeout = Duration::from_secs(600);
    registry.register(Box::new(IdleThenSilent)).await;
    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "idle-model",
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
    std::env::remove_var("AGENTGATE_STREAM_IDLE_TIMEOUT_SECS");
    String::from_utf8_lossy(&bytes).to_string()
}

#[tokio::test]
async fn idle_timeout_never_emits_top_level_error_object() {
    let text = sse_from_idle().await;
    for line in text.lines().filter(|l| l.starts_with("data:")) {
        let payload = line.trim_start_matches("data:").trim();
        if payload == "[DONE]" { continue; }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
            assert!(
                v.get("error").is_none(),
                "SSE chunk contained top-level error object (client will fail parse): {payload}"
            );
        }
    }
    assert!(text.contains("[DONE]"), "missing terminal [DONE]: {text}");
    assert!(text.to_ascii_lowercase().contains("no output")
        || text.to_ascii_lowercase().contains("timeout"),
        "expected a plain-text timeout warning: {text}");
}
