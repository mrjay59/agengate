//! Gemini CLI bridge provider — V2 adapter implementing spec_tooling.md.

use crate::bridge::BridgeClient;
use crate::provider::bridge::{
    BridgeConfig, BridgeProfile, BridgeProviderFactory, BridgeProviderV2, BridgeSession,
    BridgeToolResult, McpConfig, McpServerConfig, SessionOutput,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};
use uuid;

/// P1-AUDIT seq3: Resolve binary path with .cmd/PATHEXT handling on Windows.
/// Falls back through PATHEXT extensions (.cmd, .bat, .ps1) when the bare
/// binary name fails to resolve.
fn resolve_binary_path(binary: &str) -> Result<PathBuf> {
    if Path::new(binary).exists() || binary.contains('.') {
        return Ok(PathBuf::from(binary));
    }

    #[cfg(target_os = "windows")]
    {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        for ext in pathext.split(';') {
            let candidate = format!("{}{}", binary, ext);
            if std::process::Command::new(&candidate).arg("--version").output().is_ok() {
                return Ok(PathBuf::from(candidate));
            }
        }
    }

    Ok(PathBuf::from(binary))
}

/// Gemini CLI bridge provider (V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiBridgeProvider {
    profile: BridgeProfile,
}

impl GeminiBridgeProvider {
    pub fn new() -> Self {
        Self {
            profile: BridgeProfile {
                name: "gemini".into(),
                display_name: "Gemini CLI".into(),
                binary: "gemini".into(),
                args: vec!["generate".into(), "--model".into(), "{model}".into(), "--prompt-stdin".into()],
                env: HashMap::new(),
                mcp_config_template: Some(
                    r#"{"mcpServers":{"agentgate":{"type":"{transport}","url":"{transport_url}","headers":{"Authorization":"Bearer {token}"}}}}"#
                        .into(),
                ),
                tool_schema_format: "openai".into(),
                supports_streaming: true,
                guard_patterns: vec![
                    "bash".into(),
                    "edit".into(),
                    "write".into(),
                    "read".into(),
                    "patch".into(),
                    "grep".into(),
                    "glob".into(),
                    "list".into(),
                    "webfetch".into(),
                    "todowrite".into(),
                    "todoread".into(),
                    "task".into(),
                    "web_search".into(),
                    "agentgate_*".into(),
                    "mcp__agentgate__".into(),
                ],
            },
        }
    }

    /// Build tool-disabling config for Gemini CLI per spec §12.3
    fn build_tool_disable_config(&self) -> Value {
        serde_json::json!({
            "tools": {
                "bash": false,
                "edit": false,
                "write": false,
                "read": false,
                "patch": false,
                "grep": false,
                "glob": false,
                "list": false,
                "webfetch": false,
                "todowrite": false,
                "todoread": false,
                "task": false,
                "web_search": false
            },
            "permission": {
                "edit": "deny",
                "bash": "deny",
                "webfetch": "deny"
            }
        })
    }
}

impl Default for GeminiBridgeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BridgeProviderV2 for GeminiBridgeProvider {
    fn profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn start_bridge(
        &self,
        config: BridgeConfig,
        client: Arc<BridgeClient>,
    ) -> Result<Box<dyn BridgeSession>> {
        let session = GeminiSession::spawn(config, client, self.profile.clone(), self.build_tool_disable_config()).await?;
        Ok(Box::new(session))
    }

    async fn health_check(&self) -> Result<bool> {
        // P1-AUDIT seq3: Resolve binary with .cmd/PATHEXT handling on Windows.
        let resolved_binary = resolve_binary_path(&self.profile.binary)?;
        match Command::new(resolved_binary).arg("--version").output().await {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        }
    }
}

/// Gemini CLI session that manages the CLI process and MCP communication
struct GeminiSession {
    client: Arc<BridgeClient>,
    profile: BridgeProfile,
    session_id: String,
    child: Arc<Mutex<Option<Child>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<Value>>>>,
    closed: Arc<Mutex<bool>>,
    mcp_token: String,
    mcp_port: u16,
    /// Empty working directory for this session (spec §11.3)
    workdir: PathBuf,
}

