with open('src/api.rs', 'r') as f:
    content = f.read()

# 1. Update bridge_mcp: tool name normalization + preflight isolation check + is_bridge_tool check
old = '''            // Fail-closed (spec §13.1): tool harus ada di whitelist session ini.
            // Bila tidak -> 503 `tool_isolation_unavailable` (tidak pernah hold/execute).
            if !session.tools.iter().any(|t| t.name == name) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name);
                crate::metrics::METRICS.record_guard_violation();
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("tool_isolation_unavailable: {}", name),
                            "type": "invalid_request_error",
                            "code": "tool_isolation_unavailable",
                        }
                    })),
                ).into_response();
            }
            let mcp_id = id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            info!(event = "bridge.call_held", mcp_req_id = %mcp_id, session = %session.session_id, tool = %name);
            let hold_start = Instant::now();
            match registry.bridge.hold_tool_call(mcp_id.clone(), session.session_id.clone(), name.clone(), registry.bridge_config.call_ttl_secs).await {'''
new = '''            // Fail-closed (spec §13.1): tool harus ada di whitelist session ini.
            // Bila tidak -> 503 `tool_isolation_unavailable` (tidak pernah hold/execute).
            let normalized_name = crate::bridge::normalize_tool_name(&name, &session.tools);
            if !session.tools.iter().any(|t| t.name == normalized_name) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name);
                crate::metrics::METRICS.record_guard_violation();
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("tool_isolation_unavailable: {}", name),
                            "type": "invalid_request_error",
                            "code": "tool_isolation_unavailable",
                        }
                    })),
                ).into_response();
            }

            // Fail-closed: verify isolation/builtin disable is active for bridge providers.
            // Bila isolation tidak aktif -> 503, jangan hold/execute path yang tidak aman.
            if !session.preflight_isolation_check() {
                info!(event = "guard.isolation_unavailable", session = %session.session_id, tool = %normalized_name);
                crate::metrics::METRICS.record_guard_violation();
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": {
                            "message": "tool_isolation_unavailable: isolation guard not active for this provider",
                            "type": "invalid_request_error",
                            "code": "tool_isolation_unavailable",
                        }
                    })),
                ).into_response();
            }

            // Fail-closed: reject non-bridge tool names for bridge providers.
            if !session.is_bridge_tool(&normalized_name) {
                info!(event = "guard.non_bridge_tool", session = %session.session_id, observed_tool = %normalized_name);
                crate::metrics::METRICS.record_guard_violation();
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("tool_isolation_unavailable: non-bridge tool {}", normalized_name),
                            "type": "invalid_request_error",
                            "code": "tool_isolation_unavailable",
                        }
                    })),
                ).into_response();
            }

            let mcp_id = id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            info!(event = "bridge.call_held", mcp_req_id = %mcp_id, session = %session.session_id, tool = %normalized_name);
            let hold_start = Instant::now();
            match registry.bridge.hold_tool_call(mcp_id.clone(), session.session_id.clone(), normalized_name.clone(), registry.bridge_config.call_ttl_secs).await {'''
content = content.replace(old, new)

# 2. Update Err handling in bridge_mcp for PermissionDenied
old = '''            Err(e) => serde_json::json!({
                    "content": [{ "type": "text", "text": e.to_string() }],
                    "isError": true
                }),'''
new = '''            Err(e) => {
                // Handle Windows os error 5 / PermissionDenied with clear JSON instead of raw crash.
                let e_str = e.to_string();
                let is_permission_denied = e_str.contains("PermissionDenied")
                    || e_str.contains("os error 5")
                    || e_str.contains("access denied");
                let error_msg = if is_permission_denied {
                    format!("Read-folder access denied: {}", e_str)
                } else {
                    e_str
                };
                serde_json::json!({
                    "content": [{ "type": "text", "text": error_msg }],
                    "isError": true
                })
            }'''
content = content.replace(old, new)

with open('src/api.rs', 'w') as f:
    f.write(content)
print('api.rs done')