// Stdio shim transport — fallback MCP server for CLI agents with HTTP issues
// per spec_tooling.md §9.7

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use crate::bridge::{BridgeClient, BridgeServer};
use crate::provider::bridge::BridgeConfig;

/// Stdio shim configuration
#[derive(Debug, Clone)]
pub struct ShimConfig {
    pub session_id: String,
    pub session_token: String,
    pub bridge_host: String,
    pub bridge_port: u16,
    pub tools: Vec<crate::bridge::ToolSchema>,
}

/// JSON-RPC 2.0 request/response structures
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl TryFrom<Value> for JsonRpcError {
    type Error = serde_json::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let code = value.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603) as i32;
        let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("Bridge error").to_string();
        let data = value.get("data").cloned();
        Ok(JsonRpcError { code, message, data })
    }
}

/// MCP method names
pub mod mcp_methods {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "notifications/initialized";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
    pub const RESOURCES_LIST: &str = "resources/list";
    pub const RESOURCES_READ: &str = "resources/read";
    pub const PROMPTS_LIST: &str = "prompts/list";
    pub const PROMPTS_GET: &str = "prompts/get";
}

/// Run the stdio shim as a subprocess.
/// This is meant to be spawned by the CLI agent as an MCP server.
pub async fn run_stdio_shim(config: ShimConfig) -> Result<()> {
    info!("Starting stdio shim for session {}", config.session_id);

    let bridge_host = config.bridge_host.clone();
    let bridge_port = config.bridge_port;
    let bridge_client = Arc::new(BridgeClient::new(bridge_host, bridge_port));

    // Set up stdin/stdout pipes
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    // Track initialization state
    let mut initialized = false;

    // FIX-SHIM-LEAK: opencode in `serve --service` mode never closes the MCP
    // subprocess stdin, so relying on EOF alone leaks one `agentgate mcp-shim`
    // process per session. Bound the lifetime with an idle timeout. The default
    // must exceed bridge `call_ttl_secs` (120s) so a held tools/call can still
    // be resolved by the client.
    let idle_secs = std::env::var("AGENTGATE_SHIM_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300);
    let idle_timeout = std::time::Duration::from_secs(idle_secs);

    // Hard ceiling independent of message activity. opencode's MCP client can
    // keep sending periodic messages (ping/tools-list), which resets the idle
    // timer forever. A shim only serves one turn, so a finite lifetime is safe.
    let max_lifetime_secs = std::env::var("AGENTGATE_SHIM_MAX_LIFETIME_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300);
    info!(
        "Stdio shim lifetime limits: idle={}s max={}s for session {}",
        idle_secs, max_lifetime_secs, config.session_id
    );

    // Hard exit timer: the main loop can be blocked inside a held tools/call
    // for `call_ttl_secs` (120s), so a cooperative watch flag is not enough.
    // This process-level timer guarantees the shim never outlives its turn.
    {
        let session_id = config.session_id.clone();
        let secs = max_lifetime_secs;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            info!("Stdio shim hard lifetime reached, exiting {}", session_id);
            std::process::exit(0);
        });
    }

    // Watchdog: exit when the owning bridge session is gone (the normal case
    // after a turn ends) or when the hard lifetime is reached.
    let (exit_tx, mut exit_rx) = tokio::sync::watch::channel(false);
    {
        let session_id = config.session_id.clone();
        let token = config.session_token.clone();
        let host = config.bridge_host.clone();
        let port = config.bridge_port;
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let url = format!("http://{}:{}/_bridge/preflight/{}", host, port, session_id);
            let client = reqwest::Client::new();
            let mut missing = 0u8;
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if started.elapsed() >= std::time::Duration::from_secs(max_lifetime_secs) {
                    info!("Stdio shim watchdog: max lifetime reached for session {}", session_id);
                    let _ = exit_tx.send(true);
                    return;
                }
                match client.get(&url).bearer_auth(&token).send().await {
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                        missing += 1;
                        if missing >= 2 {
                            info!("Stdio shim watchdog: session gone, exiting {}", session_id);
                            let _ = exit_tx.send(true);
                            return;
                        }
                    }
                    Ok(_) => missing = 0,
                    Err(_) => {} // transient; keep trying
                }
            }
        });
    }

    loop {
        line.clear();
        let bytes_read = tokio::select! {
            n = reader.read_line(&mut line) => n?,
            _ = tokio::time::sleep(idle_timeout) => {
                info!(
                    "Stdio shim exiting (idle {}s) for session {}",
                    idle_secs, config.session_id
                );
                break;
            }
            _ = exit_rx.changed() => {
                if *exit_rx.borrow() {
                    info!("Stdio shim exiting (watchdog) for session {}", config.session_id);
                    break;
                }
                continue;
            }
        };
        if bytes_read == 0 {
            // EOF - client disconnected
            info!("Stdio shim: client disconnected (EOF)");
            break;
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse JSON-RPC message
        let msg: JsonRpcMessage = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                warn!("Stdio shim: failed to parse JSON-RPC: {}", e);
                let error_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: Value::Null,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: "Parse error".to_string(),
                        data: None,
                    }),
                };
                write_response(&mut stdout, &error_resp).await?;
                continue;
            }
        };

        match msg {
            JsonRpcMessage::Request(req) => {
                let response = handle_request(req, &bridge_client, &config, &mut initialized).await;
                write_response(&mut stdout, &response).await?;
            }
            JsonRpcMessage::Notification(notif) => {
                handle_notification(notif, &mut initialized).await;
            }
            JsonRpcMessage::Response(_) => {
                // Responses from client - ignore
            }
        }
    }

    info!("Stdio shim exiting for session {}", config.session_id);
    Ok(())
}