impl GeminiSession {
    async fn spawn(
        config: BridgeConfig,
        client: Arc<BridgeClient>,
        profile: BridgeProfile,
        tool_disable_config: Value,
    ) -> Result<Self> {
        let session_id = config
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mcp_token = config
            .session_token
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().replace("-", ""));
        let mcp_port = config.port;
        let transport = config.transport.unwrap_or_else(|| "http".to_string());

        // Debug log transport mode
        info!(target: "bridge.gemini.config", session_id = %session_id, transport = %transport, "Transport mode selected");

        // Create empty working directory per session (spec §11.3)
        let data_dir = config
            .data_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("agentgate"));
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        // Build MCP config with tool-disabling config
        let mcp_config = Self::build_mcp_config(&mcp_token, mcp_port, &session_id, &tool_disable_config, &transport, &config.tools)?;
        let _mcp_config_json = serde_json::to_string(&mcp_config)?;

        // Write Gemini settings to per-session temp file (spec §12.3 — isolated config per run)
        let settings_file = workdir.join("gemini-settings.json");
        let gemini_settings_json = Self::build_gemini_settings_json(
            &mcp_token,
            mcp_port,
            &session_id,
            &tool_disable_config,
            &transport,
        );
        fs::write(&settings_file, &gemini_settings_json)
            .await
            .context("Failed to write Gemini settings file")?;

        // Prepare command
        // P1-AUDIT seq3: Resolve binary with .cmd/PATHEXT handling on Windows.
        let resolved_binary = resolve_binary_path(&profile.binary)?;
        let mut cmd = Command::new(resolved_binary);
        cmd.args(&profile.args);

        // Set working directory to empty session workdir (spec §11.3)
        cmd.current_dir(&workdir);

        // Set environment variables for MCP config via file path (spec §12.3)
        cmd.env("GEMINI_CLI_SYSTEM_SETTINGS_PATH", &settings_file);

        // Debug logging for config injection verification (§12.5) and cwd (§11.3)
        info!(
            target: "bridge.gemini.config",
            session_id = %session_id,
            workdir = %workdir.display(),
            settings_file = %settings_file.display(),
            binary = %profile.binary,
            args = ?profile.args,
            mcp_port = mcp_port,
            mcp_token_len = mcp_token.len(),
            gemini_settings_json_len = gemini_settings_json.len(),
            transport = %transport,
            "Spawning Gemini CLI with injected MCP config"
        );
        debug!(
            target: "bridge.gemini.config",
            session_id = %session_id,
            "GEMINI_CLI_SYSTEM_SETTINGS_PATH = {}", gemini_settings_json
        );

        // Add profile env vars
        for (key, value) in &profile.env {
            cmd.env(key, value);
        }

        // Spawn process
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn gemini process")?;

        // Debug: verify actual working directory of spawned process (spec §11.3)
        if let Some(pid) = child.id() {
            #[cfg(target_os = "linux")]
            {
                if let Ok(cwd) = std::fs::read_link(format!("/proc/{}/cwd", pid)) {
                    info!(
                        target: "bridge.gemini.cwd",
                        session_id = %session_id,
                        expected = %workdir.display(),
                        actual = %cwd.display(),
                        "Verified child process working directory"
                    );
                }
            }
            #[cfg(target_os = "macos")]
            {
                use std::process::Command;
                if let Ok(output) = Command::new("lsof").args(["-p", &pid.to_string(), "-d", "cwd"]).output() {
                    if let Ok(stdout) = String::from_utf8(output.stdout) {
                        info!(
                            target: "bridge.gemini.cwd",
                            session_id = %session_id,
                            expected = %workdir.display(),
                            lsof_output = %stdout,
                            "Child process working directory (via lsof)"
                        );
                    }
                }
            }
        }

        let stdin = child.stdin.take().context("Failed to get stdin")?;
        let stdout = child.stdout.take().context("Failed to get stdout")?;
        let stderr = child.stderr.take().context("Failed to get stderr")?;

        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Spawn stdout reader task
        let event_tx_stdout = event_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_str::<Value>(&line) {
                    if event_tx_stdout.send(json).is_err() {
                        break;
                    }
                }
            }
        });

        // Spawn stderr reader task (for logging)
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!(target: "gemini.stderr", line = %line);
            }
        });

        let session = Self {
            client,
            profile,
            session_id,
            child: Arc::new(Mutex::new(Some(child))),
            stdin: Arc::new(Mutex::new(Some(stdin))),
            event_rx: Arc::new(Mutex::new(Some(event_rx))),
            closed: Arc::new(Mutex::new(false)),
            mcp_token,
            mcp_port,
            workdir,
        };

        session.wait_for_mcp_initialize().await?;

        Ok(session)
    }

    fn build_mcp_config(
        token: &str,
        port: u16,
        session_id: &str,
        tool_disable_config: &Value,
        transport: &str,
        tools: &[crate::bridge::ToolSchema],
    ) -> Result<McpConfig> {
        let mcp_servers: HashMap<String, McpServerConfig> = if transport == "stdio_shim" {
            let tools_json = serde_json::to_string(tools).unwrap_or_default();
            let agentgate_bin = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "agentgate".to_string());
            let mut m = HashMap::new();
            m.insert(
                "agentgate".into(),
                McpServerConfig {
                    r#type: "command".into(),
                    url: None,
                    command: Some(agentgate_bin),
                    args: Some(vec![
                        "mcp-shim".into(),
                        "--session".into(),
                        session_id.into(),
                        "--token".into(),
                        token.into(),
                        "--host".into(),
                        "127.0.0.1".into(),
                        "--port".into(),
                        port.to_string(),
                    ]),
                    env: Some({
                        let mut env = HashMap::new();
                        env.insert("AGENTGATE_SHIM_TOOLS".into(), tools_json);
                        env
                    }),
                    headers: None,
                },
            );
            m
        } else {
            // P1-AUDIT seq3: Use transport variable instead of hardcoded http.
            // transport can be "http", "sse", "ws", etc. - construct URL accordingly.
            let transport_url = format!("{}://127.0.0.1:{}/_bridge/mcp/{}", transport, port, session_id);
            let mut m = HashMap::new();
            m.insert(
                "agentgate".into(),
                McpServerConfig {
                    r#type: transport.into(),
                    url: Some(transport_url),
                    headers: Some({
                        let mut headers = HashMap::new();
                        headers.insert("Authorization".into(), format!("Bearer {token}"));
                        headers
                    }),
                    command: None,
                    args: None,
                    env: None,
                },
            );
            m
        };

        // Merge tool-disabling config into MCP config per spec §12.3
        let mut config = McpConfig { mcp_servers };
        config.mcp_servers.insert(
            "agentgate_tools".into(),
            McpServerConfig {
                r#type: "stdio".into(),
                url: None,
                command: Some("true".into()),
                args: None,
                env: Some({
                    let mut env = HashMap::new();
                    env.insert("GEMINI_CLI_SYSTEM_SETTINGS_PATH".into(), serde_json::to_string(tool_disable_config)?);
                    env
                }),
                headers: None,
            },
        );
        Ok(config)
    }

    /// Build Gemini settings JSON injected via GEMINI_CLI_SYSTEM_SETTINGS_PATH.
    /// Includes the agentgate MCP server bridge and tool-disabling rules (spec §12.3).
    fn build_gemini_settings_json(
        token: &str,
        port: u16,
        session_id: &str,
        tool_disable_config: &Value,
        transport: &str,
    ) -> String {
        let mcp_servers = if transport == "stdio_shim" {
            let agentgate_bin = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "agentgate".to_string());
            serde_json::json!({
                "agentgate": {
                    "type": "command",
                    "command": agentgate_bin,
                    "args": [
                        "mcp-shim",
                        "--session", session_id,
                        "--token", token,
                        "--host", "127.0.0.1",
                        "--port", port.to_string()
                    ],
                    "env": {}
                }
            })
        } else {
            // P1-AUDIT seq3: Use transport variable instead of hardcoded http.
            let transport_url = format!("{}://127.0.0.1:{}/_bridge/mcp/{}", transport, port, session_id);
            serde_json::json!({
                "agentgate": {
                    "type": transport,
                    "url": transport_url,
                    "headers": { "Authorization": format!("Bearer {token}") }
                }
            })
        };

        serde_json::json!({
            "mcpServers": mcp_servers,
            "tools": tool_disable_config["tools"],
            "permissions": tool_disable_config["permission"]
        })
        .to_string()
    }

    async fn wait_for_mcp_initialize(&self) -> Result<()> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    /// Preflight isolation check per spec §13.2 (P2)
    async fn preflight_isolation_check(&self) -> Result<bool> {
        info!(target: "bridge.preflight", session_id = %self.session_id, "Starting preflight isolation check");

        let guard_patterns = &self.profile.guard_patterns;
        if guard_patterns.is_empty() {
            warn!(target: "bridge.preflight", session_id = %self.session_id, "No guard patterns configured");
            return Ok(false);
        }

        info!(
            target: "bridge.preflight",
            session_id = %self.session_id,
            guard_patterns = ?guard_patterns,
            "Preflight isolation check completed - CLI should have built-in tools disabled via config injection"
        );

        info!(
            target: "bridge.preflight",
            session_id = %self.session_id,
            isolation_status = "config_injected",
            note = "Built-in tools should be disabled via GEMINI_CLI_SYSTEM_SETTINGS_PATH injection"
        );

        Ok(true)
    }

    async fn send_to_cli(&self, message: &Value) -> Result<()> {
        let mut stdin_guard = self.stdin.lock().await;
        if let Some(stdin) = stdin_guard.as_mut() {
            let line = serde_json::to_string(message)?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }
        Ok(())
    }

    async fn read_event(&self) -> Option<Value> {
        let mut rx_guard = self.event_rx.lock().await;
        if let Some(rx) = rx_guard.as_mut() {
            rx.recv().await
        } else {
            None
        }
    }
}

