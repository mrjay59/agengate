//! P0-1: prove that `BridgeCliProvider` registers its MCP session with the
//! shared `BridgeServer` so `/_bridge/mcp/{session_id}` is reachable with the
//! session token (previously the endpoint always returned 404).

use agentgate::api::api;
use agentgate::bridge::ToolSchema;
use agentgate::config::ProviderConfig;
use agentgate::provider::BridgeCliProvider;
use agentgate::service::ServiceRegistry;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::util::ServiceExt;

fn provider_config() -> ProviderConfig {
    ProviderConfig {
        id: "gemini".to_string(),
        bin: Some("gemini".to_string()),
        ..Default::default()
    }
}

fn demo_tools() -> Vec<ToolSchema> {
    vec![ToolSchema {
        name: "demo".to_string(),
        description: "demo tool".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
        provider: "gemini".to_string(),
        workspace: "default".to_string(),
    }]
}

async fn rpc(
    app: axum::Router,
    session_id: &str,
    token: &str,
    method: &str,
) -> (StatusCode, String) {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": {}
    });
    let req = Request::builder()
        .uri(format!("/_bridge/mcp/{}", session_id))
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn bridge_cli_provider_registers_reachable_session() {
    let registry = Arc::new(ServiceRegistry::new());
    let provider = BridgeCliProvider::with_bridge(provider_config(), Some(registry.bridge.clone()))
        .expect("gemini bridge provider");

    let (session_id, token) = provider
        .register_session(demo_tools())
        .await
        .expect("session registered");

    // The server must know the exact id/token handed to the adapter.
    let stored = registry
        .bridge
        .get_session(&session_id)
        .await
        .expect("session exists in BridgeServer");
    assert_eq!(stored.token, token);

    let app = api().with_state(registry.clone());

    let (status, body) = rpc(app.clone(), &session_id, &token, "initialize").await;
    assert_eq!(status, StatusCode::OK, "initialize body: {}", body);
    assert!(body.contains("protocolVersion"), "body: {}", body);

    let (status, body) = rpc(app.clone(), &session_id, &token, "tools/list").await;
    assert_eq!(status, StatusCode::OK, "tools/list body: {}", body);
    assert!(body.contains("demo"), "body: {}", body);

    // Wrong token must still be rejected.
    let (status, _) = rpc(app, &session_id, "wrong-token", "tools/list").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Teardown removes the session (subsequent requests 404).
    assert!(registry.bridge.remove_session(&session_id).await);
    let app = api().with_state(registry.clone());
    let (status, _) = rpc(app, &session_id, &token, "tools/list").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bridge_cli_provider_without_bridge_returns_none() {
    let provider = BridgeCliProvider::with_bridge(provider_config(), None).expect("factory");
    assert!(provider.register_session(demo_tools()).await.is_none());
}

#[test]
fn build_cli_provider_selects_bridge_for_opencode() {
    let cfg = ProviderConfig {
        id: "opencode".to_string(),
        bin: Some("opencode".to_string()),
        bridge: true,
        ..Default::default()
    };
    let provider = agentgate::provider::build_cli_provider(&cfg, None);
    assert_eq!(provider.id(), "opencode");
    assert!(provider.is_bridge());
}

#[test]
fn build_cli_provider_honours_bridge_false() {
    let cfg = ProviderConfig {
        id: "opencode".to_string(),
        bin: Some("opencode".to_string()),
        bridge: false,
        ..Default::default()
    };
    let provider = agentgate::provider::build_cli_provider(&cfg, None);
    assert_eq!(provider.id(), "opencode");
    assert!(!provider.is_bridge());
}

#[test]
fn build_cli_provider_falls_back_for_unknown_agent() {
    let cfg = ProviderConfig {
        id: "custom".to_string(),
        bin: Some("custom".to_string()),
        bridge: true,
        ..Default::default()
    };
    let provider = agentgate::provider::build_cli_provider(&cfg, None);
    assert!(!provider.is_bridge());
}

#[test]
fn render_conversation_keeps_roles_tool_calls_and_results() {
    use agentgate::types::{ChatMessage, FunctionCall, MessageRole, ToolCall};

    let messages = vec![
        ChatMessage::text("system", "kamu asisten"),
        ChatMessage::text("user", "buat file hello.txt"),
        ChatMessage {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: "write_file".to_string(),
                    arguments: r#"{"path":"hello.txt"}"#.to_string(),
                },
            }]),
            ..Default::default()
        },
        ChatMessage::tool_result(
            "call_1".to_string(),
            "write_file".to_string(),
            "OK: 4 bytes".to_string(),
        ),
    ];

    let rendered = agentgate::provider::render_conversation(&messages);
    assert!(rendered.contains("[system]"));
    assert!(rendered.contains("kamu asisten"));
    assert!(rendered.contains("[user]"));
    assert!(rendered.contains("name=write_file"));
    assert!(rendered.contains(r#"{"path":"hello.txt"}"#));
    assert!(rendered.contains("[tool result id=call_1"));
    assert!(rendered.contains("OK: 4 bytes"));
}

#[test]
fn build_cli_provider_selects_bridge_for_ollama_cli_mode() {
    let cfg = ProviderConfig {
        id: "ollama".to_string(),
        bin: None,
        ollama_mode: Some("cli".to_string()),
        ..Default::default()
    };
    let provider = agentgate::provider::build_cli_provider(&cfg, None);
    assert_eq!(provider.id(), "ollama");
    assert!(provider.is_bridge());
}

#[test]
fn build_cli_provider_keeps_ollama_native_by_default() {
    let cfg = ProviderConfig {
        id: "ollama".to_string(),
        bin: None,
        ollama_mode: None,
        ..Default::default()
    };
    let provider = agentgate::provider::build_cli_provider(&cfg, None);
    assert_eq!(provider.id(), "ollama");
    assert!(!provider.is_bridge());
}
