with open('src/bridge.rs', 'r') as f:
    content = f.read()

# Find the end of the test module
idx = content.rfind('}')
# Replace the last '}' with new tests + '}'
new_tests = '''

    #[test]
    fn test_normalize_tool_name_exact_match() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![ToolSchema { name: "Read".to_string(), description: None, input_schema: serde_json::json!({}) }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        let normalized = normalize_tool_name("Read", &session.tools);
        assert_eq!(normalized, "Read");
    }

    #[test]
    fn test_normalize_tool_name_case_insensitive() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![ToolSchema { name: "Read".to_string(), description: None, input_schema: serde_json::json!({}) }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        let normalized = normalize_tool_name("read", &session.tools);
        assert_eq!(normalized, "Read"); // Returns canonical casing from session
    }

    #[test]
    fn test_normalize_tool_name_fallback() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![ToolSchema { name: "Read".to_string(), description: None, input_schema: serde_json::json!({}) }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        let normalized = normalize_tool_name("UnknownTool", &session.tools);
        assert_eq!(normalized, "UnknownTool"); // Fallback to original
    }

    #[test]
    fn test_preflight_isolation_check_active() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        assert!(session.preflight_isolation_check());
    }

    #[test]
    fn test_preflight_isolation_check_inactive() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![],
            created_at: std::time::Instant::now(),
            isolation_active: false,
        };
        assert!(!session.preflight_isolation_check());
    }

    #[test]
    fn test_is_bridge_tool() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![ToolSchema { name: "Read".to_string(), description: None, input_schema: serde_json::json!({}) }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        assert!(session.is_bridge_tool("Read"));
        assert!(!session.is_bridge_tool("Unknown"));
    }
}
'''

new_content = content[:idx] + new_tests + content[idx:]

with open('src/bridge.rs', 'w') as f:
    f.write(new_content)
print('Done')