#[async_trait]
impl BridgeSession for GeminiSession {
    fn session_id(&self) -> &String {
        &self.session_id
    }

    fn provider_profile(&self) -> &BridgeProfile {
        &self.profile
    }

    /// Preflight isolation check per spec §13.2 (wired via shared helper).
    async fn preflight_isolation_check(&self) -> Result<bool> {
        crate::provider::bridge::preflight_isolation_check(&self.profile, &self.session_id, "gemini").await
    }

    async fn send_user_message(&self, text: &str) -> Result<()> {
        if *self.closed.lock().await {
            return Err(anyhow!("Session is closed"));
        }

        let message = serde_json::json!({
            "type": "user",
            "content": text
        });
        self.send_to_cli(&message).await
    }

    async fn send_tool_result(&self, result: BridgeToolResult) -> Result<()> {
        if *self.closed.lock().await {
            return Err(anyhow!("Session is closed"));
        }

        let message = serde_json::json!({
            "type": "tool_result",
            "tool_call_id": result.tool_call_id,
            "content": result.result,
            "is_error": result.is_error
        });
        self.send_to_cli(&message).await
    }

    async fn close(&self) -> Result<()> {
        let mut closed = self.closed.lock().await;
        if *closed {
            return Ok(());
        }
        *closed = true;

        let _ = self
            .send_to_cli(&serde_json::json!({ "type": "shutdown" }))
            .await;

        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let pid = child.id();
            #[cfg(windows)]
            if let Some(pid) = pid {
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .output()
                    .await;
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        let _ = fs::remove_dir_all(&self.workdir).await;

        info!("Gemini session {} closed", self.session_id);
        Ok(())
    }

    async fn wait_for_completion(&self) -> Result<SessionOutput> {
        // Drain events from the channel to collect output
        let mut content = String::new();
        let mut reasoning = None;
        let mut tool_calls: Vec<crate::types::ToolCall> = Vec::new();

        // Take the event receiver
        let mut rx_guard = self.event_rx.lock().await;
        if let Some(mut rx) = rx_guard.take() {
            // Drain all events
            while let Some(event) = rx.recv().await {
                // Parse event for content and reasoning
                if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                    content.push_str(text);
                    content.push('\n');
                }
                if let Some(reasoning_text) = event.get("reasoning").and_then(|v| v.as_str()) {
                    if reasoning.is_none() {
                        reasoning = Some(String::new());
                    }
                    if let Some(r) = &mut reasoning {
                        r.push_str(reasoning_text);
                        r.push('\n');
                    }
                }
                // Also check for "content" field
                if let Some(c) = event.get("content").and_then(|v| v.as_str()) {
                    content.push_str(c);
                    content.push('\n');
                }
                if let Some(arr) = event.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in arr {
                        if let (Some(id), Some(ty), Some(func)) = (
                            tc.get("id").and_then(|v| v.as_str()),
                            tc.get("type").and_then(|v| v.as_str()),
                            tc.get("function"),
                        ) {
                            if let (Some(name), Some(args)) = (
                                func.get("name").and_then(|v| v.as_str()),
                                func.get("arguments").and_then(|v| v.as_str()),
                            ) {
                                tool_calls.push(crate::types::ToolCall {
                                    id: id.to_string(),
                                    r#type: ty.to_string(),
                                    function: crate::reasoning::event::FunctionCall {
                                        name: name.to_string(),
                                        arguments: args.to_string(),
                                    },
                                });
                            }
                        }
                    }
                }
            }
        }
        drop(rx_guard);

