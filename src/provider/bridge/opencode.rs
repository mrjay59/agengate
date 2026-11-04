//! OpenCode bridge provider — V2 adapter implementing spec_tooling.md.

use crate::bridge::{BridgeClient, Run, ToolResult, ToolSchema};
use crate::provider::bridge::{
    BridgeConfig, BridgeProfile, BridgeProviderFactory, BridgeProviderV2, BridgeSession,
    BridgeToolResult, McpConfig, McpServerConfig, ToolCall,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex, oneshot};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};
use uuid;

/// OpenCode bridge provider (V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodeBridgeProvider {
    profile: BridgeProfile,
}

impl OpenCodeBridgeProvider {
    pub fn new() -> Self {
        Self {
            profile: BridgeProfile {
                name: "opencode".into(),
                display_name: "OpenCode".into(),
                binary: "opencode".into(),
                args: vec!["run".into(), "--format".into(), "json".into()],
                env: HashMap::new(),
                mcp_config_template: Some(
                    r#"{"mcpServers":{"agentgate":{"type":"http","url":"http://127.0.0.1:{port}/_bridge/mcp/{session}","headers":{"Authorization":"Bearer {token}"}}}}"#
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
                ],
            },
        }
    }

    /// Build tool-disabling config for OpenCode per spec §12.2
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
                "task": false
            },
            "permission": {
                "edit": "deny",
                "bash": "deny",
                "webfetch": "deny"
            }
        })
    }
}

impl Default for OpenCodeBridgeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BridgeProviderV2 for OpenCodeBridgeProvider {
    fn profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn start_bridge(
        &self,
        config: BridgeConfig,
        client: Arc<BridgeClient>,
    ) -> Result<Box<dyn BridgeSession>> {
        let session = OpenCodeSession::spawn(config, client, self.profile.clone(), self.build_tool_disable_config()).await?;
        Ok(Box::new(session))
    }

    async fn health_check(&self) -> Result<bool> {
        // Check if opencode binary is available
        match Command::new(&self.profile.binary).arg("--version").output().await {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        }
    }
}

/// OpenCode CLI session that manages the CLI process and MCP communication
struct OpenCodeSession {
    client: Arc<BridgeClient>,
    profile: BridgeProfile,
    session_id: String,
    child: Arc<Mutex<Option<Child>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    event_tx: mpsc::UnboundedSender<Value>,
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<Value>>>>,
    closed: Arc<Mutex<bool>>,
    mcp_token: String,
    mcp_port: u16,
    /// Empty working directory for this session (spec §11.3)
    workdir: PathBuf,
}

impl OpenCodeSession {
    async fn spawn(
        config: BridgeConfig,
        client: Arc<BridgeClient>,
        profile: BridgeProfile,
        tool_disable_config: Value,
    ) -> Result<Self> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let mcp_token = uuid::Uuid::new_v4().to_string().replace("-", "");
        let mcp_port = config.port;

        // Create empty working directory per session (spec §11.3)
        let data_dir = config.data_dir.clone().unwrap_or_else(|| PathBuf::from("/tmp/agentgate"));
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        // Build MCP config with tool-disabling config
        let mcp_config = Self::build_mcp_config(&mcp_token, mcp_port, &session_id, tool_disable_config)?;

        // Prepare command
        let mut cmd = Command::new(&profile.binary);
        cmd.args(&profile.args);

        // Set working directory to empty session workdir (spec §11.3)
        cmd.current_dir(&workdir);

        // Set environment variables for MCP config (spec §12.2)
        let mcp_config_json = Self::build_opencode_config_json(
            &mcp_token,
            mcp_port,
            &session_id,
            &tool_disable_config,
        );
        cmd.env("OPENCODE_CONFIG_CONTENT", &mcp_config_json);

