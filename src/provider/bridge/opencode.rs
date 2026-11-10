//! OpenCode bridge provider — V2 adapter implementing spec_tooling.md.

use crate::bridge::BridgeClient;
use crate::provider::bridge::{
    BridgeConfig, BridgeProfile, BridgeProviderFactory, BridgeProviderV2, BridgeSession,
    BridgeToolResult, ContinuationCapability, ContinuationHandle, ContinuationOutput, McpConfig, McpServerConfig, SessionOutput,
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
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex, oneshot};
use tracing::{debug, info, warn};
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
                    "execute".into(),
                ],
            },
        }
    }

    /// Build tool-disabling config for OpenCode per spec §12.2
    /// OpenCode uses specific permission keys: externalDirectory, network, etc.
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
                "execute": false,
                "*": false
            },
            "permission": {
                "*": "deny",
                "externalDirectory": "deny",
                "network": "deny",
                "edit": "deny",
                "bash": "deny",
                "webfetch": "deny",
                "write": "deny",
                "read": "deny",
                "execute": "deny"
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
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<Value>>>>,
    closed: Arc<Mutex<bool>>,
    mcp_token: String,
    mcp_port: u16,
    /// Empty working directory for this session (spec §11.3)
    workdir: PathBuf,
    /// PR-S4: continuation sender for live resume (opencode capability-gated).
    continuation_tx: Arc<Mutex<Option<oneshot::Sender<ContinuationOutput>>>>,
}

impl OpenCodeSession {
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
        info!(target: "agentgate.bridge.opencode.config", session_id = %session_id, transport = %transport, "Transport mode selected");

