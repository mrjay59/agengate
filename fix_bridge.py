with open('src/bridge.rs', 'r') as f:
    content = f.read()

# Fix hold_tool_call body
old = '''let (tx, mut rx) = oneshot::channel();
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

new = '''let (tx, mut rx) = oneshot::channel();
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
    }'''

if old in content:
    content = content.replace(old, new)
    print("hold_tool_call body replaced")
else:
    print("hold_tool_call old NOT found")
    # Debug
    idx = content.find('hold_tool_call')
    if idx >= 0:
        print(repr(content[idx:idx+1000]))

# Fix resolve_tool_call
old2 = '''    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> bool {
        if let Some(pending) = self.pending_calls.lock().await.remove(mcp_id) {
            let _ = pending.tx.send(result);
            true
        } else {
            false
        }
    }'''

new2 = '''    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> Option<String> {
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
    }'''

if old2 in content:
    content = content.replace(old2, new2)
    print("resolve_tool_call replaced")
else:
    print("resolve_tool_call old NOT found")
    idx = content.find('resolve_tool_call')
    if idx >= 0:
        print(repr(content[idx:idx+500]))

with open('src/bridge.rs', 'w') as f:
    f.write(content)

print("Done")