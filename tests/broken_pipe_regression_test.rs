//! Regression tests for client-visible `Broken pipe (os error 32)`.
//!
//! Root cause chain (see docs/TRANSPORT_HARDENING.md):
//! * a CLI child's stdin pipe closes (child exited) → `std::io` raises
//!   `Broken pipe (os error 32)` on `write_all`;
//! * the provider propagates it as a stream error;
//! * the old terminal path forwarded the raw OS string to the SSE client.
//!
//! The v0.3.35 guard (`skip_retry_when_client_gone`) only suppresses
//! retry/failover *after* the client is already gone. It does not stop the raw
//! error from reaching a still-connected client. These tests pin both halves:
//! a broken pipe before any output must be retried transparently, and a broken
//! pipe that becomes terminal must be surfaced as a clean code, never the raw
//! `os error 32`.

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
use std::time::Duration;
use tokio::sync::mpsc;

const PIPE_ERR: &str = "Broken pipe (os error 32)";

/// First stream attempt fails with a broken pipe *before any output* (a stale
/// CLI child); the second attempt succeeds. The client must never see the OS
/// error — the transient retry should recover transparently.
struct BrokenPipeThenRecover {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl CliProvider for BrokenPipeThenRecover {
    fn id(&self) -> String {
        "pipe-recover".into()
    }
    fn bin(&self) -> Option<String> {
        Some("pipe-recover".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["pipe-recover-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["pipe-recover-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error(PIPE_ERR.into()))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return Err(anyhow::anyhow!(PIPE_ERR));
        }
        let _ = tx.send(RunEvent::Text { text: "recovered".into() }).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::Stop }).await;
        Ok(())
    }
}

/// Emits partial output, then the child pipe breaks. Output was already sent so
/// no transient retry is allowed; the terminal error must be sanitized.
struct BrokenPipeAfterOutput;

#[async_trait]
impl CliProvider for BrokenPipeAfterOutput {
    fn id(&self) -> String {
        "pipe-terminal".into()
    }
    fn bin(&self) -> Option<String> {
        Some("pipe-terminal".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["pipe-terminal-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["pipe-terminal-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error(PIPE_ERR.into()))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let _ = tx.send(RunEvent::Text { text: "partial".into() }).await;
        Err(anyhow::anyhow!(PIPE_ERR))
    }
}

/// Waits for the client to hang up, then reports a broken pipe — this is the
/// exact "client disconnects mid-stream" path v0.3.35 guards.
struct DisconnectThenPipe {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl CliProvider for DisconnectThenPipe {
    fn id(&self) -> String {
        "pipe-disconnect".into()
    }
    fn bin(&self) -> Option<String> {
        Some("pipe-disconnect".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["pipe-disconnect-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["pipe-disconnect-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error(PIPE_ERR.into()))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = tx.send(RunEvent::Text { text: "streaming".into() }).await;
        // Resolve only when the SSE consumer is gone.
        let _ = tx.closed().await;
        Err(anyhow::anyhow!(PIPE_ERR))
    }
}

async fn app_with(provider: Box<dyn CliProvider + Send + Sync>) -> axum::Router {
    let mut registry = ServiceRegistry::new();
    // Keep the first-token deadline out of the way.
    registry.first_token_timeout = Duration::from_secs(600);
    registry.register(provider).await;
    api().with_state(Arc::new(registry))
}

async fn stream_request(app: axum::Router, model: &str) -> Response {
    let body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "hi" }],
        "stream": true
    });
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    tower::util::ServiceExt::oneshot(app, req).await.unwrap()
}

#[tokio::test]
async fn broken_pipe_before_output_is_retried_transparently() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = app_with(Box::new(BrokenPipeThenRecover { calls: calls.clone() })).await;

    let resp = stream_request(app, "pipe-recover-model").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("stream must terminate")
    .unwrap();
    let text = String::from_utf8_lossy(&bytes);

    assert!(text.contains("recovered"), "retry must recover, got: {text}");
    assert!(!text.contains("os error 32"), "raw OS error leaked: {text}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "expected exactly one retry");
}

#[tokio::test]
async fn terminal_broken_pipe_is_sanitized_for_the_client() {
    let app = app_with(Box::new(BrokenPipeAfterOutput)).await;

    let resp = stream_request(app, "pipe-terminal-model").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("stream must terminate")
    .unwrap();
    let text = String::from_utf8_lossy(&bytes);

    assert!(text.contains("partial"), "partial output lost: {text}");
    assert!(
        !text.contains("os error 32"),
        "raw OS error must never reach the client: {text}"
    );
    assert!(
        text.contains("provider CLI session closed"),
        "expected sanitized message, got: {text}"
    );
    assert!(text.contains("[DONE]"), "expected terminator, got: {text}");
}

#[tokio::test]
async fn client_disconnect_does_not_retry_the_pipe_error() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = app_with(Box::new(DisconnectThenPipe { calls: calls.clone() })).await;

    let resp = stream_request(app, "pipe-disconnect-model").await;
    assert_eq!(resp.status(), StatusCode::OK);
    // Drop immediately: client hung up mid-stream.
    drop(resp);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a broken pipe after client disconnect must not spawn a retry"
    );
}