        // Create empty working directory per session (spec §11.3)
        let data_dir = config
            .data_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("agentgate"));
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        // Build MCP config with tool-disabling config
        let _mcp_config = Self::build_mcp_config(&mcp_token, mcp_port, &session_id, &tool_disable_config)?;

        // Resolve binary on Windows where PATHEXT matters (rust Command does not
        // search extensions automatically).
        let mut binary = profile.binary.clone();
        #[cfg(target_os = "windows")]
        {
            if !std::path::Path::new(&binary).exists() {
                for ext in [".cmd", ".bat", ".exe"] {
                    let candidate = format!("{}{}", binary, ext);
                    if which::which(&candidate).is_ok() {
                        binary = candidate;
                        break;
                    }
                }
            }
        }

        // Prepare command: opencode v2.x expects `run` subcommand flags after `run`
        // (e.g. `opencode run --model ... --format json`). Placing --model/--format
        // before `run` triggers "Unrecognized flag" on the root command.
        let mut cmd = Command::new(&binary);
        // profile.args typically ["run","--format","json"]; normalize order.
        let has_run = profile.args.iter().any(|a| a == "run");
        if has_run {
            cmd.arg("run");
            cmd.arg("--model").arg(&config.model);
            for a in &profile.args {
                if a != "run" && a != "--model" {
                    // drop any stray --model (we set explicitly)
                    cmd.arg(a);
                }
            }
        } else {
            cmd.arg("run")
                .arg("--model")
                .arg(&config.model)
                .arg("--format")
                .arg("json")
                .args(&profile.args);
        }

        // Set working directory to empty session workdir (spec §11.3)
        cmd.current_dir(&workdir);

        // Add agentgate binary directory to PATH so opencode can find mcp-shim
        if let Ok(current_exe) = std::env::current_exe() {
            if let Some(agentgate_dir) = current_exe.parent() {
                let sep = if cfg!(target_os = "windows") { ";" } else { ":" };
                let mut path = std::env::var("PATH").unwrap_or_default();
                let agentgate_dir_str = agentgate_dir.to_string_lossy().to_string();
                if !path.split(sep).any(|p| p == agentgate_dir_str) {
                    path = format!("{}{}{}", agentgate_dir_str, sep, path);
                    cmd.env("PATH", path);
                }
            }
        }

        // Inject the MCP config as the session's *project* config file.
        //
        // Two independent reasons rule out the old `OPENCODE_CONFIG_CONTENT`
        // env-var injection:
        //   1. opencode 2.x ignores that variable (verified on v2.2.6 with
        //      `opencode debug config`), so the bridge config was silently lost.
        //   2. A single environment string may not exceed MAX_ARG_STRLEN
        //      (128 KiB on Linux). A large tool set (~90 MCP tools) made the
        //      JSON bigger than that, `execve` returned E2BIG and the run died
        //      with "Failed to spawn opencode process".
        // `opencode.json` in the child's working directory is honoured by the
        // CLI and has no size limit.
        let mcp_config_json = Self::build_opencode_config_json(
            &mcp_token,
            mcp_port,
            &session_id,
            &tool_disable_config,
            &transport,
            config.host.as_str(),
            &config.tools,
        );
        let config_path = workdir.join("opencode.json");
        fs::write(&config_path, mcp_config_json.as_bytes())
            .await
            .with_context(|| format!("Failed to write opencode config {}", config_path.display()))?;

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
            config_path = %config_path.display(),
            transport = %transport,
            "Spawning OpenCode CLI with injected project config"
        );
        let path_env = std::env::var("PATH").unwrap_or_default();
        info!(
            target: "agentgate.bridge.opencode.config",
            session_id = %session_id,
            path_len = path_env.len(),
            "PATH being used for opencode spawn"
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
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                anyhow!(
                    "Failed to spawn opencode process: binary={} cwd={} config_bytes={} error={}",
                    binary,
                    workdir.display(),
                    mcp_config_json.len(),
                    e
                )
            })?;

        info!(
            target: "agentgate.bridge.opencode.spawn",
            session_id = %session_id,
            pid = ?child.id(),
            "OpenCode process spawned"
        );

        // Debug: verify actual working directory of spawned process (spec §11.3)
        if let Some(pid) = child.id() {
            #[cfg(target_os = "linux")]
            {
                if let Ok(cwd) = std::fs::read_link(format!("/proc/{}/cwd", pid)) {
                    info!(
                        target: "bridge.opencode.cwd",
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
                            target: "bridge.opencode.cwd",
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

        // Store child in Arc<Mutex> early so we can clone for spawned tasks
        let child = Arc::new(Mutex::new(Some(child)));

        // Set up event channel
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Spawn stderr reader task
        let child_for_stderr = child.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            let mut line_count = 0;
            while let Ok(Some(line)) = reader.next_line().await {
                line_count += 1;
                info!(target: "agentgate.bridge.opencode.stderr", line_count, line = %line, "Read line from opencode stderr");
            }
            // Check if process exited
            let mut child_guard = child_for_stderr.lock().await;
            if let Some(ref mut child) = child_guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        info!(target: "agentgate.bridge.opencode.stderr", line_count, status = %status, "opencode process exited (from stderr reader)");
                    }
                    Ok(None) => {
                        info!(target: "agentgate.bridge.opencode.stderr", line_count, "opencode stderr reader finished but process still running");
                    }
                    Err(e) => {
                        warn!(target: "agentgate.bridge.opencode.stderr", error = %e, "Failed to check process status");
                    }
                }
            }
            info!(target: "agentgate.bridge.opencode.stderr", line_count, "opencode stderr reader finished");
        });

        // Spawn stdout reader task
        let event_tx_stdout = event_tx.clone();
        let child_for_stdout = child.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            let mut line_count: u64 = 0;
            while let Ok(Some(line)) = reader.next_line().await {
                line_count += 1;
                // Log length only (no full JSON dump). ANSI-stripped length for
                // signal; full payload stays off the hot log path.
                let line_len = strip_ansi_len(&line);
                let preview: String = line.chars().take(200).collect();
                info!(
                    target: "agentgate.bridge.opencode.stdout",
                    line_count,
                    line_len,
                    preview = %preview,
                    "Read line from opencode stdout"
                );
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(json) = serde_json::from_str::<Value>(&line) {
                    if event_tx_stdout.send(json).is_err() {
                        break; // Receiver dropped
                    }
                } else {
                    let preview = strip_ansi_preview(&line, 120);
                    warn!(
                        target: "agentgate.bridge.opencode.stdout",
                        line_count,
                        line_len,
                        preview = %preview,
                        "Failed to parse JSON from opencode stdout"
                    );
                }
            }
            // Check if process exited
            let mut child_guard = child_for_stdout.lock().await;
            if let Some(ref mut child) = child_guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        info!(target: "agentgate.bridge.opencode.stdout", line_count, status = %status, "opencode process exited");
                    }
                    Ok(None) => {
                        info!(target: "agentgate.bridge.opencode.stdout", line_count, "opencode stdout reader finished but process still running");
                    }
                    Err(e) => {
                        warn!(target: "agentgate.bridge.opencode.stdout", error = %e, "Failed to check process status");
                    }
                }
            }
            info!(target: "agentgate.bridge.opencode.stdout", line_count, "opencode stdout reader finished");
        });

        let session = Self {
            client,
            profile,
            session_id,
            child,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            event_rx: Arc::new(Mutex::new(Some(event_rx))),
            closed: Arc::new(Mutex::new(false)),
            mcp_token,
            mcp_port,
            workdir,
            continuation_tx: Arc::new(Mutex::new(None)),
        };

        // Wait for MCP initialization
        session.wait_for_mcp_initialize().await?;

        Ok(session)
    }

    fn build_mcp_config(token: &str, port: u16, session_id: &str, tool_disable_config: &Value) -> Result<McpConfig> {
        let mut mcp_servers = HashMap::new();
        mcp_servers.insert(
            "agentgate".into(),
            McpServerConfig {
                r#type: "http".into(),
                url: Some(format!("http://127.0.0.1:{port}/_bridge/mcp/{session_id}")),
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
                    url: None,
                    command: Some("true".into()), // dummy command, config only
                    args: None,
                    env: Some({
                        let mut env = HashMap::new();
                        env.insert(
                            "OPENCODE_CONFIG_CONTENT".into(),
                            serde_json::to_string(tool_disable_config)?,
                        );
                        env
                    }),
                    headers: None,
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
        transport: &str,
        bridge_host: &str,
        tools: &[crate::bridge::ToolSchema],
    ) -> String {
        let tools_json = serde_json::to_string(tools).unwrap_or_default();
        let agentgate_bin = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "agentgate".to_string());

        let (mcp_config, mcp_servers_config) = if transport == "stdio_shim" {
            // Command-based MCP server (stdio shim). Include multiple field names
            // for compatibility across OpenCode config schema variants.
            let mcp = serde_json::json!({
                "agentgate": {
                    "type": "local",
                    "command": [
                        agentgate_bin.clone(),
                        "mcp-shim".to_string(),
                        "--session".to_string(),
                        session_id.to_string(),
                        "--token".to_string(),
                        token.to_string(),
                        "--host".to_string(),
                        bridge_host.to_string(),
                        "--port".to_string(),
                        port.to_string()
                    ],
                    "environment": { "AGENTGATE_SHIM_TOOLS": tools_json.clone() },
                    "enabled": true
                }
            });
            let mcp_servers = serde_json::json!({
                "agentgate": {
                    "command": agentgate_bin,
                    "args": ["mcp-shim", "--session", session_id, "--token", token, "--host", bridge_host, "--port", &port.to_string()],
                    "env": { "AGENTGATE_SHIM_TOOLS": tools_json }
                }
            });
            info!(
                target: "agentgate.bridge.opencode.config",
                port = port,
                session_id = %session_id,
                tools_count = tools.len(),
                "Generated stdio_shim MCP config"
            );
            (mcp, mcp_servers)
        } else {
            // Default HTTP transport (spec §12.2 style) + compatibility key.
            let url = format!("http://{bridge_host}:{port}/_bridge/mcp/{session_id}");
            let mcp = serde_json::json!({
                "agentgate": {
                    "type": "remote",
                    "url": url.clone(),
                    "headers": { "Authorization": format!("Bearer {token}") },
                    "enabled": true
                }
            });
            let mcp_servers = serde_json::json!({
                "agentgate": {
                    "type": "http",
                    "url": url,
                    "headers": { "Authorization": format!("Bearer {token}") }
                }
            });
            tracing::info!(target: "agentgate.bridge.opencode.config", port = port, session_id = %session_id, "Generated HTTP MCP config");
            (mcp, mcp_servers)
        };

        // Include both `mcp` (spec/OpenCode style) and `mcpServers` for compatibility.
        serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": mcp_config,
            "mcpServers": mcp_servers_config,
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
            info!(
                target: "agentgate.bridge.opencode.stdin",
                message = %line,
                "Writing to opencode stdin"
            );
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            info!(
                target: "agentgate.bridge.opencode.stdin",
                "Successfully wrote to opencode stdin"
            );
        } else {
            warn!(target: "agentgate.bridge.opencode.stdin", "No stdin available to write to");
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

    /// PR-S4: opencode MCP-hold flow can continue when the CLI process stays alive and
    /// MCP `tools/call` holds are resolved with real results. Full live resume routing
    /// is wired incrementally; capability-gated and replay fallback remains mandatory.
    fn continuation_capability(&self) -> ContinuationCapability {
        ContinuationCapability::ToolResultContinuation
    }

    fn continuation_handle(&self) -> Option<ContinuationHandle> {
        Some(ContinuationHandle {
            session_id: self.session_id.clone(),
            provider: "opencode".to_string(),
            capability: ContinuationCapability::ToolResultContinuation,
        })
    }

    /// Preflight isolation check per spec §13.2 (wired via shared helper).
    async fn preflight_isolation_check(&self) -> Result<bool> {
        crate::provider::bridge::preflight_isolation_check(&self.profile, &self.session_id, "opencode").await
    }

    /// Check if the underlying CLI process is still alive.
    async fn check_liveness(&self) -> Result<bool> {
        let mut child_guard = self.child.lock().await;
        if let Some(child) = child_guard.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => Ok(false), // Process has exited
                Ok(None) => Ok(true),     // Process is still running
                Err(e) => Err(anyhow!("Failed to check process status: {}", e)),
            }
        } else {
            Ok(false) // No child process
        }
    }

    async fn send_user_message(&self, text: &str) -> Result<()> {
        if *self.closed.lock().await {
            return Err(anyhow!("Session is closed"));
        }

        // Send user message to CLI via stdin
        // For non-persistent mode, send plain text and close stdin to signal EOF
        let mut stdin_guard = self.stdin.lock().await;
        if let Some(stdin) = stdin_guard.as_mut() {
            stdin.write_all(text.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
            // Explicitly drop stdin to close the pipe and signal EOF
            drop(stdin_guard.take());
        } else {
            return Err(anyhow!("No stdin available"));
        }
        Ok(())
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

        // Kill the child process (process tree on Windows per audit P0-3)
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            let pid = child.id();
            // Drop stdin explicitly before killing to ensure clean pipe closure if process still reads
            drop(self.stdin.lock().await.take());
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

        // Clean up workdir
        let _ = fs::remove_dir_all(&self.workdir).await;

        info!("OpenCode session {} closed", self.session_id);
        Ok(())
    }

    async fn wait_for_completion(&self) -> Result<SessionOutput> {
        use crate::reasoning::parser::ReasoningParser;
        use crate::reasoning::RunEvent;

        // Drain events from the channel to collect output. Reuse the shared
        // tolerant parser so OpenCode's JSONL event shapes (`step_start`,
        // `text` with `part.text`, `tool_use`, `error`, `step_finish`) are
        // mapped consistently with the native CLI path.
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<crate::types::ToolCall> = Vec::new();
        let error: Option<String> = None;

        // Take the event receiver
        let mut rx_guard = self.event_rx.lock().await;
        if let Some(mut rx) = rx_guard.take() {
            let mut parser = ReasoningParser::new();
            while let Some(event) = rx.recv().await {
                let line = event.to_string();
                let parsed_events = parser.parse_line(&line);
                debug!(target: "agentgate.bridge.opencode", line = %line, "raw opencode event");
                for parsed in &parsed_events {
                    debug!(target: "agentgate.bridge.opencode", event = ?parsed, "parsed run event");
                }
                for parsed in parsed_events {
                    match parsed {
                        RunEvent::Text { text } => {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(&text);
                        }
                        RunEvent::ReasoningDelta { text, .. } => reasoning.push_str(&text),
                        RunEvent::ToolCalls { calls } => tool_calls.extend(calls),
                        RunEvent::Error { message, .. } => {
                            // Treat an explicit CLI error event as terminal: return
                            // immediately instead of waiting for process exit / turn timeout.
                            drop(rx_guard);
                            return Err(anyhow!("provider_error: {}", message));
                        }
                        _ => {}
                    }
                }
                // PR-S4: if tool calls detected, park early for continuation (don't wait for CLI exit)
                if !tool_calls.is_empty() {
                    // Put receiver back for continuation task
                    *rx_guard = Some(rx);
                    // Don't wait for process exit - CLI stays alive for continuation
                    break;
                }
            }
            // rx.recv() returned None: stdout EOF / channel closed while turn still active.
            // Treat as terminal if we haven't collected any output yet.
            if content.trim().is_empty() && tool_calls.is_empty() && error.is_none() {
                drop(rx_guard);
                return Err(anyhow!("provider_error: CLI produced no output (stdout EOF, no output)"))
            }
            for parsed in parser.finish() {
                match parsed {
                    RunEvent::Text { text } => {
                        if !content.is_empty() {
                            content.push('\n');
                        }
                        content.push_str(&text);
                    }
                    RunEvent::ToolCalls { calls } => tool_calls.extend(calls),
                    _ => {}
                }
            }
        }
        drop(rx_guard);

        // Wait for process exit, but never forever: some CLIs close stdout and
        // keep the process alive (e.g. OpenCode retrying a provider error while
        // an MCP server is attached). Once stdout reached EOF the turn is over.
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                Ok(Ok(status)) => debug!("OpenCode process exited with status: {}", status),
                Ok(Err(e)) => warn!("OpenCode process wait failed: {}", e),
                Err(_) => {
                    warn!("OpenCode process did not exit after stdout EOF; killing it");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }
        drop(child_guard);

        // PR-S4: if tool calls detected, this is a continuation park - don't wait for process exit,
        // don't take child process, leave it alive for continuation.
        let is_continuation_park = !tool_calls.is_empty();
        if !is_continuation_park {
            if let Some(message) = error {
                return Err(anyhow!("provider_error: {}", message));
            }
            if content.trim().is_empty() && tool_calls.is_empty() {
                return Err(anyhow!(
                    "provider_error: CLI produced no output (upstream error or unsupported model)"
                ));
            }
        }

        Ok(SessionOutput {
            content: content.trim().to_string(),
            reasoning: if reasoning.trim().is_empty() {
                None
            } else {
                Some(reasoning.trim().to_string())
            },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        })
    }

    /// PR-S4: Wait for continuation output after MCP hold resolution.
    /// Continues reading events from the receiver (put back by wait_for_completion)
    /// and waits for process exit.
    async fn wait_for_continuation(&self) -> Result<ContinuationOutput> {
        use crate::reasoning::parser::ReasoningParser;
        use crate::reasoning::RunEvent;

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<crate::types::ToolCall> = Vec::new();
        let error: Option<String> = None;

        // Take the event receiver (put back by wait_for_completion)
        let mut rx_guard = self.event_rx.lock().await;
        if let Some(mut rx) = rx_guard.take() {
            let mut parser = ReasoningParser::new();
            while let Some(event) = rx.recv().await {
                let line = event.to_string();
                let parsed_events = parser.parse_line(&line);
                for parsed in parsed_events {
                    match parsed {
                        RunEvent::Text { text } => {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(&text);
                        }
                        RunEvent::ReasoningDelta { text, .. } => reasoning.push_str(&text),
                        RunEvent::ToolCalls { calls } => tool_calls.extend(calls),
                        RunEvent::Error { message, .. } => {
                            // Treat an explicit CLI error event as terminal: return
                            // immediately instead of waiting for process exit.
                            drop(rx_guard);
                            return Err(anyhow!("provider_error: {}", message));
                        }
                        _ => {}
                    }
                }
            }
            // rx.recv() returned None: stdout EOF / channel closed while turn still active.
            // Treat as terminal if we haven't collected any output yet.
            if content.trim().is_empty() && tool_calls.is_empty() && error.is_none() {
                drop(rx_guard);
                return Err(anyhow!("provider_error: CLI produced no output (stdout EOF, no output)"))
            }
            for parsed in parser.finish() {
                match parsed {
                    RunEvent::Text { text } => {
                        if !content.is_empty() {
                            content.push('\n');
                        }
                        content.push_str(&text);
                    }
                    RunEvent::ToolCalls { calls } => tool_calls.extend(calls),
                    _ => {}
                }
            }
        }
        drop(rx_guard);

        // Wait for process exit (CLI should exit after sending continuation output)
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
                Ok(Ok(status)) => debug!("OpenCode process exited with status: {}", status),
                Ok(Err(e)) => warn!("OpenCode process wait failed: {}", e),
                Err(_) => {
                    warn!("OpenCode process did not exit after continuation; killing it");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }
        drop(child_guard);

        if let Some(message) = error {
            return Err(anyhow!("provider_error: {}", message));
        }
        if content.trim().is_empty() && tool_calls.is_empty() {
            return Err(anyhow!(
                "provider_error: CLI produced no output during continuation"
            ));
        }

        Ok(ContinuationOutput {
            content: content.trim().to_string(),
            reasoning: if reasoning.trim().is_empty() { None } else { Some(reasoning.trim().to_string()) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            session_id: Some(self.session_id.clone()),
        })
    }

    async fn take_event_receiver(&self) -> Result<Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>> {
        let mut rx_guard = self.event_rx.lock().await;
        Ok(rx_guard.take())
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
            guard_patterns: vec!["bash".into(), "edit".into(), "write".into(), "execute".into()],
        }
    }
}

/// Strip ANSI escape sequences from a string and return the byte length of the
/// sanitized result. Used so the opencode stdout log records a bounded length
/// instead of dumping full JSON lines.
fn strip_ansi_len(s: &str) -> usize {
    let mut len: usize = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip CSI sequence: ESC [ ... final byte (@..~)
            let mut j = i + 2;
            while j < bytes.len() && !matches!(bytes[j], b'@'..=b'~') {
                j += 1;
            }
            i = j + 1;
            continue;
        }
        len += 1;
        i += 1;
    }
    len
}

/// Strip ANSI escape sequences and return a bounded preview (first `max` chars).
fn strip_ansi_preview(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && !matches!(bytes[j], b'@'..=b'~') {
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if out.chars().count() >= max {
            break;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
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
        let tool_disable = serde_json::json!({});
        let config = OpenCodeSession::build_mcp_config("test-token", 8788, "test-session", &tool_disable).unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("agentgate"));
        assert!(json.contains("test-token"));
        assert!(json.contains("8788"));
        assert!(json.contains("test-session"));
    }
}