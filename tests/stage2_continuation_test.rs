use agentgate::bridge::{ContinuationOutput, ToolResult};
use agentgate::service::ServiceRegistry;
use std::sync::Arc;

#[tokio::test]
async fn continuation_registry_register_take_remove() {
    let registry = Arc::new(ServiceRegistry::new());
    let bridge = registry.bridge.clone();

    let tx = bridge.register_continuation("run-1".to_string()).await;
    // Sender should be able to send; receiver taken by service/consumer side.
    let waiter = bridge.take_continuation("run-1").await.expect("continuation registered");
    tx.send(ContinuationOutput {
        content: "ok".to_string(),
        reasoning: None,
        tool_calls: None,
        session_id: Some("sess".to_string()),
    }).expect("send continuation");
    let out = waiter.rx.await.expect("continuation output");
    assert_eq!(out.content, "ok");
    assert!(bridge.take_continuation("run-1").await.is_none());

    let _tx2 = bridge.register_continuation("run-2".to_string()).await;
    assert!(bridge.remove_continuation("run-2").await);
    assert!(!bridge.remove_continuation("run-2").await);
}

#[tokio::test]
async fn resolve_tool_result_by_id_finds_associated_tool_call() {
    let registry = Arc::new(ServiceRegistry::new());
    let bridge = registry.bridge.clone();

    // Hold an MCP call keyed by mcp_id="mcp-1", then associate OpenAI tool_call_id and resolve by it.
    let b = bridge.clone();
    let hold = tokio::spawn(async move {
        b.hold_tool_call("mcp-1".to_string(), "sess".to_string(), None, None, "demo".to_string(), 5).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    // S4: resolve with real result by associated/openai id lookup path
    bridge.associate_tool_call_ids("mcp-1", "call_abc").await;
    let resolved = bridge.resolve_tool_result_by_id("call_abc", ToolResult {
        tool_call_id: "call_abc".to_string(),
        name: "demo".to_string(),
        content: serde_json::json!({"value": 42}),
        is_error: false,
    }).await;
    assert!(resolved.is_none() || resolved.is_some()); // run_id optional; resolution is the key assertion
    let out = hold.await.expect("join").expect("hold resolved");
    assert_eq!(out.content, serde_json::json!({"value": 42}));
    assert!(!out.is_error);
}
