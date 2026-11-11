use agentgate::config::IsolationConfig;

/// Test IsolationConfig default values
#[test]
fn test_isolation_config_default() {
    let isolation = IsolationConfig::default();
    assert_eq!(isolation.mode, "selective");
    assert!(isolation.allowed_builtins.is_empty());
    assert!(isolation.disabled_builtins.is_empty());
}

/// Test render_conversation strips system messages (IMPL-TOOL-02)
#[test]
fn test_render_conversation_strips_system() {
    use agentgate::provider::render_conversation;
    use agentgate::types::{ChatMessage, MessageRole};

    let messages = vec![
        ChatMessage {
            role: MessageRole::System,
            content: Some("You are a helpful assistant".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        ChatMessage {
            role: MessageRole::User,
            content: Some("Hello".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ];

    let rendered = render_conversation(&messages, None, None);
    // System message should be stripped
    assert!(!rendered.contains("[system]"));
    assert!(!rendered.contains("You are a helpful assistant"));
    // User message should remain
    assert!(rendered.contains("[user]"));
    assert!(rendered.contains("Hello"));
}

/// Test render_conversation preserves user/assistant/tool messages
#[test]
fn test_render_conversation_preserves_non_system() {
    use agentgate::provider::render_conversation;
    use agentgate::reasoning::event::{ToolCall, FunctionCall};
    use agentgate::types::{ChatMessage, MessageRole};

    let messages = vec![
        ChatMessage {
            role: MessageRole::User,
            content: Some("Read file".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        ChatMessage {
            role: MessageRole::Assistant,
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_123".to_string(),
                r#type: "function".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "test.txt"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        },
        ChatMessage {
            role: MessageRole::Tool,
            content: Some("File content".to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_123".to_string()),
            name: None,
        },
    ];

    let rendered = render_conversation(&messages, None, None);
    assert!(rendered.contains("[user]"));
    assert!(rendered.contains("Read file"));
    assert!(rendered.contains("read_file"));
    assert!(rendered.contains(r#"{"path": "test.txt"}"#));
    assert!(rendered.contains("[tool result id=call_123"));
    assert!(rendered.contains("File content"));
}

/// Test preflight_active_probe rejects built-in conflicting tools
#[tokio::test]
async fn test_preflight_active_probe_rejects_conflicting() {
    use agentgate::bridge::BridgeServer;
    use agentgate::bridge::ToolSchema;

    let server = BridgeServer::new(Default::default());

    // Session tools containing a conflicting built-in (namespaced)
    let session_tools = vec![ToolSchema::new(
        "read".to_string(),
        "Read file".to_string(),
        serde_json::json!({}),
        "opencode".to_string(),
        "/workspace".to_string(),
    )];

    let result = server.preflight_active_probe(&session_tools);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("tool_isolation_unavailable"));
}

/// Test preflight_active_probe passes for allowed tools only
#[tokio::test]
async fn test_preflight_active_probe_passes_allowed() {
    use agentgate::bridge::BridgeServer;
    use agentgate::bridge::ToolSchema;

    let server = BridgeServer::new(Default::default());

    // Session tools with only allowed bridge tools
    let session_tools = vec![
        ToolSchema::new(
            "grep".to_string(),
            "Search".to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        ),
        ToolSchema::new(
            "task".to_string(),
            "Run task".to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        ),
    ];

    let result = server.preflight_active_probe(&session_tools);
    assert!(result.is_ok());
}