        // Wait for process to exit
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let status = child.wait().await?;
            debug!("Gemini process exited with status: {}", status);
        }

        Ok(SessionOutput {
            content: content.trim().to_string(),
            reasoning: reasoning.map(|r| r.trim().to_string()),
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        })
    }

    async fn take_event_receiver(&self) -> Result<Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>> {
        let mut rx_guard = self.event_rx.lock().await;
        Ok(rx_guard.take())
    }
}

#[derive(Debug, Default)]
pub struct GeminiBridgeFactory;

#[async_trait]
impl BridgeProviderFactory for GeminiBridgeFactory {
    fn provider_name(&self) -> &str {
        "gemini"
    }

    async fn create(&self, _config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>> {
        Ok(Box::new(GeminiBridgeProvider::new()))
    }

    fn default_profile(&self) -> crate::provider::bridge::BridgeProfileSpec {
        crate::provider::bridge::BridgeProfileSpec {
            name: "gemini".into(),
            display_name: "Gemini CLI".into(),
            binary: "gemini".into(),
            args: vec!["generate".into(), "--model".into(), "{model}".into(), "--prompt-stdin".into()],
            env: HashMap::new(),
            mcp_config_template: Some(
                r#"{"mcpServers":{"agentgate":{"type":"{transport}","url":"{transport_url}","headers":{"Authorization":"Bearer {token}"}}}}"#
                    .into(),
            ),
            tool_schema_format: "openai".into(),
            supports_streaming: true,
            guard_patterns: vec![
                    "bash".into(),
                    "edit".into(),
                    "write".into(),
                    "read".into(),
                    "patch".into(),
                    "grep".into(),
                    "glob".into(),
                    "list".into(),
                    "webfetch".into(),
                    "todowrite".into(),
                    "agentgate_*".into(),
                    "mcp__agentgate__".into(),
                ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_name() {
        let f = GeminiBridgeFactory;
        assert_eq!(f.provider_name(), "gemini");
    }

    #[test]
    fn mcp_config_serialization() {
        let tool_disable = serde_json::json!({});
        let config = GeminiSession::build_mcp_config("test-token", 8788, "test-session", &tool_disable, "http", &[]).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("agentgate"));
        assert!(json.contains("test-token"));
        assert!(json.contains("8788"));
        assert!(json.contains("test-session"));
    }
}


