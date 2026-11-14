with open('src/bridge.rs', 'r') as f:
    content = f.read()

# Fix hold_tool_call body - exact match with space after );
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
    print("hold_tool_call old NOT found - trying alternative")
    # Try without the trailing space
    old_alt = old.replace('        );\n        \n', '        );\n\n')
    if old_alt in content:
        content = content.replace(old_alt, new)
        print("hold_tool_call body replaced (alt)")
    else:
        print("alt also not found")
        # Print exact bytes around the area
        idx = content.find('let (tx, mut rx) = oneshot::channel();')
        if idx >= 0:
            for i, ch in enumerate(content[idx:idx+700]):
                if ord(ch) > 127 or ch in '\r\t':
                    print(f"  pos {idx+i}: {repr(ch)}")

with open('src/bridge.rs', 'w') as f:
    f.write(content)

print("Done")