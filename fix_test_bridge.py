with open('tests/bridge_resolve_auth_test.rs', 'r') as f:
    content = f.read()

# First test: clone bridge before spawn
old1 = '''    // Hold a tool call in the background (as the MCP `tools/call` handler does).
    let bridge = registry.bridge.clone();
    let owner = session_id.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = bridge.hold_tool_call("call_auth".to_string(), owner, "demo".to_string(), 5).await;
        let _ = done_tx.send(res.map(|r| r.content.to_string()).ok());
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let pending = bridge.pending_count().await;'''

new1 = '''    // Hold a tool call in the background (as the MCP `tools/call` handler does).
    let bridge = registry.bridge.clone();
    let bridge_for_debug = bridge.clone();
    let owner = session_id.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = bridge.hold_tool_call("call_auth".to_string(), owner, "demo".to_string(), 5).await;
        let _ = done_tx.send(res.map(|r| r.content.to_string()).ok());
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let pending = bridge_for_debug.pending_count().await;'''

content = content.replace(old1, new1)

# Second test: same fix
old2 = '''    let bridge = registry.bridge.clone();
    let owner = session_id.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = bridge.hold_tool_call("call_del".to_string(), owner, "demo".to_string(), 5).await;
        let _ = done_tx.send(res.is_err());
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let pending = bridge.pending_count().await;'''

new2 = '''    let bridge = registry.bridge.clone();
    let bridge_for_debug = bridge.clone();
    let owner = session_id.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = bridge.hold_tool_call("call_del".to_string(), owner, "demo".to_string(), 5).await;
        let _ = done_tx.send(res.is_err());
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let pending = bridge_for_debug.pending_count().await;'''

content = content.replace(old2, new2)

with open('tests/bridge_resolve_auth_test.rs', 'w') as f:
    f.write(content)
print('Done')