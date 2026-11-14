import re

# ============ bridge.rs changes ============

with open('src/bridge.rs', 'r') as f:
    content = f.read()

# 1. PendingCall struct: add fields
old = '''struct PendingCall {
    session_id: String,
    tx: oneshot::Sender<ToolResult>,
}'''
new = '''struct PendingCall {
    session_id: String,
    run_id: Option<String>,
    tool_name: String,
    tool_call_id: Option<String>,
    tx: oneshot::Sender<ToolResult>,
}'''
content = content.replace(old, new)

# 2. hold_tool_call: add tool_name, keepalive, deadline
old = '''    pub async fn hold_tool_call(&self, mcp_id: String, session_id: String, timeout_secs: u64) -> Result<ToolResult> {
        let (tx, rx) = oneshot::channel();
        self.pending_calls.lock().await.insert(
            mcp_id.clone(),
            PendingCall { session_id, tx },
        );

        info!(event = "bridge.tool_call.hold", mcp_id = %mcp_id, timeout = timeout_secs);

        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(res)) => {
                info!(event = "bridge.tool_call.resolved", mcp_id = %mcp_id);
                Ok(res)
            },
            Ok(Err(_)) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                Err(anyhow::anyhow!("channel closed"))
            },
            Err(_) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id);
                Err(anyhow::anyhow!("timeout menunggu hasil dari client"))
            }
        }
    }'''
new = '''    pub async fn hold_tool_call(&self, mcp_id: String, session_id: String, tool_name: String, timeout_secs: u64) -> Result<ToolResult> {
        let (tx, mut rx) = oneshot::channel();
        let run_id = {
            let sessions = self.sessions.read().await;
            sessions.get(&session_id).and_then(|s| s.run_id.clone())
        };

        self.pending_calls.lock().await.insert(
            mcp_id.clone(),
            PendingCall { session_id: session_id.clone(), run_id, tool_name: tool_name.clone(), tool_call_id: None, tx },
        );

        info!(event = "bridge.tool_call.hold", mcp_id = %mcp_id, tool = %tool_name, timeout = timeout_secs);

        let keepalive_interval = self.config.keepalive_interval_secs;
        let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(keepalive_interval));
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let deadline = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs));
        tokio::pin!(deadline);

        let pending_clone = self.pending_calls.clone();
        let mcp_id_clone = mcp_id.clone();

        loop {
            tokio::select! {
                res = &mut rx => {
                    match res {
                        Ok(result) => {
                            info!(event = "bridge.tool_call.resolved", mcp_id = %mcp_id);
                            return Ok(result);
                        }
                        Err(_) => {
                            pending_clone.lock().await.remove(&mcp_id);
                            return Err(anyhow::anyhow!("channel closed"));
                        }
                    }
                }
                _ = keepalive.tick() => {
                    crate::metrics::METRICS.record_bridge_keepalive();
                    debug!(event = "bridge.tool_call.keepalive", mcp_id = %mcp_id);
                }
                _ = &mut deadline => {
                    pending_clone.lock().await.remove(&mcp_id_clone);
                    info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id);
                    return Err(anyhow::anyhow!("timeout menunggu hasil dari client"));
                }
            }
        }
    }'''
content = content.replace(old, new)

# 3. resolve_tool_call: return Option<String>, add new methods
old = '''    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> bool {
        if let Some(pending) = self.pending_calls.lock().await.remove(mcp_id) {
            let _ = pending.tx.send(result);
            true
        } else {
            false
        }
    }

    /// Session that owns a held `tools/call`, if still pending.
    pub async fn pending_session_id(&self, mcp_id: &str) -> Option<String> {'''
new = '''    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> Option<String> {
        if let Some(pending) = self.pending_calls.lock().await.remove(mcp_id) {
            let _ = pending.tx.send(result);
            pending.run_id
        } else {
            None
        }
    }

    /// Resolve a held MCP call by OpenAI tool_call_id (resume role:tool path).
    /// Returns the associated run_id if found.
    pub async fn resolve_by_openai_tool_call_id(&self, tool_call_id: &str) -> Option<String> {
        let mut pending = self.pending_calls.lock().await;
        let key = pending.iter().find(|(_, p)| p.tool_call_id.as_deref() == Some(tool_call_id)).map(|(k, _)| k.clone());
        if let Some(k) = key {
            if let Some(p) = pending.remove(&k) {
                return p.run_id;
            }
        }
        None
    }

    /// Associate an OpenAI tool_call_id with a pending MCP call (mcp_id).
    /// Enables resume path to find the held call by OpenAI tool_call_id.
    pub async fn associate_tool_call_ids(&self, mcp_id: &str, tool_call_id: String) -> Result<()> {
        let mut pending = self.pending_calls.lock().await;
        if let Some(p) = pending.get_mut(mcp_id) {
            p.tool_call_id = Some(tool_call_id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("no pending call for mcp_id {}", mcp_id))
        }
    }

    /// Session that owns a held `tools/call`, if still pending.
    pub async fn pending_session_id(&self, mcp_id: &str) -> Option<String> {'''
content = content.replace(old, new)

# 4. execute_tool PendingCall insert: add run_id, tool_name
old = '''        let (tx, rx) = oneshot::channel();
        self.pending_calls
            .lock()
            .await
            .insert(mcp_id.clone(), PendingCall { session_id: String::new(), tx });'''
