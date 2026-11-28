//! Regresi: EPIPE/readiness sebelum output harus di-retry sampai 3x dengan
//! session baru, bukan langsung terminal "provider CLI session closed mid-stream".
//! Setelah output bermakna, retry TIDAK boleh terjadi (mencegah duplikasi).

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::reasoning::{FinishReason, RunEvent};
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{body::Body, http::Request, response::Response};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::mpsc;

const PIPE_ERR: &str = "Broken pipe (os error 32)";

struct FailNThenSucceed { failures: usize, calls: Arc<AtomicUsize>, emit_text_first: bool }

#[async_trait]
impl CliProvider for FailNThenSucceed {
    fn id(&self) -> String { "pipe-budget".into() }
    fn bin(&self) -> Option<String> { Some("pipe-budget".into()) }
    fn models(&self) -> Vec<String> { vec!["pipe-budget-model".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _v: bool) {}
    async fn check_available(&self) -> bool { true }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> { Ok(self.models()) }
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
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.emit_text_first {
            let _ = tx.send(RunEvent::Text { text: "partial".into() }).await;
            return Err(anyhow::anyhow!(PIPE_ERR));
        }
        if n < self.failures {
            return Err(anyhow::anyhow!(PIPE_ERR));
        }
        let _ = tx.send(RunEvent::Text { text: "done".into() }).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::Stop }).await;
        Ok(())
    }
}

async fn run_case(provider: FailNThenSucceed) -> String {
    let mut registry = ServiceRegistry::new();
    registry.first_token_timeout = std::time::Duration::from_secs(600);
    registry.register(Box::new(provider)).await;
    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "pipe-budget-model",
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
    String::from_utf8_lossy(&bytes).to_string()
}

#[tokio::test]
async fn two_broken_pipes_then_success_no_terminal_warning() {
    let calls = Arc::new(AtomicUsize::new(0));
    let text = run_case(FailNThenSucceed {
        failures: 2, calls: calls.clone(), emit_text_first: false,
    }).await;
    assert!(text.contains("done"), "stream must recover: {text}");
    assert!(!text.contains("provider CLI session closed"), "terminal warning leaked: {text}");
    assert!(calls.load(Ordering::SeqCst) >= 3, "expected >=3 attempts, got {}", calls.load(Ordering::SeqCst));
}

#[tokio::test]
async fn broken_pipe_after_output_is_terminal_without_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let text = run_case(FailNThenSucceed {
        failures: 99, calls: calls.clone(), emit_text_first: true,
    }).await;
    assert!(text.contains("partial"), "partial output lost: {text}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "must not retry after meaningful output");
}
