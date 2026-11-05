use agentgate::api::api;
use agentgate::bridge::BridgeServer;
use agentgate::bridge::ToolResult;
use agentgate::service::ServiceRegistry;
use agentgate::bridge::ToolSchema;
use axum::{body::Body, http::{Request, StatusCode}, response::Response};
use std::sync::Arc;
use tower::util::ServiceExt;

#[tokio::test]
async fn bridge_mcp_hold_and_resolve() {
    let registry = Arc::new(ServiceRegistry::new());
    let (session_id, token) = registry.bridge.create_session(vec![ToolSchema {
        name: "demo".into(),
        description: "demo".into(),
        parameters: serde_json::json!({}),
        provider: "opencode".into(),
        workspace: "default".into(),
    }], None).await;

    let app = api().with_state(registry.clone());
    let uri = format!("/_bridge/mcp/{}", session_id);

    // Resolve in background after short delay
    let reg = registry.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let res = ToolResult {
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
        .uri(uri)
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains("ok"));
}