new = '''        let (tx, rx) = oneshot::channel();
        let run_id_opt = None;
        self.pending_calls
            .lock()
            .await
            .insert(mcp_id.clone(), PendingCall { session_id: String::new(), run_id: run_id_opt, tool_name: tool_name.to_string(), tool_call_id: None, tx });'''
content = content.replace(old, new)

# 5. BridgeSession struct: add isolation_active field
old = '''pub struct BridgeSession {
    pub session_id: String,
    pub token: String,
    pub run_id: Option<String>,
    pub tools: Vec<ToolSchema>,
    pub created_at: std::time::Instant,
}'''
new = '''pub struct BridgeSession {
    pub session_id: String,
    pub token: String,
    pub run_id: Option<String>,
    pub tools: Vec<ToolSchema>,
    pub created_at: std::time::Instant,
    pub isolation_active: bool,
}'''
content = content.replace(old, new)

# 6. Add impl BridgeSession block with preflight_isolation_check and is_bridge_tool
old = '''#[derive(Clone)]
pub struct BridgeServer {'''
new = '''impl BridgeSession {
    /// Fail-closed guard: returns true only when isolation/guard patterns are active
    /// for this session's provider (opencode/claude/gemini config-level).
    pub fn preflight_isolation_check(&self) -> bool {
        self.isolation_active
    }

    /// Returns true if `name` is a bridge-managed tool (i.e. present in session's tool list).
    pub fn is_bridge_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name == name)
    }
}

#[derive(Clone)]
pub struct BridgeServer {'''
content = content.replace(old, new)

# 7. Add pending_count to BridgeServer impl
old = '''impl BridgeServer {
    pub fn new(config: BridgeConfig) -> Self {'''
new = '''impl BridgeServer {
    pub async fn pending_count(&self) -> usize {
        self.pending_calls.lock().await.len()
    }

    pub fn new(config: BridgeConfig) -> Self {'''
content = content.replace(old, new)

# 8. Session creation: add isolation_active: true
old = '''            tools,
            created_at: std::time::Instant::now(),
        };
        self.sessions.write().await.insert(session_id.clone(), session.clone());'''
new = '''            tools,
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        self.sessions.write().await.insert(session_id.clone(), session.clone());'''
content = content.replace(old, new)

# 9. Update test BridgeSession constructions
old = '''        BridgeSession {
            session_id: "test-session".to_string(),
            token: "test-token".to_string(),
            run_id: Some("test-run-1".to_string()),
            tools: vec![],
            created_at: std::time::Instant::now(),
        }'''
new = '''        BridgeSession {
            session_id: "test-session".to_string(),
            token: "test-token".to_string(),
            run_id: Some("test-run-1".to_string()),
            tools: vec![],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        }'''
content = content.replace(old, new)

# 10. Add normalize_tool_name after sanitize_tool_name
old = '''/// Session that owns a held `tools/call`, if still pending.'''
new = '''/// Normalize tool name for bridge lookup (case-insensitive for builtin tools like Read/read).
/// Returns the normalized name as it appears in the session's tool list.
pub fn normalize_tool_name(name: &str, session_tools: &[crate::bridge::ToolSchema]) -> String {
    // First try exact match
    if session_tools.iter().any(|t| t.name == name) {
        return name.to_string();
    }
    // Try case-insensitive match for known builtin tools
    let lower = name.to_lowercase();
    for t in session_tools {
        if t.name.to_lowercase() == lower {
            return t.name.clone(); // Return the canonical casing from session
        }
    }
    // Fallback: return original name
    name.to_string()
}

/// Session that owns a held `tools/call`, if still pending.'''
content = content.replace(old, new)

# 11. Add tests at end of test module (before closing })
old = '''        assert!(session.is_bridge_tool("Read"));
        assert!(!session.is_bridge_tool("Unknown"));
    }
}'''
new = '''        assert!(session.is_bridge_tool("Read"));
        assert!(!session.is_bridge_tool("Unknown"));
    }

    #[test]
    fn test_normalize_tool_name_exact_match() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![ToolSchema { name: "Read".to_string(), description: "".to_string(), parameters: serde_json::json!({}), provider: "test".to_string(), workspace: "test".to_string() }],
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
            tools: vec![ToolSchema { name: "Read".to_string(), description: "".to_string(), parameters: serde_json::json!({}), provider: "test".to_string(), workspace: "test".to_string() }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        let normalized = normalize_tool_name("read", &session.tools);
        assert_eq!(normalized, "Read");
    }

    #[test]
    fn test_normalize_tool_name_fallback() {
        let session = BridgeSession {
            session_id: "test".to_string(),
            token: "test".to_string(),
            run_id: None,
            tools: vec![ToolSchema { name: "Read".to_string(), description: "".to_string(), parameters: serde_json::json!({}), provider: "test".to_string(), workspace: "test".to_string() }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        let normalized = normalize_tool_name("UnknownTool", &session.tools);
        assert_eq!(normalized, "UnknownTool");
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
            tools: vec![ToolSchema { name: "Read".to_string(), description: "".to_string(), parameters: serde_json::json!({}), provider: "test".to_string(), workspace: "test".to_string() }],
            created_at: std::time::Instant::now(),
            isolation_active: true,
        };
        assert!(session.is_bridge_tool("Read"));
        assert!(!session.is_bridge_tool("Unknown"));
    }
}'''
content = content.replace(old, new)

with open('src/bridge.rs', 'w') as f:
    f.write(content)
print('bridge.rs done')