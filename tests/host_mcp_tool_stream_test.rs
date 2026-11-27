//! Regresi: tool host MCP (mis. vps_list_nodes) dieksekusi lokal oleh opencode
//! via passthrough, TIDAK boleh diteruskan sebagai tool_call ke client dan
//! TIDAK boleh menghentikan stream. Sebelum fix, emit_event me-drop tool ini
//! dan caller mengirim stop+[DONE] → "prompting stops".

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::reasoning::{FinishReason, RunEvent};
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{body::Body, http::Request, response::Response};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Provider mock: tool host MCP diikuti text "done" lalu Done.
struct EmitsHostMcpThenText;

#[async_trait]
impl CliProvider for EmitsHostMcpThenText {
    fn id(&self) -> String { "host-mcp".into() }
    fn bin(&self) -> Option<String> { Some("host-mcp".into()) }
    fn models(&self) -> Vec<String> { vec!["host-mcp-model".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _v: bool) {}
    async fn check_available(&self) -> bool { true }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["host-mcp-model".into()])
    }
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
        let _ = tx.send(RunEvent::ToolCalls { calls: vec![
            agentgate::reasoning::event::ToolCall {
                id: "call-vps".into(), r#type: "function".into(),
                function: agentgate::reasoning::event::FunctionCall {
                    name: "vps_list_nodes".into(), arguments: "{}".into(),
                },
            },
        ]}).await;
        let _ = tx.send(RunEvent::Text { text: "done".into() }).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::Stop }).await;
        Ok(())
    }
}

async fn stream_request() -> Response {
    let mut registry = ServiceRegistry::new();
    registry.first_token_timeout = std::time::Duration::from_secs(600);
    registry.register(Box::new(EmitsHostMcpThenText)).await;
    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "host-mcp-model",
        "messages": [{ "role": "user", "content": "list vps" }],
        "stream": true,
    });
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string())).unwrap();
    tower::util::ServiceExt::oneshot(app, req).await.unwrap()
}

#[tokio::test]
async fn host_mcp_tool_does_not_stop_stream() {
    let resp = stream_request().await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);

    // Turn must continue to the text after the skipped host MCP tool.
    assert!(text.contains("done"), "stream stopped before text: {text}");
    assert!(text.contains("[DONE]"), "missing terminal [DONE]: {text}");
    // Host MCP tool must not be forwarded as a client tool_call nor fatal.
    assert!(!text.contains("vps_list_nodes"), "host MCP tool leaked: {text}");
    assert!(!text.contains("not registered by client"), "fatal warning emitted: {text}");
}
