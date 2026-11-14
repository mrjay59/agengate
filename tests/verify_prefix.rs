use agentgate::bridge::BridgeServer;

#[test]
fn prefix_normalization_strips_agentgate_ns() {
    let cases: &[(&str, &str)] = &[
        ("agentgate_read", "read"),
        ("agentgate__read", "read"),
        ("agentgate_Glob", "Glob"),
        ("agentgate__Glob", "Glob"),
        ("agentgate_shell", "shell"),
        ("agentgate_execute", "execute"),
        ("agentgate_grep", "grep"),
        ("AgentGate_Glob", "Glob"),
        ("agentgate_create", "create"),
        ("agentgate_list", "list"),
    ];
    for (input, expected) in cases {
        let stripped = BridgeServer::strip_mcp_tool_prefix(input);
        assert_eq!(
            &stripped, expected,
            "strip_mcp_tool_prefix(\"{}\") = \"{}\", expected \"{}\"", input, stripped, expected
        );
    }
}

#[test]
fn prefix_candidates_explores_all_forms() {
    let cands = BridgeServer::tool_name_candidates("agentgate_Glob");
    assert!(cands.contains(&"Glob".to_string()), "candidates: {:?}", cands);
    assert!(cands.contains(&"agentgate_Glob".to_string()));
}

#[test]
fn normalize_tool_name_resolves_against_session_tools() {
    let session_tools: Vec<agentgate::bridge::ToolSchema> = vec![
        agentgate::bridge::ToolSchema::new(
            "read_file".into(),
            "Read a file".into(),
            serde_json::json!({}),
            "agentgate".into(),
            "*".into(),
        ),
    ];
    let normalized = BridgeServer::normalize_tool_name("agentgate_read_file", &session_tools);
    assert_eq!(normalized, "read_file", "normalized: {}", normalized);
}