/// Minimal OS-level reproduction: write to the stdin of a child that has
/// already exited. This is the exact syscall that yields
/// `Broken pipe (os error 32)` on Linux (and `os error 109/232` on Windows).
/// Documents that the transport classifier maps the real OS error correctly so
/// the sanitizer above triggers.
/// Provider emits an invalid stream event (non-string delta.content) BEFORE
/// any meaningful output → classified as InvalidOutput → retry once → recovers.
struct InvalidStreamEventThenRecover {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl CliProvider for InvalidStreamEventThenRecover {
    fn id(&self) -> String {
        "invalid-output-recover".into()
    }
    fn bin(&self) -> Option<String> {
        Some("invalid-output-recover".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["invalid-output-recover-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["invalid-output-recover-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error(
            "provider.invalid-output: Expected string at delta.content".into(),
        ))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return Err(anyhow::anyhow!(
                "provider_error: Invalid 9router/openai-compatible-chat stream event: Expected string | null | undefined at choices[0].delta.content"
            ));
        }
        let _ = tx.send(RunEvent::Text { text: "recovered".into() }).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::Stop }).await;
        Ok(())
    }
}

/// Provider emits invalid stream event AFTER partial output → terminal error,
/// must be sanitized, not raw schema error.
struct InvalidStreamEventTerminal;

#[async_trait]
impl CliProvider for InvalidStreamEventTerminal {
    fn id(&self) -> String {
        "invalid-output-terminal".into()
    }
    fn bin(&self) -> Option<String> {
        Some("invalid-output-terminal".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["invalid-output-terminal-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["invalid-output-terminal-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error(
            "provider.invalid-output: Expected string at delta.content".into(),
        ))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let _ = tx.send(RunEvent::Text { text: "partial".into() }).await;
        Err(anyhow::anyhow!(
            "provider_error: Invalid 9router/openai-compatible-chat stream event: Expected string | null | undefined at choices[0].delta.content"
        ))
    }
}

#[tokio::test]
async fn invalid_stream_event_before_output_is_retried_transparently() {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = app_with(Box::new(InvalidStreamEventThenRecover { calls: calls.clone() })).await;

    let resp = stream_request(app, "invalid-output-recover-model").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("stream must terminate")
    .unwrap();
    let text = String::from_utf8_lossy(&bytes);

    assert!(text.contains("recovered"), "retry must recover, got: {text}");
    assert!(
        !text.contains("Expected string"),
        "raw schema error leaked to client: {text}"
    );
    assert!(
        !text.contains("Invalid 9router"),
        "raw provider string leaked: {text}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "expected exactly one retry");
}

#[tokio::test]
async fn terminal_invalid_stream_event_is_sanitized_for_the_client() {
    let app = app_with(Box::new(InvalidStreamEventTerminal)).await;

    let resp = stream_request(app, "invalid-output-terminal-model").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("stream must terminate")
    .unwrap();
    let text = String::from_utf8_lossy(&bytes);

    assert!(text.contains("partial"), "partial output lost: {text}");
    assert!(
        !text.contains("Expected string"),
        "raw schema error must never reach the client: {text}"
    );
    assert!(
        !text.contains("Invalid 9router"),
        "raw provider string must never reach the client: {text}"
    );
    assert!(
        text.contains("provider emitted an invalid stream event"),
        "expected sanitized message, got: {text}"
    );
    assert!(text.contains("[DONE]"), "expected terminator, got: {text}");
}

#[tokio::test]
async fn closed_child_stdin_write_is_classified_as_broken_pipe() {
    use tokio::io::AsyncWriteExt;

    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", "exit 0"]);
        c
    };
    #[cfg(unix)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", "exit 0"]);
        c
    };

    cmd.stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn short-lived child");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let _ = child.wait().await;

    let mut raw = String::new();
    for _ in 0..4 {
        match stdin.write_all(b"prompt\n").await {
            Ok(()) => {
                let _ = stdin.flush().await;
            }
            Err(e) => {
                raw = e.to_string();
                break;
            }
        }
    }

    eprintln!("observed closed-pipe write error: {raw:?}");
    // Some platforms buffer the tiny write before reporting; only assert when
    // the OS actually surfaced the failure.
    if !raw.is_empty() {
        assert_eq!(
            agentgate::transport::classify_message(&raw),
            agentgate::transport::TransportErrorClass::BrokenPipe,
            "closed-pipe error not classified as BrokenPipe: {raw}"
        );
    }
}

/// The provider-side marker replaces the raw OS string, but must keep the
/// `broken pipe` token so the transient retry still classifies it as
/// retryable (42/139 stale-session retries succeeded on node-ai).
#[test]
fn provider_pipe_marker_stays_retryable() {
    const MARKER: &str = "provider_error: broken pipe — CLI stdin closed; stale session dropped";
    assert_eq!(
        agentgate::transport::classify_message(MARKER),
        agentgate::transport::TransportErrorClass::BrokenPipe
    );
    assert!(!MARKER.contains("os error 32"));
}