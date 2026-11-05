//! P0-4: `/_bridge/resolve/:call_id` must be authenticated with the token of
//! the session that owns the held call.

use agentgate::api::api;
use agentgate::bridge::ToolSchema;
use agentgate::service::ServiceRegistry;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::util::ServiceExt;

fn tools() -> Vec<ToolSchema> {
    vec![ToolSchema {
        name: "demo".to_string(),
        description: "demo".to_string(),
        parameters: serde_json::json!({"type": "object"}),
        provider: "gemini".to_string(),
        workspace: "default".to_string(),
    }]
}

async fn post_resolve(
    app: axum::Router,
    call_id: &str,
    token: Option<&str>,
) -> StatusCode {
    let mut builder = Request::builder()
        .uri(format!("/_bridge/resolve/{}", call_id))
        .method("POST")
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {}", token));
    }
    let req = builder
        .body(Body::from(
            serde_json::json!({"content": "ok", "name": "demo"}).to_string(),
        ))
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn resolve_requires_the_owning_session_token() {
    let registry = Arc::new(ServiceRegistry::new());
    let (session_id, token) = registry.bridge.create_session(tools(), None).await;
    let app = api().with_state(registry.clone());

    // Hold a tool call in the background (as the MCP `tools/call` handler does).
    let bridge = registry.bridge.clone();
    let owner = session_id.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = bridge.hold_tool_call("call_auth".to_string(), owner, 5).await;
        let _ = done_tx.send(res.map(|r| r.content.to_string()).ok());
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Missing / wrong token -> 401, and the call stays pending.
    assert_eq!(post_resolve(app.clone(), "call_auth", None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        post_resolve(app.clone(), "call_auth", Some("wrong")).await,
        StatusCode::UNAUTHORIZED
    );

    // Correct token -> 200 and the held call receives the result.
    assert_eq!(
        post_resolve(app.clone(), "call_auth", Some(&token)).await,
        StatusCode::OK
    );
    let resolved = done_rx.await.expect("hold task");
    assert_eq!(resolved.as_deref(), Some("\"ok\""));

    // Second resolve -> 404 (already resolved).
    assert_eq!(
        post_resolve(app.clone(), "call_auth", Some(&token)).await,
        StatusCode::NOT_FOUND
    );

    // Unknown call id -> 404.
    assert_eq!(
        post_resolve(app, "call_unknown", Some(&token)).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn delete_session_wakes_held_calls() {
    let registry = Arc::new(ServiceRegistry::new());
    let (session_id, _token) = registry.bridge.create_session(tools(), None).await;
    let app = api().with_state(registry.clone());

    let bridge = registry.bridge.clone();
    let owner = session_id.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = bridge.hold_tool_call("call_del".to_string(), owner, 5).await;
        let _ = done_tx.send(res.is_err());
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let req = Request::builder()
        .uri(format!("/_bridge/mcp/{}", session_id))
        .method("DELETE")
        .body(Body::empty())
        .unwrap();
    let status = app.oneshot(req).await.unwrap().status();
    assert_eq!(status, StatusCode::OK);

    // The held call is released with an error instead of waiting for the TTL.
    assert!(done_rx.await.expect("hold task"));
    assert!(registry.bridge.get_session(&session_id).await.is_none());
}