/// Handle a JSON-RPC request
async fn handle_request(
    req: JsonRpcRequest,
    bridge_client: &Arc<BridgeClient>,
    config: &ShimConfig,
    initialized: &mut bool,
) -> JsonRpcResponse {
    match req.method.as_str() {
        mcp_methods::INITIALIZE => handle_initialize(req, config).await,
        mcp_methods::TOOLS_LIST => handle_tools_list(req, config).await,
        mcp_methods::TOOLS_CALL => handle_tools_call(req, bridge_client, config).await,
        mcp_methods::RESOURCES_LIST => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: Some(json!({ "resources": [] })),
            error: None,
        },
        mcp_methods::RESOURCES_READ => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: Some(json!({ "contents": [] })),
            error: None,
        },
        mcp_methods::PROMPTS_LIST => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: Some(json!({ "prompts": [] })),
            error: None,
        },
        mcp_methods::PROMPTS_GET => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: Some(json!({ "messages": [] })),
            error: None,
        },
        _ => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", req.method),
                data: None,
            }),
        },
    }
}

/// Handle a JSON-RPC notification
async fn handle_notification(notif: JsonRpcNotification, initialized: &mut bool) {
    if notif.method == mcp_methods::INITIALIZED {
        *initialized = true;
        info!("Stdio shim: client initialized");
    }
}

/// Handle initialize request
async fn handle_initialize(req: JsonRpcRequest, config: &ShimConfig) -> JsonRpcResponse {
    let result = json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {},
            "resources": {},
            "prompts": {}
        },
        "serverInfo": {
            "name": "agentgate-stdio-shim",
            "version": "0.1.0"
        }
    });

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: req.id,
        result: Some(result),
        error: None,
    }
}

/// Handle tools/list request
async fn handle_tools_list(req: JsonRpcRequest, config: &ShimConfig) -> JsonRpcResponse {
    let tools: Vec<Value> = config
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.parameters
            })
        })
        .collect();

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: req.id,
        result: Some(json!({ "tools": tools })),
        error: None,
    }
}

