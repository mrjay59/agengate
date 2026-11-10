//! Regression tests for the "no response" class of failures seen on the Linux VPS.
//!
//! A CLI provider that never emits anything (missing model → multi-GB auto-pull,
//! CLI that does not exit after an upstream quota error, stuck process) used to
//! keep the SSE stream alive forever: the client only ever received
//! `: keep-alive` comments, never an error chunk and never `data: [DONE]`.
//!
//! These tests pin down the fail-fast behaviour:
//! * a provider that stays silent → `first_token_timeout` error chunk + `[DONE]`
//! * a provider that reports an error and then hangs → terminal error chunk + `[DONE]`
//!
//! Both must finish in seconds, not minutes.

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};use agentgate::reasoning::RunEvent;
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tower::util::ServiceExt;

/// Provider that accepts the request and then never says anything.
struct SilentProvider;

#[async_trait]
impl CliProvider for SilentProvider {
    fn id(&self) -> String {
        "silent".into()
    }
    fn bin(&self) -> Option<String> {
        Some("silent".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["silent-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["silent-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        tokio::time::sleep(Duration::from_secs(600)).await;
        unreachable!("silent provider must never answer")
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        _tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        // Hangs while holding the sender: without a deadline the stream never ends.
        tokio::time::sleep(Duration::from_secs(600)).await;
        Ok(())
    }
}

/// Provider that reports a terminal error (like opencode's quota error) and then
/// keeps the process/sender alive, exactly like `opencode-ai` 1.19.5 does.
struct ErrorThenHangProvider;

#[async_trait]
impl CliProvider for ErrorThenHangProvider {
    fn id(&self) -> String {
        "errhang".into()
    }
    fn bin(&self) -> Option<String> {
        Some("errhang".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["errhang-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["errhang-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error(
            "AI_APICallError: Free usage limit reached".to_string(),
        ))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let _ = tx
            .send(RunEvent::Error {
                message: "AI_APICallError: Free usage limit reached".to_string(),
                code: Some("provider_error".to_string()),
            })
            .await;
        // The CLI does not exit after reporting the error.
        tokio::time::sleep(Duration::from_secs(600)).await;
        Ok(())
    }
}

async fn registry_with_fast_timeout(
    provider: Box<dyn CliProvider + Send + Sync>,
) -> Arc<ServiceRegistry> {
    let mut registry = ServiceRegistry::new();
    // Fail fast in tests instead of waiting the production 120 s.
    registry.first_token_timeout = Duration::from_secs(1);
    registry.register(provider).await;
    Arc::new(registry)
}

async fn stream_body(app: axum::Router, model: &str) -> (Response, Duration) {
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
    let start = Instant::now();
    let resp: Response = app.oneshot(req).await.unwrap();
    (resp, start.elapsed())
}

#[tokio::test]
async fn silent_provider_emits_first_token_timeout_and_done() {
    let registry = registry_with_fast_timeout(Box::new(SilentProvider)).await;
    let app = api().with_state(registry);

    let (resp, _) = stream_body(app, "silent-model").await;
    assert_eq!(resp.status(), StatusCode::OK);

    // The body must terminate well before the 600 s provider sleep.
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("SSE body must terminate instead of hanging forever")
    .unwrap();

    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("first_token_timeout"),
        "expected a first_token_timeout error chunk, got: {text}"
    );
    assert!(
        text.contains("[DONE]"),
        "expected the SSE terminator, got: {text}"
    );
}

#[tokio::test]
async fn provider_error_is_terminal_even_if_process_stays_alive() {
    let registry = registry_with_fast_timeout(Box::new(ErrorThenHangProvider)).await;
    let app = api().with_state(registry);

    let (resp, _) = stream_body(app, "errhang-model").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let start = Instant::now();
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("an explicit provider error must terminate the stream immediately")
    .unwrap();
    let elapsed = start.elapsed();

    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("AI_APICallError: Free usage limit reached"),
        "provider error message must reach the client, got: {text}"
    );
    assert!(
        text.contains("[DONE]"),
        "expected the SSE terminator, got: {text}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "terminal error took {elapsed:?}, expected < 10s"
    );
}

/// Provider that behaves like a real CLI child: it produces no output until
/// either the client hangs up or a very long timeout elapses. `cancelled` proves
/// the disconnect actually reached the provider task.
struct DisconnectAwareProvider {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl CliProvider for DisconnectAwareProvider {
    fn id(&self) -> String {
        "disconnect".into()
    }
    fn bin(&self) -> Option<String> {
        Some("disconnect".into())
    }
    fn models(&self) -> Vec<String> {
        vec!["disconnect-model".into()]
    }
    fn is_available(&self) -> bool {
        true
    }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool {
        true
    }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["disconnect-model".into()])
    }
    async fn send_chat_completion(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Error("not used".into()))
    }
    async fn send_chat_completion_stream(
        &self,
        _model: &str,
        _request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        // Mirrors the CLI child wait: stop as soon as the client is gone.
        tokio::select! {
            _ = tx.closed() => {
                self.cancelled
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            _ = tokio::time::sleep(Duration::from_secs(600)) => {}
        }
        Ok(())
    }
}

#[tokio::test]
async fn client_disconnect_stops_the_provider_task() {
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut registry = ServiceRegistry::new();
    // Keep the first-token deadline out of the way: only the client disconnect
    // may close the provider channel in this test.
    registry.first_token_timeout = Duration::from_secs(600);
    registry
        .register(Box::new(DisconnectAwareProvider {
            cancelled: cancelled.clone(),
        }))
        .await;
    let registry = Arc::new(registry);
    let app = api().with_state(registry);

    let (resp, _) = stream_body(app, "disconnect-model").await;
    assert_eq!(resp.status(), StatusCode::OK);
    // Drop the response body immediately: this is exactly what curl/browsers do
    // when the user cancels a request.
    drop(resp);

    let deadline = Instant::now() + Duration::from_secs(10);
    while !cancelled.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        cancelled.load(std::sync::atomic::Ordering::SeqCst),
        "dropping the SSE body must close the provider channel so the CLI child is killed"
    );
}
