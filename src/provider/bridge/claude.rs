//! Claude Code bridge provider — V2 adapter implementing spec_tooling.md.

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
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

/// Claude Code bridge provider (V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeBridgeProvider {
    profile: BridgeProfile,
}

impl ClaudeBridgeProvider {
    pub fn new() -> Self {
        Self {
            profile: BridgeProfile {
                name: "claude".into(),
                display_name: "Claude Code".into(),
                binary: "claude".into(),
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
                    "glob".into(),
                    "grep".into(),
                    "web_search".into(),
                    "task".into(),
                ],
            },
        }
    }
}

impl Default for ClaudeBridgeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BridgeProviderV2 for ClaudeBridgeProvider {
    fn profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn start_bridge(
        &self,
        config: BridgeConfig,
        client: Arc<BridgeClient>,
    ) -> Result<Box<dyn BridgeSession>> {
        let session = ClaudeSession::spawn(config, client, self.profile.clone()).await?;
        Ok(Box::new(session))
    }

    async fn health_check(&self) -> Result<bool> {
        match Command::new(&self.profile.binary).arg("--version").output().await {
            Ok(output) => Ok(output.status.success()),
            Err(_) => Ok(false),
        }
    }
}

/// Claude Code CLI session that manages the CLI process and MCP communication
struct ClaudeSession {
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
}

impl ClaudeSession {
    async fn spawn(
        config: BridgeConfig,
        client: Arc<BridgeClient>,
        profile: BridgeProfile,
    ) -> Result<Self> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let mcp_token = uuid::Uuid::new_v4().to_string().replace("-", "");
        let mcp_port = config.port;

        // Build MCP config
        let mcp_config = Self::build_mcp_config(&mcp_token, mcp_port, &session_id)?;

        // Prepare command
        let mut cmd = Command::new(&profile.binary);
        cmd.args(&profile.args);
        
        // Set environment variables for MCP config
        // Claude Code uses CLAUDE_CONFIG or similar env var
        cmd.env("CLAUDE_MCP_CONFIG", serde_json::to_string(&mcp_config)?);
        
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
            .context("Failed to spawn claude process")?;

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
                        break;
                    }
                }
            }
        });

        // Spawn stderr reader task (for logging)
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!(target: "claude.stderr", line = %line);
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
        };

        // Wait for MCP initialization
        session.wait_for_mcp_initialize().await?;

        Ok(session)
    }

    fn build_mcp_config(token: &str, port: u16, session_id: &str) -> Result<McpConfig> {
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
        Ok(McpConfig { mcp_servers })
    }

    async fn wait_for_mcp_initialize(&self) -> Result<()> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
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
impl BridgeSession for ClaudeSession {
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
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        info!("Claude session {} closed", self.session_id);
        Ok(())
    }

    async fn wait_for_completion(&self) -> Result<()> {
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let status = child.wait().await?;
            debug!("Claude process exited with status: {}", status);
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ClaudeBridgeFactory;

#[async_trait]
impl BridgeProviderFactory for ClaudeBridgeFactory {
    fn provider_name(&self) -> &str {
        "claude"
    }

    async fn create(&self, _config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>> {
        Ok(Box::new(ClaudeBridgeProvider::new()))
    }

    fn default_profile(&self) -> crate::provider::bridge::BridgeProfileSpec {
        crate::provider::bridge::BridgeProfileSpec {
            name: "claude".into(),
            display_name: "Claude Code".into(),
            binary: "claude".into(),
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
        let f = ClaudeBridgeFactory;
        assert_eq!(f.provider_name(), "claude");
    }

    #[test]
    fn mcp_config_serialization() {
        let config = ClaudeSession::build_mcp_config("test-token", 8788, "test-session").unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("agentgate"));
        assert!(json.contains("test-token"));
        assert!(json.contains("8788"));
        assert!(json.contains("test-session"));
    }
}