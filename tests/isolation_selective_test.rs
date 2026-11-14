// Copyright 2025 AgentGate Contributors
// SPDX-License-Identifier: Apache-2.0

use agentgate::bridge::BridgeServer;
use agentgate::bridge::ToolSchema;
use agentgate::config::IsolationConfig;

fn server_with_isolation(isolation: IsolationConfig) -> (BridgeServer, IsolationConfig) {
    let server = BridgeServer::new(Default::default());
    (server, isolation)
}

fn default_server() -> (BridgeServer, IsolationConfig) {
    server_with_isolation(IsolationConfig::default())
}

/// Test preflight_active_probe rejects built-in conflicting tools
#[tokio::test]
async fn test_preflight_active_probe_rejects_conflicting() {
    let (server, isolation) = default_server();

    // Session tools containing a conflicting built-in (namespaced)
    let session_tools = vec![ToolSchema::new(
        "read".to_string(),
        "Read file".to_string(),
        serde_json::json!({}),
        "opencode".to_string(),
        "/workspace".to_string(),
    )];

    let result = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("tool_isolation_unavailable"));
}

/// Test preflight_active_probe passes when tool is explicitly allowed via allowed_builtins
#[tokio::test]
async fn test_preflight_active_probe_passes_allowed() {
    let server = BridgeServer::new(Default::default());
    let mut isolation = IsolationConfig::default();
    isolation.allowed_builtins = vec!["read".to_string(), "write".to_string()];

    // Session tools containing a conflicting built-in that is explicitly allowed
    let session_tools = vec![
        ToolSchema::new(
            "read".to_string(),
            "Read file".to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        ),
        ToolSchema::new(
            "write".to_string(),
            "Write file".to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        ),
    ];

    let result = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result.is_ok(), "preflight should pass for allowed tools: {:?}", result);
}

/// Test preflight_active_probe rejects opencode utility tools (list_mcp_resources)
#[tokio::test]
async fn test_preflight_active_probe_rejects_opencode_utils() {
    let (server, isolation) = default_server();

    // Session tools containing opencode utility tools that leak on resume
    let session_tools = vec![
        ToolSchema::new(
            "opencode_list_mcp_resources".to_string(),
            "List MCP resources".to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        ),
        ToolSchema::new(
            "list_mcp_resources".to_string(),
            "List MCP resources".to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        ),
    ];

    let result = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("tool_isolation_unavailable"));
}

/// Test that preflight_active_probe runs on every call (not cached)
#[tokio::test]
async fn test_preflight_runs_on_every_call() {
    let server = BridgeServer::new(Default::default());
    let isolation = IsolationConfig::default();

    // Session tools with conflicting built-in
    let session_tools = vec![ToolSchema::new(
        "bash".to_string(),
        "Run bash command".to_string(),
        serde_json::json!({}),
        "opencode".to_string(),
        "/workspace".to_string(),
    )];

    // First call: rejected
    let result1 = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result1.is_err());

    // Second call: also rejected (no caching)
    let result2 = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result2.is_err());

    // Third call with conflicting tool: rejected on every call
    let result3 = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result3.is_err());
}

#[tokio::test]
async fn test_selective_mode_blocks_conflicting_by_default() {
    let server = BridgeServer::new(Default::default());
    let isolation = IsolationConfig::default();

    let conflicting_tools = vec![
        ("read", "Read file"),
        ("write", "Write file"),
        ("bash", "Run bash"),
        ("execute", "Execute command"),
        ("glob", "Glob pattern"),
        ("list", "List directory"),
        ("patch", "Patch file"),
        ("edit", "Edit file"),
    ];

    for (tool_name, desc) in conflicting_tools {
        let session_tools = vec![ToolSchema::new(
            tool_name.to_string(),
            desc.to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        )];

        let result = server.preflight_active_probe(&session_tools, &isolation);
        assert!(result.is_err(), "Tool '{}' should be rejected in selective mode", tool_name);
    }
}

#[tokio::test]
async fn test_selective_mode_allows_safe_tools_by_default() {
    let server = BridgeServer::new(Default::default());
    let isolation = IsolationConfig::default();

    let safe_tools = vec![
        ("grep", "Grep search"),
        ("webfetch", "Web fetch"),
        ("todowrite", "Todo write"),
        ("todoread", "Todo read"),
        ("task", "Task tool"),
    ];

    for (tool_name, desc) in safe_tools {
        let session_tools = vec![ToolSchema::new(
            tool_name.to_string(),
            desc.to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        )];

        let result = server.preflight_active_probe(&session_tools, &isolation);
        assert!(result.is_ok(), "Tool '{}' should be allowed in selective mode", tool_name);
    }
}

