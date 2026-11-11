//! P1 Fail-closed (spec_tooling.md §13.1, §7.6): `on_client_tools = "reject"`
//! -> HTTP 501 `tool_bridge_unsupported`; tool tidak di-whitelist session ->
//! HTTP 503 `tool_isolation_unavailable`.

use agentgate::api::api;
use agentgate::bridge::ToolSchema;
use agentgate::config::ProviderConfig;
use agentgate::error::AppError;
use agentgate::permission::ClientToolsPolicy;
use agentgate::provider::{BridgeCliProvider, CliProvider, ProviderResponse};
use agentgate::service::ServiceRegistry;
use agentgate::types::{ChatCompletionRequest, FunctionDefinition, ToolDefinition};
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

fn client_tools() -> Option<Vec<ToolDefinition>> {
    Some(vec![ToolDefinition {
        r#type: "function".into(),
        function: FunctionDefinition {
            name: "demo".into(),
            description: Some("demo".into()),
            parameters: Some(serde_json::json!({"type": "object"})),
        },
    }])
}

#[tokio::test]
async fn client_tools_reject_policy_returns_501_tool_bridge_unsupported() {
    let mut registry = ServiceRegistry::new();
    registry.routing.on_client_tools = ClientToolsPolicy::Reject;
    registry.register(Box::new(DummyProvider)).await;

    let err = registry
        .chat_completion(ChatCompletionRequest {
            model: "dummy/m1".into(),
            messages: vec![],
            tools: client_tools(),
            ..Default::default()
        })
        .await
        .err()
        .expect("reject policy must fail closed");
    assert_eq!(err.code(), "tool_bridge_unsupported");
}

#[tokio::test]
async fn client_tools_without_bridge_provider_returns_501() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(DummyProvider)).await;

    let err = registry
        .chat_completion(ChatCompletionRequest {
            model: "dummy/m1".into(),
            messages: vec![],
            tools: client_tools(),
            ..Default::default()
        })
        .await
        .err()
        .expect("non-bridge provider must reject client tools");
    assert_eq!(err.code(), "tool_bridge_unsupported");
}

#[tokio::test]
async fn client_tools_reject_wins_even_with_bridge_provider() {
    let mut registry = ServiceRegistry::new();
    registry.routing.on_client_tools = ClientToolsPolicy::Reject;
    let bp = BridgeCliProvider::with_bridge(ProviderConfig {
        id: "opencode".into(),
        bin: Some("opencode".into()),
        ..Default::default()
    }, None, None).expect("bridge provider for opencode");
    registry.register(Box::new(bp)).await;

    let err = registry
        .chat_completion(ChatCompletionRequest {
            model: "opencode/m1".into(),
            messages: vec![],
            tools: client_tools(),
            ..Default::default()
        })
        .await
        .err()
        .expect("reject policy must fail closed even for bridge-capable provider");
    assert_eq!(err.code(), "tool_bridge_unsupported");
}

#[tokio::test]
async fn unknown_mcp_tool_returns_503_tool_isolation_unavailable() {
    let registry = Arc::new(ServiceRegistry::new());
    let tools = vec![ToolSchema {
        name: "demo".into(),
        description: "demo".into(),
        parameters: serde_json::json!({}),
        provider: "opencode".into(),
        workspace: "default".into(),
    }];
    let (session_id, token) = registry.bridge.create_session(tools, None).await;

    let app = api().with_state(registry.clone());
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "evil_tool", "arguments": {} }
    });
    let req = Request::builder()
        .uri(format!("/_bridge/mcp/{}", session_id))
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains("tool_isolation_unavailable"), "body: {}", s);
}

#[tokio::test]
async fn known_mcp_tool_still_accepted_when_listed() {
    let registry = Arc::new(ServiceRegistry::new());
    let tools = vec![ToolSchema {
        name: "demo".into(),
        description: "demo".into(),
        parameters: serde_json::json!({}),
        provider: "opencode".into(),
        workspace: "default".into(),
    }];
    let (session_id, token) = registry.bridge.create_session(tools, None).await;

    let app = api().with_state(registry.clone());

    // The held `tools/call` must be resolved from the outside (as the client would).
    let reg = registry.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let res = agentgate::bridge::ToolResult {
            tool_call_id: "1".into(),
            name: "demo".into(),
            content: serde_json::json!({"ok": true}),
            is_error: false,
        };
        let _ = reg.bridge.resolve_tool_call("1", res).await;
    });

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "demo", "arguments": {} }
    });
    let req = Request::builder()
        .uri(format!("/_bridge/mcp/{}", session_id))
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// Silence unused-import warnings for helper types referenced only in docs.
#[allow(dead_code)]
fn _assert_types(_: Option<AppError>) {}