/// Handle tools/call request - forward to bridge server via HTTP
async fn handle_tools_call(
    req: JsonRpcRequest,
    bridge_client: &Arc<BridgeClient>,
    config: &ShimConfig,
) -> JsonRpcResponse {
    let params = match req.params {
        Some(p) => p,
        None => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Missing params".to_string(),
                    data: None,
                }),
            };
        }
    };

    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Missing tool name".to_string(),
                    data: None,
                }),
            };
        }
    };

    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    debug!(
        "Stdio shim: forwarding tools/call {} to bridge",
        tool_name
    );

    // Forward to bridge server via HTTP
    // The bridge API route is POST /_bridge/mcp/:session_id with JSON-RPC body
    let url = format!(
        "http://{}:{}/_bridge/mcp/{}",
        config.bridge_host, config.bridge_port, config.session_id
    );

    let http_client = reqwest::Client::new();
    let rpc_body = json!({
        "jsonrpc": "2.0",
        "id": req.id,
        "method": "tools/call",
        "params": {
            "name": tool_name,
            "arguments": arguments
        }
    });

    match http_client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.session_token))
        .header("Content-Type", "application/json")
        .json(&rpc_body)
        .send()
        .await
    {
        Ok(response) => {
            if response.status().is_success() {
                match response.json::<Value>().await {
                    Ok(result) => {
                        // Bridge returns JSON-RPC response; unwrap result if present
                        if let Some(r) = result.get("result") {
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: req.id,
                                result: Some(r.clone()),
                                error: None,
                            }
                        } else if let Some(err) = result.get("error") {
                            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603) as i32;
                            let message = err.get("message").and_then(|v| v.as_str()).unwrap_or("Bridge error").to_string();
                            let data = err.get("data").cloned();
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: req.id,
                                result: None,
                                error: Some(JsonRpcError {
                                    code,
                                    message,
                                    data,
                                }),
                            }
                        } else {
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: req.id,
                                result: Some(result),
                                error: None,
                            }
                        }
                    }
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32603,
                            message: format!("Invalid response from bridge: {}", e),
                            data: None,
                        }),
                    },
                }
            } else {
                let status = response.status().as_u16();
                let error_text = response.text().await.unwrap_or_default();
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32603,
                        message: format!("Bridge error ({}): {}", status, error_text),
                        data: None,
                    }),
                }
            }
        }
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32603,
                message: format!("Failed to connect to bridge: {}", e),
                data: None,
            }),
        },
    }
}

/// Write a JSON-RPC response to stdout
async fn write_response<W: AsyncWriteExt + Unpin>(writer: &mut W, response: &JsonRpcResponse) -> Result<()> {
    let json = serde_json::to_string(response)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Helper to spawn the stdio shim as a child process for a CLI agent
/// This is used when `bridge.transport = "stdio_shim"` in provider config
pub async fn spawn_stdio_shim(
    session_id: String,
    session_token: String,
    bridge_host: String,
    bridge_port: u16,
    tools: Vec<crate::bridge::ToolSchema>,
) -> Result<Child> {
    let current_exe = std::env::current_exe()?;
    
    // We use the same binary with a special subcommand
    let mut cmd = Command::new(current_exe);
    cmd.arg("mcp-shim")
        .arg("--session")
        .arg(&session_id)
        .arg("--token")
        .arg(&session_token)
        .arg("--host")
        .arg(&bridge_host)
        .arg("--port")
        .arg(bridge_port.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Pass tools via environment variable (serialized JSON)
    let tools_json = serde_json::to_string(&tools)?;
    cmd.env("AGENTGATE_SHIM_TOOLS", tools_json);

    let child = cmd.spawn().context("Failed to spawn mcp-shim process")?;
    info!("Spawned mcp-shim for session {}", session_id);
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            method: "tools/list".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("tools/list"));
    }

    #[test]
    fn test_json_rpc_response() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: json!(1),
            result: Some(json!({ "tools": [] })),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("tools"));
    }
}