#[tokio::test]
async fn test_strict_mode_blocks_all_builtins() {
    let server = BridgeServer::new(Default::default());
    let mut isolation = IsolationConfig::default();
    isolation.mode = "strict".to_string();

    let all_builtins = vec![
        ("read", "Read file"),
        ("write", "Write file"),
        ("bash", "Run bash"),
        ("execute", "Execute command"),
        ("glob", "Glob pattern"),
        ("list", "List directory"),
        ("patch", "Patch file"),
        ("edit", "Edit file"),
        ("grep", "Grep search"),
        ("webfetch", "Web fetch"),
        ("todowrite", "Todo write"),
        ("todoread", "Todo read"),
        ("task", "Task tool"),
        ("opencode_list_mcp_resources", "List MCP resources"),
        ("list_mcp_resources", "List MCP resources"),
    ];

    for (tool_name, desc) in all_builtins {
        let session_tools = vec![ToolSchema::new(
            tool_name.to_string(),
            desc.to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        )];

        let result = server.preflight_active_probe(&session_tools, &isolation);
        assert!(result.is_err(), "Tool '{}' should be rejected in strict mode", tool_name);
    }
}

#[tokio::test]
async fn test_off_mode_allows_all_builtins() {
    let server = BridgeServer::new(Default::default());
    let mut isolation = IsolationConfig::default();
    isolation.mode = "off".to_string();

    let all_builtins = vec![
        ("read", "Read file"),
        ("write", "Write file"),
        ("bash", "Run bash"),
        ("execute", "Execute command"),
        ("glob", "Glob pattern"),
        ("list", "List directory"),
        ("patch", "Patch file"),
        ("edit", "Edit file"),
        ("grep", "Grep search"),
        ("webfetch", "Web fetch"),
        ("todowrite", "Todo write"),
        ("todoread", "Todo read"),
        ("task", "Task tool"),
        ("opencode_list_mcp_resources", "List MCP resources"),
        ("list_mcp_resources", "List MCP resources"),
    ];

    for (tool_name, desc) in all_builtins {
        let session_tools = vec![ToolSchema::new(
            tool_name.to_string(),
            desc.to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        )];

        let result = server.preflight_active_probe(&session_tools, &isolation);
        assert!(result.is_ok(), "Tool '{}' should be allowed in off mode", tool_name);
    }
}

#[tokio::test]
async fn test_allowed_builtins_override_selective_mode() {
    let server = BridgeServer::new(Default::default());
    let mut isolation = IsolationConfig::default();
    isolation.allowed_builtins = vec!["bash".to_string(), "execute".to_string(), "read".to_string()];

    let allowed_tools = vec![
        ("bash", "Run bash"),
        ("execute", "Execute command"),
        ("read", "Read file"),
    ];

    for (tool_name, desc) in allowed_tools {
        let session_tools = vec![ToolSchema::new(
            tool_name.to_string(),
            desc.to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        )];

        let result = server.preflight_active_probe(&session_tools, &isolation);
        assert!(result.is_ok(), "Tool '{}' should be allowed when in allowed_builtins", tool_name);
    }

    // Tools not in allowed_builtins should still be rejected
    let rejected_tools = vec![
        ("write", "Write file"),
        ("edit", "Edit file"),
        ("glob", "Glob pattern"),
    ];

    for (tool_name, desc) in rejected_tools {
        let session_tools = vec![ToolSchema::new(
            tool_name.to_string(),
            desc.to_string(),
            serde_json::json!({}),
            "opencode".to_string(),
            "/workspace".to_string(),
        )];

        let result = server.preflight_active_probe(&session_tools, &isolation);
        assert!(result.is_err(), "Tool '{}' should be rejected when not in allowed_builtins", tool_name);
    }
}

#[tokio::test]
async fn test_disabled_builtins_override_allowed() {
    let server = BridgeServer::new(Default::default());
    let mut isolation = IsolationConfig::default();
    isolation.allowed_builtins = vec!["bash".to_string(), "read".to_string()];
    isolation.disabled_builtins = vec!["read".to_string()]; // Explicitly disable read even though it's allowed

    // read should be rejected because it's explicitly disabled
    let session_tools = vec![ToolSchema::new(
        "read".to_string(),
        "Read file".to_string(),
        serde_json::json!({}),
        "opencode".to_string(),
        "/workspace".to_string(),
    )];
    let result = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result.is_err(), "Tool 'read' should be rejected when explicitly disabled");

    // bash should still be allowed
    let session_tools = vec![ToolSchema::new(
        "bash".to_string(),
        "Run bash".to_string(),
        serde_json::json!({}),
        "opencode".to_string(),
        "/workspace".to_string(),
    )];
    let result = server.preflight_active_probe(&session_tools, &isolation);
    assert!(result.is_ok(), "Tool 'bash' should be allowed when in allowed_builtins and not disabled");
}