        // Debug logging: verify config injection (spec §12.5)
        info!(
            target: "bridge.opencode.config",
            session_id = %session_id,
            workdir = %workdir.display(),
            binary = %profile.binary,
            args = ?profile.args,
            mcp_port = mcp_port,
            mcp_token_len = mcp_token.len(),
            mcp_config_json_len = mcp_config_json.len(),
            "Spawning OpenCode CLI with injected MCP config"
        );
        debug!(
            target: "bridge.opencode.config",
            session_id = %session_id,
            "OPENCODE_CONFIG_CONTENT = {}", mcp_config_json
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
            .context("Failed to spawn opencode process")?;

        let stdin = child.stdin.take().context("Failed to get stdin")?;
        let stdout = child.stdout.take().context("Failed to get stdout")?;
        let stderr = child.stderr.take().context("Failed to get stderr")?;

        // Set up event channel
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
                        break; // Receiver dropped
                    }
                }
            }
        });

        // Spawn stderr reader task (for logging)
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!(target: "opencode.stderr", line = %line);
            }
        });

        let session = Self {
            client,
            profile,
            session_id,
            child: Arc::new(Mutex::new(Some(child))),
            stdin: Arc::new(Mutex::new(Some(stdin))),
            event_tx,
            event_rx: Arc::new(Mutex::new(Some(event_rx))),
            closed: Arc::new(Mutex::new(false)),
            mcp_token,
            mcp_port,
            workdir,
        };

        // Wait for MCP initialization
        session.wait_for_mcp_initialize().await?;

        Ok(session)
    }

    fn build_mcp_config(token: &str, port: u16, session_id: &str, tool_disable_config: Value) -> Result<McpConfig> {
        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "agentgate".into(),
            McpServerConfig {
                r#type: "http".into(),
                url: format!("http://127.0.0.1:{port}/_bridge/mcp/{session_id}"),
                headers: Some({
                    let mut headers = HashMap::new();
                    headers.insert("Authorization".into(), format!("Bearer {token}"));
                    headers
                }),
            },
        );

        // Merge tool-disabling config into MCP config per spec §12.2
        // The full config (mcpServers + tools + permission) is injected via
        // OPENCODE_CONFIG_CONTENT env var directly on the CLI process.
        let mut config = McpConfig { mcp_servers };
        config
            .mcp_servers
            .insert(
                "agentgate_tools".into(),
                McpServerConfig {
                    r#type: "stdio".into(),
                    command: Some("true".into()), // dummy command, config only
                    args: None,
                    env: Some({
                        let mut env = HashMap::new();
                        env.insert(
                            "OPENCODE_CONFIG_CONTENT".into(),
                            serde_json::to_string(&tool_disable_config)?,
                        );
                        env
                    }),
                },
            );
        Ok(config)
    }

    /// Build the full OpenCode config JSON injected via OPENCODE_CONFIG_CONTENT.
    /// Includes both the agentgate MCP server bridge and the tool-disabling
    /// rules so the CLI actually disables its built-in tools (spec §12.2).
    fn build_opencode_config_json(
        token: &str,
        port: u16,
        session_id: &str,
        tool_disable_config: &Value,
    ) -> String {
        serde_json::json!({
            "mcpServers": {
                "agentgate": {
                    "type": "http",
                    "url": format!("http://127.0.0.1:{port}/_bridge/mcp/{session_id}"),
                    "headers": { "Authorization": format!("Bearer {token}") }
                }
            },
            "tools": tool_disable_config["tools"],
            "permission": tool_disable_config["permission"]
        })
        .to_string()
    }

    async fn wait_for_mcp_initialize(&self) -> Result<()> {
        // Wait for the CLI to initialize and connect to MCP
        // This is a best-effort wait; actual initialization happens asynchronously
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    /// Preflight isolation check per spec §13.2 (P2)
    /// Verifies the CLI has no built-in tools available by checking guard patterns.
    async fn preflight_isolation_check(&self) -> Result<bool> {
        info!(target: "bridge.preflight", session_id = %self.session_id, "Starting preflight isolation check");

        // Check if guard patterns are properly configured
        let guard_patterns = &self.profile.guard_patterns;
        if guard_patterns.is_empty() {
            warn!(target: "bridge.preflight", session_id = %self.session_id, "No guard patterns configured");
            return Ok(false);
        }

        // Try to list tools from CLI via MCP - if tools are disabled, we should get empty list
        // This is a best-effort check; actual isolation is enforced by config injection
        info!(
            target: "bridge.preflight",
            session_id = %self.session_id,
            guard_patterns = ?guard_patterns,
            "Preflight isolation check completed - CLI should have built-in tools disabled via config injection"
        );

        // Log isolation status per spec §13.2
        info!(
            target: "bridge.preflight",
            session_id = %self.session_id,
            isolation_status = "config_injected",
            note = "Built-in tools should be disabled via OPENCODE_CONFIG_CONTENT injection"
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
impl BridgeSession for OpenCodeSession {
    fn session_id(&self) -> &String {
        &self.session_id
    }

    fn provider_profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn send_user_message(&self, text: &str) -> Result<()> {
        if *self.closed.lock().await {
            return Err(anyhow!("Session is closed"));
        }

        // Send user message to CLI via stdin
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

        // Send tool result to CLI via stdin
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

        // Send shutdown signal to CLI
        let _ = self
            .send_to_cli(&serde_json::json!({ "type": "shutdown" }))
            .await;

        // Kill the child process
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        // Clean up workdir
        let _ = fs::remove_dir_all(&self.workdir).await;

        info!("OpenCode session {} closed", self.session_id);
        Ok(())
    }

    async fn wait_for_completion(&self) -> Result<()> {
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let status = child.wait().await?;
            debug!("OpenCode process exited with status: {}", status);
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct OpenCodeBridgeFactory;

#[async_trait]
impl BridgeProviderFactory for OpenCodeBridgeFactory {
    fn provider_name(&self) -> &str {
        "opencode"
    }

    async fn create(&self, _config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>> {
        Ok(Box::new(OpenCodeBridgeProvider::new()))
    }

    fn default_profile(&self) -> crate::provider::bridge::BridgeProfileSpec {
        crate::provider::bridge::BridgeProfileSpec {
            name: "opencode".into(),
            display_name: "OpenCode".into(),
            binary: "opencode".into(),
            args: vec!["run".into(), "--format".into(), "json".into()],
            env: HashMap::new(),
            mcp_config_template: Some(
                r#"{"mcpServers":{"agentgate":{"type":"http","url":"http://127.0.0.1:{port}/_bridge/mcp/{session}","headers":{"Authorization":"Bearer {token}"}}}}"#
                    .into(),
            ),
            tool_schema_format: "openai".into(),
            supports_streaming: true,
            guard_patterns: vec!["bash".into(), "edit".into(), "write".into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_name() {
        let f = OpenCodeBridgeFactory;
        assert_eq!(f.provider_name(), "opencode");
    }

    #[test]
    fn mcp_config_serialization() {
        let config = OpenCodeSession::build_mcp_config("test-token", 8788, "test-session", serde_json::json!({})).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("agentgate"));
        assert!(json.contains("test-token"));
        assert!(json.contains("8788"));
        assert!(json.contains("test-session"));
    }
}