//! Regresi: tool built-in CLI (mis. `Bash`) yang tidak terdaftar di client
//! harus dipetakan ke nama tool client yang setara (`ExecCommand`), bukan
//! di-drop fail-closed.

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::reasoning::{FinishReason, RunEvent};
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{body::Body, http::Request, response::Response};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Provider mock yang meng-emit tool call `Bash` (built-in opencode).
struct EmitsBash;

#[async_trait]
impl CliProvider for EmitsBash {
    fn id(&self) -> String { "emits-bash".into() }
    fn bin(&self) -> Option<String> { Some("emits-bash".into()) }
    fn models(&self) -> Vec<String> { vec!["bash-model".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _v: bool) {}
    async fn check_available(&self) -> bool { true }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["bash-model".into()])
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
                id: "call-1".into(), r#type: "function".into(),
                function: agentgate::reasoning::event::FunctionCall {
                    name: "Bash".into(), arguments: r#"{"command":"echo hi"}"#.into(),
                },
            },
        ]}).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::ToolCalls }).await;
        Ok(())
    }
}

async fn stream_with_tools(tools: serde_json::Value) -> Response {
    let mut registry = ServiceRegistry::new();
    registry.first_token_timeout = std::time::Duration::from_secs(600);
    registry.register(Box::new(EmitsBash)).await;
    let app = api().with_state(Arc::new(registry));
    let body = serde_json::json!({
        "model": "bash-model",
        "messages": [{ "role": "user", "content": "run echo" }],
        "tools": tools,
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
async fn bash_maps_to_execcommand_when_client_registers_it() {
    let tools = serde_json::json!([{
        "type": "function",
        "function": {
            "name": "ExecCommand",
            "description": "Execute a command",
            "parameters": { "type": "object", "properties": { "command": { "type": "string" } } }
        }
    }]);
    let resp = stream_with_tools(tools).await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("ExecCommand"), "expected mapped name, got: {text}");
    assert!(!text.contains("not registered by client"), "tool was dropped: {text}");
}

#[tokio::test]
async fn bash_is_dropped_when_client_has_no_equivalent() {
    let tools = serde_json::json!([{
        "type": "function",
        "function": {
            "name": "get_time",
            "description": "Get time",
            "parameters": { "type": "object", "properties": {} }
        }
    }]);
    let resp = stream_with_tools(tools).await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("not registered by client"), "expected fail-closed, got: {text}");
    assert!(!text.contains("\"name\":\"Bash\""), "raw builtin leaked: {text}");
}
