$path = "C:\Users\EDN\Documents\AION\project\agentgate\src\bridge.rs"
$content = [System.IO.File]::ReadAllText($path)

# 1. Update PendingCall struct
$old1 = @"
struct PendingCall {
    session_id: String,
    tx: oneshot::Sender<ToolResult>,
}
"@
$new1 = @"
struct PendingCall {
    session_id: String,
    run_id: Option<String>,
    tool_name: String,
    tool_call_id: Option<String>,
    tx: oneshot::Sender<ToolResult>,
}
"@
$content = $content -replace [regex]::escape($old1), $new1

# 2. Update hold_tool_call
$old2 = @"
    pub async fn hold_tool_call(&self, mcp_id: String, session_id: String, timeout_secs: u64) -> Result<ToolResult> {
        let (tx, rx) = oneshot::channel();
        self.pending_calls.lock().await.insert(
            mcp_id.clone(),
            PendingCall { session_id, tx },
        );

        info!(event = "bridge.tool_call.hold", mcp_id = %mcp_id, timeout = timeout_secs);

        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
"@
$new2 = @"
    pub async fn hold_tool_call(&self, mcp_id: String, session_id: String, tool_name: String, timeout_secs: u64) -> Result<ToolResult> {
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
                else => {
                    pending_clone.lock().await.remove(&mcp_id_clone);
                    info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id);
                    return Err(anyhow::anyhow!("timeout menunggu hasil dari client"));
                }
            }
        }
    }
"@
$content = $content -replace [regex]::escape($old2), $new2

# 3. Update resolve_tool_call and add new methods
$old3 = @"
    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> bool {
        if let Some(pending) = self.pending_calls.lock().await.remove(mcp_id) {
            let _ = pending.tx.send(result);
            true
        } else {
            false
        }
    }
"@
$new3 = @"
    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> Option<String> {
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
"@
$content = $content -replace [regex]::escape($old3), $new3

# 4. Update execute_tool PendingCall insert
$old4 = @"
        let (tx, rx) = oneshot::channel();
        self.pending_calls
            .lock()
            .await
            .insert(mcp_id.clone(), PendingCall { session_id: String::new(), tx });
"@
$new4 = @"
        let (tx, rx) = oneshot::channel();
        let run_id_opt = None;
        self.pending_calls
            .lock()
            .await
            .insert(mcp_id.clone(), PendingCall { session_id: String::new(), run_id: run_id_opt, tool_name: tool_name.to_string(), tool_call_id: None, tx });
"@
$content = $content -replace [regex]::escape($old4), $new4

[System.IO.File]::WriteAllText($path, $content)
Write-Host "Done"