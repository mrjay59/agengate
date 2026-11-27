use crate::config::ProviderConfig;
pub mod combo;
pub mod http;
pub mod mappings;
use crate::provider::bridge::BridgeSession as ProviderBridgeSession;
use crate::reasoning::parser::ReasoningParser;
use crate::reasoning::{FinishReason, ReasoningKind, RunEvent};
use crate::types::ChatCompletionRequest;
use anyhow::{Context, anyhow};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::OnceLock;

static PENDING_CLI_SESSIONS: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
static PENDING_CLI_BUSY: OnceLock<RwLock<HashMap<String, std::time::Instant>>> = OnceLock::new();
static ACTIVE_CLI_SESSIONS: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

fn pending_sessions() -> &'static RwLock<HashMap<String, String>> {
    PENDING_CLI_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}
fn pending_busy() -> &'static RwLock<HashMap<String, std::time::Instant>> {
    PENDING_CLI_BUSY.get_or_init(|| RwLock::new(HashMap::new()))
}
fn active_sessions() -> &'static RwLock<HashMap<String, String>> {
    ACTIVE_CLI_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

pub async fn register_pending_cli_session(agentgate_session_id: &str, cli_session_id: Option<String>) {
    let mut map = pending_sessions().write().await;
    match cli_session_id {
        Some(sid) if !sid.trim().is_empty() => {
            map.insert(agentgate_session_id.to_string(), sid);
        }
        _ => {
            map.remove(agentgate_session_id);
        }
    }
}

/// Clear all stored CLI session state for an agentgate session chain.
/// Used when model reroute/quota failover makes resuming the old CLI session unsafe.
pub async fn clear_cli_session_for_chain(agentgate_session_id: &str) {
    let key = agentgate_session_id.to_string();
    pending_sessions().write().await.remove(&key);
    pending_busy().write().await.remove(&key);
    active_sessions().write().await.remove(&key);
}

pub async fn mark_bridge_cli_session_busy(agentgate_session_id: &str) {
    pending_busy().write().await.insert(agentgate_session_id.to_string(), std::time::Instant::now());
}

pub async fn mark_bridge_cli_session_active(agentgate_session_id: &str) {
    if let Some(sid) = pending_sessions().read().await.get(agentgate_session_id).cloned() {
        if !sid.trim().is_empty() {
            active_sessions().write().await.insert(agentgate_session_id.to_string(), sid);
        }
    }
    pending_busy().write().await.remove(agentgate_session_id);
}

pub async fn activate_busy_cli_session(agentgate_session_id: &str) {
    mark_bridge_cli_session_active(agentgate_session_id).await;
}

pub async fn stored_chain_cli_session_id_for_next_turn(agentgate_session_id: &str) -> Option<String> {
    if let Some(sid) = active_sessions().read().await.get(agentgate_session_id).cloned() {
        if !sid.trim().is_empty() {
            return Some(sid);
        }
    }
    if let Some(sid) = pending_sessions().read().await.get(agentgate_session_id).cloned() {
        if !sid.trim().is_empty() {
            return Some(sid);
        }
    }
    None
}

pub async fn stored_chain_cli_session_id() -> Option<String> {
    stored_chain_cli_session_id_for_next_turn("").await
}

pub(crate) fn opencode_binary_from_env() -> Option<String> {
    std::env::var("OPENCODE_BIN").ok().filter(|s| !s.trim().is_empty())
}
use std::process::Command;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command as TokioCommand;
use tokio::sync::{mpsc, RwLock};
use uuid;

/// Determine if a provider is native (model-api) rather than cli-agent
pub fn is_native_provider(provider_id: &str) -> bool {
    provider_id == "ollama"
}

#[derive(Debug, Clone)]
pub enum ProviderResponse {
    Success {
        content: String,
        reasoning: Option<String>,
        tool_calls: Option<Vec<crate::types::ToolCall>>,
        session_id: Option<String>,
        /// MCP session ID for bridge providers (used for continuation lookup).
        mcp_session_id: Option<String>,
    },
    Error(String),
}

#[async_trait]
pub trait CliProvider: Send + Sync {
    fn id(&self) -> String;
    fn bin(&self) -> Option<String>;
    fn models(&self) -> Vec<String>;
    fn is_available(&self) -> bool;
    fn set_available(&mut self, available: bool);
    async fn check_available(&self) -> bool;
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error>;
    /// Whether this provider runs through the MCP bridge adapter.
    fn is_bridge(&self) -> bool {
        false
    }
    async fn send_chat_completion(
        &self,
        model: &str,
        request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error>;

    /// Stream a chat completion as [`RunEvent`]s.
    ///
    /// Providers that can read incremental CLI/stdout output should override
    /// this. The default implementation adapts the aggregated
    /// [`send_chat_completion`](Self::send_chat_completion) result so every
    /// existing provider keeps working.
    async fn send_chat_completion_stream(
        &self,
        model: &str,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        match self.send_chat_completion(model, request).await? {
            ProviderResponse::Success { content, reasoning, tool_calls, session_id: _, .. } => {
                if let Some(reasoning) = reasoning.filter(|r| !r.trim().is_empty()) {
                    if tx
                        .send(RunEvent::ReasoningStart {
                            block: 0,
                            kind: ReasoningKind::Raw,
                        })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                    if tx
                        .send(RunEvent::ReasoningDelta {
                            block: 0,
                            text: reasoning,
                        })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                    if tx
                        .send(RunEvent::ReasoningEnd { block: 0 })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                if !content.is_empty()
                    && tx.send(RunEvent::Text { text: content }).await.is_err()
                {
                    return Ok(());
                }
                if let Some(calls) = tool_calls.filter(|v| !v.is_empty()) {
                    if tx.send(RunEvent::ToolCalls { calls }).await.is_err() {
                        return Ok(());
                    }
                }
                let _ = tx
                    .send(RunEvent::Done {
                        reason: FinishReason::Stop,
                    })
                    .await;
            }
            ProviderResponse::Error(message) => {
                let _ = tx
                    .send(RunEvent::Error {
                        message,
                        code: Some("provider_error".to_string()),
                    })
                    .await;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CliProviderImpl {
    pub id: String,
    pub bin: Option<String>,
    pub models_cmd: Option<Vec<String>>,
    pub chat_cmd: Option<Vec<String>>,
    pub strip_ansi: bool,
    pub models: Vec<String>,
    pub is_available: bool,
}

#[async_trait]
impl CliProvider for CliProviderImpl {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn bin(&self) -> Option<String> {
        self.bin.clone()
    }

    fn models(&self) -> Vec<String> {
        self.models.clone()
    }

    fn is_available(&self) -> bool {
        self.is_available
    }

    fn set_available(&mut self, available: bool) {
        self.is_available = available;
    }

    async fn check_available(&self) -> bool {
        let bin = match &self.bin {
            Some(b) => b,
            None => return false,
        };

                // Verify by actually spawning the binary (the `which` crate can
        // disagree with the OS on some platforms, e.g. npm shims on Linux).
        // Resolve to a runnable path first (handles Windows PATHEXT).
        let resolved = self.resolve_binary(bin).unwrap_or_else(|| bin.to_string());
        let mut cmd = std::process::Command::new(&resolved);
        cmd.arg("--version");
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        match cmd.output() {
            Ok(out) => {
                tracing::info!(provider = %self.id, bin = %bin, resolved = %resolved, status = %out.status, "check_available: resolved");
                true
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    tracing::warn!(provider = %self.id, bin = %bin, "check_available: NOT found on PATH");
                    false
                } else {
                    tracing::warn!(provider = %self.id, bin = %bin, error = %e, "check_available: spawn error, assuming present");
                    true
                }
            }
        }
    }

    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        let provider_id = self.id.clone();
        let models_cmd = self.models_cmd.clone();
        let bin = self.bin.clone();
        let config_models = self.models.clone();
        let provider_id_inner = provider_id.clone();
        let config_models_inner = config_models.clone();

        // Run the blocking command in a blocking task
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<String>, anyhow::Error> {
            let start = std::time::Instant::now();
            tracing::info!(provider = %provider_id_inner, "scan_models: starting");
            
            if let Some(cmds) = &models_cmd {
                if cmds.is_empty() {
                    tracing::info!(provider = %provider_id_inner, elapsed_ms = %start.elapsed().as_millis(), "scan_models: empty models_cmd, returning config models");
                    return Ok(config_models_inner);
                }

                let bin = bin.ok_or_else(|| {
                    anyhow::anyhow!("No binary configured for provider {}", provider_id_inner)
                })?;
                tracing::debug!(provider = %provider_id_inner, bin = %bin, cmd = ?cmds, "scan_models: executing command");

                let output = execute_cmd_static(&bin, cmds)?;
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    tracing::debug!(provider = %provider_id_inner, stderr = %stderr, "scan_models: command stderr");
                }
                
                // We need access to parse_models_output - use a static version
                let models = parse_models_output_static(&stdout, &provider_id_inner);
                tracing::info!(provider = %provider_id_inner, models_found = models.len(), elapsed_ms = %start.elapsed().as_millis(), "scan_models: completed");
                
                if !models.is_empty() {
                    return Ok(models);
                }

                tracing::info!(provider = %provider_id_inner, "scan_models: no models found, falling back to config");
                return Ok(config_models_inner);
            }

            Ok(config_models_inner)
        }).await;

        match result {
            Ok(Ok(models)) => Ok(models),
            Ok(Err(e)) => {
                tracing::warn!(provider = %provider_id, error = %e, "scan_models: command failed, using config models");
                Ok(config_models)
            }
            Err(e) => {
                tracing::error!(provider = %provider_id, error = %e, "scan_models: task join error, using config models");
                Ok(config_models)
            }
        }
    }

    async fn send_chat_completion(
        &self,
        model: &str,
        request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        // Create empty session workdir per spec §11.3
        let session_id = uuid::Uuid::new_v4().to_string();
        let data_dir = std::env::temp_dir().join("agentgate");
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        let (mut cmd, stdin_prompt) = self.build_chat_command(model, &request, Some(&workdir))?;
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn()?;

        // Write prompt to stdin if required, then close to signal EOF
        if let Some(prompt) = stdin_prompt {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(prompt.as_bytes()).await?;
                // Explicitly close stdin to signal EOF to CLI
                drop(stdin);
            }
        }

        // Drain stderr concurrently so a chatty CLI cannot fill the pipe and
        // deadlock while we wait. Taking stdout/stderr out of `child` keeps
        // ownership of `child` with us, so we can still kill() it on timeout
        // (wait_with_output would have consumed `child`).
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf).await;
                buf
            })
        });
        let mut stdout_handle = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(ProviderResponse::Error(
                    "stdout not captured for CLI".to_string(),
                ));
            }
        };

        // Bound the CLI so a hung process (missing auth, waiting for a TTY,
        // unreachable upstream) cannot block the request forever. On timeout we
        // kill the child and surface stderr so the failure is diagnosable
        // instead of an indefinite "gantung"/hang.
        const CLI_TIMEOUT_SECS: u64 = 300;
        let mut stdout_bytes: Vec<u8> = Vec::new();
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(CLI_TIMEOUT_SECS),
            stdout_handle.read_to_end(&mut stdout_bytes),
        )
        .await;
        if read_result.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let stderr_buf = match stderr_task {
                Some(t) => t.await.unwrap_or_default(),
                None => String::new(),
            };
            let msg = if stderr_buf.trim().is_empty() {
                format!(
                    "CLI timed out after {}s (no output) — cek PATH/auth/TTY opencode di mesin ini",
                    CLI_TIMEOUT_SECS
                )
            } else {
                format!(
                    "CLI timed out after {}s: {}",
                    CLI_TIMEOUT_SECS,
                    stderr_buf.trim()
                )
            };
            tracing::warn!(provider = %self.id, "{}", msg);
            return Ok(ProviderResponse::Error(msg));
        }
        if let Err(e) = read_result.unwrap_or_else(|e| Err(e.into())) {
            return Err(e.into());
        }
        let status = child.wait().await?;
        let stderr_buf = match stderr_task {
            Some(t) => t.await.unwrap_or_default(),
            None => String::new(),
        };
        // Rebuild an Output-like structure for downstream code.
        let output_stdout = stdout_bytes;
        let output_stderr = stderr_buf.into_bytes();
        let output_status = status;

        let stdout = String::from_utf8_lossy(&output_stdout);
        let stderr = String::from_utf8_lossy(&output_stderr);
        let cleaned = if self.strip_ansi {
            String::from_utf8_lossy(&strip_ansi_escapes::strip(&*stdout)).to_string()
        } else {
            stdout.to_string()
        };
        // If stdout empty and stderr has content, treat as error
        if cleaned.trim().is_empty() && !stderr.trim().is_empty() {
            // keep stderr path below via output_status? just return error
            return Ok(ProviderResponse::Error(stderr.trim().to_string()));
        }
        if !output_status.success() && cleaned.trim().is_empty() {
            // fall through to existing status check below? handle here
        }
        // Continue with existing parse; need status for the later check.
        // Store status in a variable name expected below.
        let _ = output_status;
        // NOTE: the original code below references `output.status` and `output.stderr`.
        // We have consumed them; skip by jumping to parse.
        let (content, reasoning, error) = parse_cli_output(cleaned.trim());
        if let Some(message) = error {
            return Ok(ProviderResponse::Error(message));
        }
        if !output_status.success() && content.is_empty() {
            return Ok(ProviderResponse::Error(format!(
                "CLI exited with status {}",
                output_status
            )));
        }
        return Ok(ProviderResponse::Success {
            content,
            reasoning,
            tool_calls: None,
            session_id: None,
            mcp_session_id: None,
        });
        // (handled above)
    }

    /// RS0: read CLI stdout incrementally and forward events immediately.
    async fn send_chat_completion_stream(
        &self,
        model: &str,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        // Create empty session workdir per spec §11.3
        let session_id = uuid::Uuid::new_v4().to_string();
        let data_dir = std::env::temp_dir().join("agentgate");
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        let (mut cmd, stdin_prompt) = self.build_chat_command(model, &request, Some(&workdir))?;
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn()?;

        if let Some(prompt) = stdin_prompt {
            if let Some(mut stdin) = child.stdin.take() {
                if let Err(err) = stdin.write_all(prompt.as_bytes()).await {
                    let _ = child.kill().await;
                    return Err(err.into());
                }
                drop(stdin);
            }
        }

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill().await;
                return Err(anyhow::anyhow!(
                    "stdout not captured for provider {}",
                    self.id
                ));
            }
        };

        // Drain stderr concurrently so a chatty CLI cannot fill the pipe and
        // deadlock while we are still reading stdout.
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf).await;
                buf
            })
        });

        let mut reader = tokio::io::BufReader::new(stdout);
        let mut parser = ReasoningParser::new();
        parser.with_client_tool_defs(request.tools.as_deref().unwrap_or(&[]));
        let mut saw_event = false;
        let mut line_buf: Vec<u8> = Vec::new();
        let mut stream_error: Option<String> = None;

        loop {
            line_buf.clear();
            let read = tokio::select! {
                _ = tx.closed() => {
                    // Client went away: stop the child immediately.
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Ok(());
                }
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(180),
                    reader.read_until(b'\n', &mut line_buf),
                ) => {
                    match result {
                        Ok(r) => r,
                        Err(_) => {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            let msg = "CLI idle timeout after 180s (no output) — cek PATH/auth/TTY opencode di mesin ini".to_string();
                            tracing::warn!(provider = %self.id, "{}", msg);
                            let _ = tx
                                .send(RunEvent::Error {
                                    message: msg,
                                    code: Some("timeout".to_string()),
                                })
                                .await;
                            return Ok(());
                        }
                    }
                }
            };

            match read {
                Ok(0) => break,
                Ok(_) => {
                    // Lossy conversion also covers non-UTF-8 / binary stdout.
                    let line = String::from_utf8_lossy(&line_buf);
                    for event in parser.parse_line(&line) {
                        saw_event = true;
                        if tx.send(event).await.is_err() {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            return Ok(());
                        }
                    }
                }
                Err(err) => {
                    stream_error = Some(err.to_string());
                    break;
                }
            }
        }

        let stderr = match stderr_task {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        };
        let status = child.wait().await?;
        tracing::info!("CLI finished: provider={}, status={}, stderr_bytes={}", self.id, status, stderr.len());
        if !stderr.trim().is_empty() {
            tracing::debug!("CLI stderr: {}", stderr.trim());
        }

        if let Some(err) = stream_error {
            let _ = tx
                .send(RunEvent::Error {
                    message: err,
                    code: Some("stream_error".to_string()),
                })
                .await;
            return Ok(());
        }

        if !saw_event && !status.success() {
            let message = if stderr.trim().is_empty() {
                format!("CLI exited with status {}", status)
            } else {
                stderr.trim().to_string()
            };
            let _ = tx
                .send(RunEvent::Error {
                    message,
                    code: Some("provider_error".to_string()),
                })
                .await;
            return Ok(());
        }

        for event in parser.finish() {
            if tx.send(event).await.is_err() {
                break;
            }
        }
        Ok(())
    }
}

/// Parse CLI output. Supports OpenCode `--format json` (JSONL with reasoning/text events)
/// and falls back to plain text with <think>...</think> extraction.
fn parse_cli_output(cleaned: &str) -> (String, Option<String>, Option<String>) {
    let lines: Vec<&str> = cleaned.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return (String::new(), None, None);
    }
    
    // 1. Try JSONL parsing through the shared tolerant parser so Claude,
    //    Gemini, and OpenCode event shapes all end up in the same model.
    let all_json = lines
        .iter()
        .all(|line| serde_json::from_str::<serde_json::Value>(line.trim()).is_ok());

    if all_json {
        let mut parser = ReasoningParser::new();
        let mut reasoning = String::new();
        let mut texts: Vec<String> = Vec::new();
        let mut error: Option<String> = None;
        for line in &lines {
            for event in parser.parse_line(line) {
                match event {
                    RunEvent::ReasoningDelta { text, .. } => reasoning.push_str(&text),
                    RunEvent::Text { text } => texts.push(text),
                    RunEvent::Error { message, .. } => {
                        if error.is_none() {
                            error = Some(message);
                        }
                    }
                    _ => {}
                }
            }
        }
        for event in parser.finish() {
            if let RunEvent::Text { text } = event {
                texts.push(text);
            }
        }
        let content = texts.join("\n\n");
        let reasoning = if reasoning.trim().is_empty() {
            None
        } else {
            Some(reasoning)
        };
        return (content, reasoning, error);
    }

    // 2. Fallback: Extract <think>...</think> from plain text
    let mut reasoning = None;
    let mut content = cleaned.to_string();

    if let Some(start_idx) = cleaned.find("<think>") {
        if let Some(end_idx) = cleaned.find("</think>") {
            if end_idx > start_idx {
                let think_block = &cleaned[start_idx + 7..end_idx];
                reasoning = Some(think_block.trim().to_string());
                
                let mut clean_content = String::new();
                clean_content.push_str(&cleaned[..start_idx]);
                clean_content.push_str(&cleaned[end_idx + 8..]);
                content = clean_content.trim().to_string();
            }
        }
    }

    // 3. Ollama CLI prints an inline reasoning block:
    //      Thinking...
    //      <reasoning>
    //      ...done thinking.
    //      <answer>
    // Split it so reasoning is returned separately from the answer.
    if let Some(start_idx) = cleaned.find("Thinking...") {
        if let Some(rel_end) = cleaned[start_idx..].find("done thinking") {
            let end_idx = start_idx + rel_end;
            let block = &cleaned[start_idx + "Thinking...".len()..end_idx];
            let block = block
                .trim()
                .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
            if !block.is_empty() {
                reasoning = Some(block.to_string());
            }
            let mut clean_content = String::new();
            clean_content.push_str(&cleaned[..start_idx]);
            let after = &cleaned[end_idx + "done thinking".len()..];
            clean_content.push_str(after.trim_start_matches(['.', ' ', '\r', '\n']));
            content = clean_content.trim().to_string();
        }
    }

    (content, reasoning, None)
}

impl CliProviderImpl {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            id: config.id,
            bin: config.bin,
            models_cmd: config.models_cmd,
            chat_cmd: config.chat_cmd,
            strip_ansi: config.strip_ansi,
            models: config.models,
            is_available: false,
        }
    }

    /// Resolve a configured binary to a runnable path on this platform.
    fn resolve_binary(&self, bin: &str) -> Option<String> {
        if std::path::Path::new(bin).exists() {
            return Some(bin.to_string());
        }

        // Windows: try extensions (prefer .cmd over extensionless).
        if cfg!(target_os = "windows") {
            for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                if bin.to_lowercase().ends_with(ext) {
                    continue;
                }
                let candidate = format!("{}{}", bin, ext);
                if which::which(&candidate).is_ok() {
                    return Some(candidate);
                }
            }
        }

        if which::which(bin).is_ok() {
            return Some(bin.to_string());
        }

        // Fallback search in common install locations (nvm, npm global, ...).
        // The `which` crate can miss binaries that the OS can run (e.g. npm
        // shims installed via nvm), so walk a few well-known directories.
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(home) = std::env::var("HOME") {
            let nvm_root = std::path::PathBuf::from(&home).join(".nvm/versions/node");
            if let Ok(entries) = std::fs::read_dir(&nvm_root) {
                for e in entries.flatten() {
                    candidates.push(e.path().join("bin").join(bin));
                }
            }
            candidates.push(std::path::PathBuf::from(&home).join(".npm-global/bin").join(bin));
        }
        for d in ["/usr/local/bin", "/usr/bin", "/opt/homebrew/bin"] {
            candidates.push(std::path::PathBuf::from(d).join(bin));
        }
        for c in candidates {
            if c.exists() {
                return Some(c.to_string_lossy().to_string());
            }
        }

        // Last resort: assume it's on PATH; spawn will fail if not present.
        Some(bin.to_string())
    }

    /// Build the chat command and the optional prompt that must be written to
    /// stdin. Shared by the aggregated and streaming paths so both behave the
    /// same way.
    fn build_chat_command(
        &self,
        model: &str,
        request: &ChatCompletionRequest,
        workdir: Option<&std::path::Path>,
    ) -> Result<(TokioCommand, Option<String>), anyhow::Error> {
        let cmd_template = match &self.chat_cmd {
            Some(c) => c.clone(),
            None => return Err(anyhow::anyhow!("No chat_cmd defined for provider {}", self.id)),
        };

        // Ollama models may arrive as "ollama/<model>"; strip the prefix so the
        // CLI receives the bare model name (avoids "pull model manifest" errors).
        let model = if self.id == "ollama" {
            model.strip_prefix("ollama/").unwrap_or(model)
        } else if self.id == "opencode" {
            // "opencode/9router/free-forever" -> "9router/free-forever", but keep
            // "opencode/beam-free" as-is (rest has no '/').
            if let Some(rest) = model.strip_prefix("opencode/") {
                if rest.contains('/') { rest } else { model }
            } else {
                model
            }
        } else {
            model
        };

        // Build prompt from messages.
        // Default strip client system prompt for isolation (consistent with bridge/native paths).
        let prompt = request
            .messages
            .iter()
            .filter(|msg| msg.role != crate::types::MessageRole::System)
            .filter_map(|msg| msg.content.clone())
            .collect::<Vec<String>>()
            .join("\n");

        let use_stdin = cmd_template.contains(&"--prompt-stdin".to_string())
            || !cmd_template.iter().any(|arg| arg.contains("{prompt}"))
            || prompt.len() > 2000; // Avoid Windows CLI argument limits.

        // Replace placeholders {model} and {prompt}; the --prompt-stdin marker
        // is a config-level directive, not an argument for the CLI.
        let cmd_args: Vec<String> = cmd_template
            .into_iter()
            .filter(|arg| arg != "--prompt-stdin")
            .map(|arg| {
                let mut a = arg.replace("{model}", model);
                if use_stdin {
                    a = a.replace("{prompt}", "");
                } else {
                    a = a.replace("{prompt}", &prompt);
                }
                a
            })
            .collect();

        let bin = self
            .bin
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No binary configured for provider {}", self.id))?;
        let resolved = self
            .resolve_binary(bin)
            .ok_or_else(|| anyhow::anyhow!("Binary not found for provider {}", self.id))?;

        let mut cmd = if resolved.contains(".cmd")
            || resolved.contains(".bat")
            || resolved.contains(".ps1")
        {
            let mut c = TokioCommand::new("cmd");
            c.arg("/C").arg(&resolved);
            c
        } else {
            TokioCommand::new(&resolved)
        };

        // Set working directory to empty session workdir (spec §11.3)
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        // NOTE: opencode 2.x ignores OPENCODE_CONFIG_CONTENT and reads project
        // config from opencode.json in the working directory. However, writing a
        // disable-tools/permission config has been observed to trigger
        // provider.free-limit on some free models (e.g. opencode/iceman-free),
        // while the same run without opencode.json succeeds. To avoid breaking
        // chat-only/native runs, do not inject a config file here; isolation
        // relies on the empty session workdir. Bridge (MCP) sessions that must
        // avoid E2BIG still write opencode.json in the bridge provider.

        // Do not force --standalone: it starts a private server that lacks the
        // shared background session/auth and (on free models) returns 429, which
        // surfaces as a "gantung"/zombie turn. Rely on the default CLI path,
        // which exits cleanly after the turn.
        let final_args = cmd_args.clone();

        cmd.args(&final_args)
            .stdin(if use_stdin {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        Ok((cmd, if use_stdin { Some(prompt) } else { None }))
    }

    /// Execute command with platform-aware binary resolution
    fn execute_cmd_inner(&self, bin: &str, args: &[String]) -> Result<std::process::Output, anyhow::Error> {
        // Try direct path
        if std::path::Path::new(bin).exists() {
            tracing::debug!("Executing via direct path: {}", bin);
            return Ok(Command::new(bin)
                .args(args)
                .output()?);
        }

        // Windows: try extensions FIRST (prefer .cmd over extensionless)
        if cfg!(target_os = "windows") {
            for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                if bin.to_lowercase().ends_with(ext) {
                    continue;
                }
                let candidate = format!("{}{}", bin, ext);
                if which::which(&candidate).is_ok() {
                    tracing::debug!("Executing via PATH with ext: {}", candidate);
                    // .cmd/.bat/.ps1 files need shell execution on Windows
                    if ext == &".cmd" || ext == &".bat" || ext == &".ps1" {
                        return Ok(Command::new("cmd")
                            .args(["/C"])
                            .arg(&candidate)
                            .args(args)
                            .output()?);
                    }
                    return Ok(Command::new(&candidate)
                        .args(args)
                        .output()?);
                }
            }
        }

        // Try PATH search (for non-Windows or if no extension found)
        if which::which(bin).is_ok() {
            tracing::debug!("Executing via PATH: {}", bin);
            return Ok(Command::new(bin)
                .args(args)
                .output()?);
        }

        // Windows: try extensions again as fallback (redundant but safe)
        if cfg!(target_os = "windows") {
            for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                if bin.to_lowercase().ends_with(ext) {
                    continue;
                }
                let candidate = format!("{}{}", bin, ext);
                if which::which(&candidate).is_ok() {
                    tracing::debug!("Executing via PATH with ext (fallback): {}", candidate);
                    if ext == &".cmd" || ext == &".bat" || ext == &".ps1" {
                        return Ok(Command::new("cmd")
                            .args(["/C"])
                            .arg(&candidate)
                            .args(args)
                            .output()?);
                    }
                    return Ok(Command::new(&candidate)
                        .args(args)
                        .output()?);
                }
            }
        }

        Err(anyhow::anyhow!("Binary not found: {}", bin))
    }

    /// Parse model list from command output
    /// Handles JSON array output, JSON object output, newline-separated list, and CSV
    pub fn parse_models_output(&self, output: &str) -> Vec<String> {
        let cleaned = if self.strip_ansi {
            String::from_utf8_lossy(&strip_ansi_escapes::strip(output)).to_string()
        } else {
            output.to_string()
        };

        let trimmed = cleaned.trim();

        // Try JSON array format: ["model1", "model2"]
        if trimmed.starts_with('[') {
            if let Ok(parsed) = serde_json::from_str::<Vec<String>>(trimmed) {
                return parsed;
            }
            // Try JSON object format: {"models": ["model1", "model2"]}
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(arr) = parsed.get("models").and_then(|v| v.as_array()) {
                    return arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                }
            }
        }

        // Try JSON object format: {"models": ["model1", "model2"]}
        if trimmed.starts_with('{') {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(arr) = parsed.get("models").and_then(|v| v.as_array()) {
                    return arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                }
            }
        }

        // Provider-specific parsing
        // opencode: single-line space-separated -> all tokens are models
        // ollama: tabular (NAME ID SIZE MODIFIED) -> only first column (NAME)
        // gemini/others: fallback to first column per line
        let is_opencode = self.id == "opencode";

        // Newline-separated list
        // Table format (e.g., Ollama list) or newline-separated list
        // Skip header starting with "NAME"
        let mut lines = cleaned.lines();
        let first = lines.next();
        let has_header = if let Some(l) = first {
            l.trim_start().starts_with("NAME")
        } else {
            false
        };
        let lines_iter = if has_header { lines } else { cleaned.lines() };
        let models: Vec<String> = lines_iter
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                if is_opencode {
                    // opencode: space-separated on one line -> all tokens
                    line.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>()
                } else {
                    // ollama/gemini/others: tabular -> only first column
                    line.split_whitespace().next().map(|s| s.to_string()).into_iter().collect::<Vec<_>>()
                }
            })
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        if !models.is_empty() {
            return models;
        }
        // Fallback: newline-separated list
        cleaned
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Static version of execute_cmd_inner for use in blocking tasks
fn execute_cmd_static(bin: &str, args: &[String]) -> Result<std::process::Output, anyhow::Error> {
    // Try direct path
    if std::path::Path::new(bin).exists() {
        tracing::debug!("Executing via direct path: {}", bin);
        return Ok(Command::new(bin)
            .args(args)
            .output()?);
    }

    // Windows: try extensions FIRST (prefer .cmd over extensionless)
    if cfg!(target_os = "windows") {
        for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
            if bin.to_lowercase().ends_with(ext) {
                continue;
            }
            let candidate = format!("{}{}", bin, ext);
            if which::which(&candidate).is_ok() {
                tracing::debug!("Executing via PATH with ext: {}", candidate);
                // .cmd/.bat/.ps1 files need shell execution on Windows
                if ext == &".cmd" || ext == &".bat" || ext == &".ps1" {
                    return Ok(Command::new("cmd")
                        .args(["/C"])
                        .arg(&candidate)
                        .args(args)
                        .output()?);
                }
                return Ok(Command::new(&candidate)
                    .args(args)
                    .output()?);
            }
        }
    }

    // Try PATH search (for non-Windows or if no extension found)
    if which::which(bin).is_ok() {
        tracing::debug!("Executing via PATH: {}", bin);
        return Ok(Command::new(bin)
            .args(args)
            .output()?);
    }

    // Windows: try extensions again as fallback (redundant but safe)
    if cfg!(target_os = "windows") {
        for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
            if bin.to_lowercase().ends_with(ext) {
                continue;
            }
            let candidate = format!("{}{}", bin, ext);
            if which::which(&candidate).is_ok() {
                tracing::debug!("Executing via PATH with ext (fallback): {}", candidate);
                if ext == &".cmd" || ext == &".bat" || ext == &".ps1" {
                    return Ok(Command::new("cmd")
                        .args(["/C"])
                        .arg(&candidate)
                        .args(args)
                        .output()?);
                }
                return Ok(Command::new(&candidate)
                    .args(args)
                    .output()?);
            }
        }
    }

    Err(anyhow::anyhow!("Binary not found: {}", bin))
}

/// Static version of parse_models_output for use in blocking tasks
fn parse_models_output_static(output: &str, provider_id: &str) -> Vec<String> {
    let cleaned = String::from_utf8_lossy(&strip_ansi_escapes::strip(output)).to_string();
    let trimmed = cleaned.trim();

    // Try JSON array format: ["model1", "model2"]
    if trimmed.starts_with('[') {
        if let Ok(parsed) = serde_json::from_str::<Vec<String>>(trimmed) {
            return parsed;
        }
        // Try JSON object format: {"models": ["model1", "model2"]}
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(arr) = parsed.get("models").and_then(|v| v.as_array()) {
                return arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            }
        }
    }

    // Try JSON object format: {"models": ["model1", "model2"]}
    if trimmed.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(arr) = parsed.get("models").and_then(|v| v.as_array()) {
                return arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
            }
        }
    }

    // Provider-specific parsing
    // opencode: single-line space-separated -> all tokens are models
    // ollama: tabular (NAME ID SIZE MODIFIED) -> only first column (NAME)
    // gemini/others: fallback to first column per line
    let is_opencode = provider_id == "opencode";

    // Newline-separated list
    // Table format (e.g., Ollama list) or newline-separated list
    // Skip header starting with "NAME"
    let mut lines = cleaned.lines();
    let first = lines.next();
    let has_header = if let Some(l) = first {
        l.trim_start().starts_with("NAME")
    } else {
        false
    };
    let lines_iter = if has_header { lines } else { cleaned.lines() };
    let models: Vec<String> = lines_iter
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if is_opencode {
                // opencode: space-separated on one line -> all tokens
                line.split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>()
            } else {
                // ollama/gemini/others: tabular -> only first column
                line.split_whitespace().next().map(|s| s.to_string()).into_iter().collect::<Vec<_>>()
            }
        })
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    if !models.is_empty() {
        return models;
    }
    // Fallback: newline-separated list
    cleaned
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Strip all ANSI escape codes from text
pub fn strip_ansi_codes(text: &str) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes::strip(text)).to_string()
}

pub mod ollama_native;
pub mod bridge;
pub mod ollama_cli;

use crate::provider::bridge::{BridgeConfig, create_bridge_factory_v2};
use crate::bridge::BridgeClient;

/// Maximum time to wait for a CLI turn before killing the bridge session.
/// Default: 120s (reduced from 300s to fail faster on free-tier quota issues).
/// Override with `AGENTGATE_BRIDGE_TIMEOUT_SECS`.
pub fn bridge_turn_timeout() -> std::time::Duration {
    let secs = std::env::var("AGENTGATE_BRIDGE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120u64);
    std::time::Duration::from_secs(secs)
}

/// Render an OpenAI conversation into a single prompt for CLI agents.
///
/// Unlike a naive `content.join("\n")`, this keeps system instructions,
/// assistant tool calls, and `role:"tool"` results so a resumed/replayed run
/// can continue correctly (spec §7.5, §11.2).
pub fn render_conversation(
    messages: &[crate::types::ChatMessage],
    max_prompt_chars: Option<usize>,
    max_result_bytes: Option<usize>,
) -> String {
    use crate::types::MessageRole;

    // One cumulative budget for the WHOLE rendered prompt. Previously only
    // user/assistant *content* was charged; tool-call arguments (110 KB in the
    // field) and every tool result (up to 256 KB each) were appended verbatim,
    // so replay prompts grew without bound until the 120 s turn timeout.
    let budget = max_prompt_chars.unwrap_or(64_000);
    // A single giant result must not crowd out the rest of the conversation.
    let per_part = max_result_bytes.unwrap_or(262_144).min(16_384);

    let blocks: Vec<String> = messages
        .iter()
        .filter_map(|msg| {
            let block = match msg.role {
                // FIX-PROMPT-01: strip system messages by default.
                MessageRole::System => return None,
                MessageRole::User => {
                    let content = msg.content.as_deref()?.trim();
                    if content.is_empty() {
                        return None;
                    }
                    format!("[user]\n{}", content)
                }
                MessageRole::Assistant => {
                    let mut parts: Vec<String> = Vec::new();
                    if let Some(calls) = msg.tool_calls.as_deref() {
                        for call in calls {
                            parts.push(format!(
                                "[assistant tool_call id={} name={}]\n{}",
                                call.id,
                                call.function.name,
                                clip(&call.function.arguments, per_part)
                            ));
                        }
                    }
                    if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                        parts.push(format!("[assistant]\n{}", content.trim()));
                    }
                    if parts.is_empty() {
                        return None;
                    }
                    parts.join("\n")
                }
                MessageRole::Tool => {
                    let content = clip(msg.content.as_deref().unwrap_or(""), per_part);
                    format!(
                        "[tool result id={} name={}]\n{}",
                        msg.tool_call_id.as_deref().unwrap_or(""),
                        msg.name.as_deref().unwrap_or(""),
                        content
                    )
                }
            };
            Some(block)
        })
        .collect();

    // Walk from newest to oldest so the most recent context always survives a
    // truncation (the old code broke out of a forward loop and dropped the
    // tail, i.e. exactly the messages that mattered).
    let mut start = blocks.len();
    let mut used = 0usize;
    for (i, block) in blocks.iter().enumerate().rev() {
        let cost = block.len() + 1; // +1 for the joining newline
        if used + cost <= budget {
            used += cost;
            start = i;
        } else {
            break;
        }
    }

    let mut out = String::with_capacity(used + 128);
    if start > 0 {
        out.push_str(&format!(
            "[... {} earlier messages truncated to keep prompt under {} bytes]\n\n",
            start, budget
        ));
    }
    if start < blocks.len() {
        // A single newest block can still exceed the budget on its own.
        let mut first = blocks[start].clone();
        if first.len() > budget {
            first = format!("{}...[truncated]", clip(&first, budget.saturating_sub(16)));
        }
        out.push_str(&first);
        for block in &blocks[start + 1..] {
            out.push('\n');
            out.push_str(block);
        }
    }
    out
}

/// Truncate `content` to at most `limit` bytes without splitting a UTF-8 char.
fn clip(content: &str, limit: usize) -> &str {
    if content.len() <= limit {
        return content;
    }
    let mut end = limit.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

/// Prompt instructions for CLIs/models without native tool calling
/// (`prompt_tools_fallback = true`, spec_ollama-cli.md).
fn tool_prompt_instructions(tools: &[crate::types::ToolDefinition]) -> String {
    let mut out = String::from("\n\n[available tools]\n");
    for tool in tools {
        out.push_str(&format!(
            "- {}: {}\n  parameters: {}\n",
            tool.function.name,
            tool.function.description.clone().unwrap_or_default(),
            tool.function
                .parameters
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}))
        ));
    }
    out.push_str(
        "\nIf you need to call a tool, reply ONLY with a JSON object like:\n\
         {\"tool_calls\":[{\"name\":\"tool_name\",\"arguments\":{...}}]}\n",
    );
    out
}

/// A CLI provider that uses the bridge architecture (MCP bridge to Device A).
/// Used for OpenCode, Gemini, and Claude CLI agents per spec_tooling.md.
/// A cached provider session with its creation time, so keep_alive sessions
/// can be expired instead of accumulating live CLI child processes forever.
struct PersistentSession {
    session: Arc<dyn crate::provider::bridge::BridgeSession + Send + Sync>,
    created_at: std::time::Instant,
}

/// Maximum number of persistent provider sessions kept alive. Beyond this,
/// the oldest sessions are closed to bound memory / child-process count.
/// Override with AGENTGATE_MAX_PERSISTENT_SESSIONS for constrained nodes.
fn max_persistent_sessions() -> usize {
    std::env::var("AGENTGATE_MAX_PERSISTENT_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(4)
}

/// True when the error is an OS broken pipe (a CLI child's stdin closed).
fn is_pipe_broken(err: &anyhow::Error) -> bool {
    matches!(
        crate::transport::classify_message(&err.to_string()),
        crate::transport::TransportErrorClass::BrokenPipe
    )
}

pub struct BridgeCliProvider {
    pub id: String,
    pub bin: Option<String>,
    pub models: Vec<String>,
    pub is_available: bool,
    factory: Box<dyn crate::provider::bridge::BridgeProviderFactory + Send + Sync>,
    /// Bridge server used to register MCP sessions for each run (spec §9.2).
    bridge: Option<Arc<crate::bridge::BridgeServer>>,
    keep_alive: Option<String>,
    prompt_tools_fallback: bool,
    /// Transport type: "http" (default) or "stdio_shim"
    transport: Option<String>,
    /// Isolation config for selective tool disabling (TB-TOOL-02)
    isolation_config: crate::config::IsolationConfig,
    /// Merge host opencode.json MCP servers into injected bridge config.
    passthrough_host_mcp: bool,
    passthrough_mcp_allowlist: Vec<String>,
    passthrough_mcp_denylist: Vec<String>,
    allow_client_mcp: bool,
    /// Tool injection limits (QUOTA-01)
    limits: crate::config::LimitsConfig,
    /// Cache of persistent sessions (keyed by session_id) for keep_alive mode.
    /// When keep_alive is enabled, sessions are stored here instead of being closed.
    /// Using Arc to allow cloning and sharing sessions across requests.
    persistent_sessions: Arc<RwLock<HashMap<String, PersistentSession>>>,
    /// Parsed keep_alive duration (default 600s when config uses a boolean).
    keep_alive_ttl: std::time::Duration,
    /// Cache of OpenCode CLI conversation session ids (e.g. `ses_...`) captured
    /// from CLI events, keyed by the agentgate session id. Used to resume the
    /// same CLI conversation via `--session` on the next spawn (cross-turn
    /// session reuse for keep_alive flows).
    persistent_opencode_sessions: Arc<RwLock<HashMap<String, String>>>,
    /// Limits for prompt rendering
    max_prompt_chars: usize,
    max_result_bytes: usize,
}

/// Parse a keep_alive value into a duration. Accepts "10m", "600s", "600",
/// or the legacy booleans "true"/"1" (default 10 minutes).
fn parse_keep_alive_ttl(value: Option<&str>) -> std::time::Duration {
    let default = std::time::Duration::from_secs(600);
    let Some(raw) = value else { return default };
    let v = raw.trim().to_ascii_lowercase();
    if v.is_empty() || matches!(v.as_str(), "true" | "1" | "yes" | "on") {
        return default;
    }
    let (num, unit) = if let Some(n) = v.strip_suffix('m') {
        (n, 60u64)
    } else if let Some(n) = v.strip_suffix('s') {
        (n, 1u64)
    } else {
        (v.as_str(), 1u64)
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|n| std::time::Duration::from_secs(n.saturating_mul(unit)))
        .unwrap_or(default)
}

impl BridgeCliProvider {
    /// Cache a persistent session, first evicting sessions older than
    /// `keep_alive_ttl` and then the oldest ones beyond `MAX_PERSISTENT_SESSIONS`.
    /// This bounds live CLI child processes / memory under sustained load.
    async fn store_persistent_session(
        &self,
        id: String,
        session: Arc<dyn crate::provider::bridge::BridgeSession + Send + Sync>,
    ) {
        let mut sessions = self.persistent_sessions.write().await;
        let now = std::time::Instant::now();
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, p)| now.duration_since(p.created_at) >= self.keep_alive_ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            if let Some(old) = sessions.remove(&key) {
                tracing::info!(target: "bridge.session", session = %key, "evicting expired keep_alive session");
                let _ = old.session.close().await;
            }
        }
        while sessions.len() >= max_persistent_sessions() {
            let oldest = sessions
                .iter()
                .min_by_key(|(_, p)| p.created_at)
                .map(|(k, _)| k.clone());
            let Some(key) = oldest else { break };
            if let Some(old) = sessions.remove(&key) {
                tracing::info!(target: "bridge.session", session = %key, "evicting LRU keep_alive session (cap reached)");
                let _ = old.session.close().await;
            }
        }
        sessions.insert(id, PersistentSession { session, created_at: now });
    }

    /// Sweep expired keep_alive sessions. Called periodically and on store.
    async fn sweep_expired_persistent_sessions(&self) {
        let mut sessions = self.persistent_sessions.write().await;
        let now = std::time::Instant::now();
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, p)| now.duration_since(p.created_at) >= self.keep_alive_ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            if let Some(old) = sessions.remove(&key) {
                tracing::info!(target: "bridge.session", session = %key, "sweeping expired keep_alive session");
                let _ = old.session.close().await;
            }
        }
    }

    fn spawn_persistent_session_sweeper(
        persistent_sessions: std::sync::Weak<RwLock<HashMap<String, PersistentSession>>>,
        keep_alive_ttl: std::time::Duration,
    ) {
        let interval_secs = std::cmp::max(30, (keep_alive_ttl.as_secs() / 2).max(30));
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
                loop {
                    ticker.tick().await;
                    let Some(sessions_arc) = persistent_sessions.upgrade() else { break; };
                    let now = std::time::Instant::now();
                    let mut sessions = sessions_arc.write().await;
                    let expired: Vec<String> = sessions
                        .iter()
                        .filter(|(_, p)| now.duration_since(p.created_at) >= keep_alive_ttl)
                        .map(|(k, _)| k.clone())
                        .collect();
                    for key in expired {
                        if let Some(old) = sessions.remove(&key) {
                            tracing::info!(target: "bridge.session", session = %key, "sweeping expired keep_alive session (bg)");
                            let _ = old.session.close().await;
                        }
                    }
                }
            });
        }
    }

    pub fn new(
        config: crate::config::ProviderConfig,
        bridge_config: Option<&crate::config::BridgeConfig>,
        limits: Option<&crate::config::LimitsConfig>,
    ) -> Option<Self> {
        Self::with_bridge(config, None, bridge_config, limits)
    }

    /// Create a bridge provider bound to the shared `BridgeServer` so each run
    /// registers its MCP session (id + token) before spawning the CLI.
    pub fn with_bridge(
        config: crate::config::ProviderConfig,
        bridge: Option<Arc<crate::bridge::BridgeServer>>,
        bridge_config: Option<&crate::config::BridgeConfig>,
        limits: Option<&crate::config::LimitsConfig>,
    ) -> Option<Self> {
        // Determine which bridge factory to use based on provider ID
        let factory = create_bridge_factory_v2(&config.id)?;

        // Get isolation config from bridge config, defaulting to default IsolationConfig
        let isolation_config = bridge_config
            .map(|bc| bc.isolation.clone())
            .unwrap_or_default();
        let passthrough_host_mcp = bridge_config.map(|bc| bc.passthrough_host_mcp).unwrap_or(false);
        let passthrough_mcp_allowlist = bridge_config.map(|bc| bc.passthrough_mcp_allowlist.clone()).unwrap_or_default();
        let passthrough_mcp_denylist = bridge_config.map(|bc| bc.passthrough_mcp_denylist.clone()).unwrap_or_default();
        let allow_client_mcp = bridge_config.map(|bc| bc.allow_client_mcp).unwrap_or(false);

        // Get limits config, defaulting to default LimitsConfig
        let limits = limits.cloned().unwrap_or_default();
        let max_prompt_chars = limits.max_prompt_chars;
        let max_result_bytes = limits.max_result_bytes;

        let keep_alive_ttl = parse_keep_alive_ttl(config.keep_alive.as_deref());
        let persistent_sessions = Arc::new(RwLock::new(HashMap::new()));
        if config.keep_alive.is_some() {
            Self::spawn_persistent_session_sweeper(Arc::downgrade(&persistent_sessions), keep_alive_ttl);
        }
        Some(Self {
            id: config.id,
            bin: config.bin.clone(),
            models: config.models.clone(),
            is_available: false,
            factory,
            bridge,
            keep_alive: config.keep_alive.clone(),
            keep_alive_ttl,
            prompt_tools_fallback: config.prompt_tools_fallback,
            transport: config.transport.clone(),
            isolation_config,
            passthrough_host_mcp,
            passthrough_mcp_allowlist,
            passthrough_mcp_denylist,
            allow_client_mcp,
            limits,
            persistent_sessions,
            persistent_opencode_sessions: Arc::new(RwLock::new(HashMap::new())),
            max_prompt_chars,
            max_result_bytes,
        })
    }

    /// Register an MCP session for the given tools. Returns `None` when no
    /// bridge server is attached (e.g. unit tests / standalone usage).
    pub async fn register_session(
        &self,
        tools: Vec<crate::bridge::ToolSchema>,
    ) -> Option<(String, String)> {
        match &self.bridge {
            Some(bridge) => {
                let n_tools = tools.len();
                let (id, token) = bridge.create_session(tools, None).await;
                tracing::info!(event = "bridge.tools_list", session = %id, n_tools = n_tools, provider = %self.id);
                Some((id, token))
            }
            None => None,
        }
    }

    /// Look up a previously captured OpenCode CLI conversation session id
    /// (e.g. `ses_...`) for an agentgate session key. Used to resume the same
    /// CLI conversation via `--session` on the next spawn (cross-turn reuse).
    async fn stored_opencode_session(&self, agentgate_session_id: &str) -> Option<String> {
        self.persistent_opencode_sessions
            .read()
            .await
            .get(agentgate_session_id)
            .cloned()
    }

    /// Remember (or clear) the captured OpenCode CLI conversation session id for
    /// an agentgate session key after a turn completes.
    async fn remember_opencode_session(&self, agentgate_session_id: &str, opencode_session_id: Option<String>) {
        match opencode_session_id {
            Some(sid) if !sid.trim().is_empty() => {
                self.persistent_opencode_sessions
                    .write()
                    .await
                    .insert(agentgate_session_id.to_string(), sid);
            }
            _ => {
                self.persistent_opencode_sessions
                    .write()
                    .await
                    .remove(agentgate_session_id);
            }
        }
    }

    /// Check if keep_alive (persistent session) is enabled for this provider.
    pub fn is_keep_alive(&self) -> bool {
        // FIX-SESSION: `keep_alive` is a duration ("10m") or a boolean flag.
        // Any non-empty, non-false value means persistent sessions are enabled.
        match &self.keep_alive {
            Some(val) => {
                let v = val.trim().to_ascii_lowercase();
                !v.is_empty() && v != "false" && v != "0" && v != "off" && v != "none"
            }
            None => false,
        }
    }

    /// Build the MCP tool schemas for this request.
    /// Applies OpenAI-safe name aliasing, `$ref` inlining, and schema
    /// normalization (spec §8) before the schemas are handed to the bridge.
    pub fn tool_schemas_from_request(
        &self,
        request: &crate::types::ChatCompletionRequest,
    ) -> Vec<crate::bridge::ToolSchema> {
        use crate::openai::tools::{
            inline_refs, normalize_schema, openai_name_alias, SchemaProfile,
        };

        let profile = match self.id.as_str() {
            "gemini" => SchemaProfile::StrictGemini,
            _ => SchemaProfile::OpenAI,
        };

        request
            .tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .enumerate()
                    .map(|(index, t)| {
                        let raw_name = openai_name_alias(&t.function.name, index);
                        // Normalize client tool names to opencode canonical names
                        let name = crate::openai::tools::canonical_tool_name(&raw_name);
                        let description = t.function.description.clone().unwrap_or_default();
                        let parameters = t
                            .function
                            .parameters
                            .clone()
                            .map(inline_refs)
                            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                        let schema = crate::bridge::ToolSchema {
                            name,
                            description,
                            parameters,
                            provider: self.id.clone(),
                            workspace: "default".to_string(),
                        };
                        normalize_schema(&schema, profile)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn release_session(&self, session_id: &Option<String>) {
        if let (Some(bridge), Some(id)) = (&self.bridge, session_id) {
            bridge.remove_session(id).await;
        }
    }

    /// Drop a cached CLI session whose child pipe has already closed. The
    /// caller's transient retry then spawns a fresh process instead of writing
    /// into a dead pipe and surfacing `Broken pipe (os error 32)` to the client.
    async fn retire_broken_session(
        &self,
        session: &Arc<dyn crate::provider::bridge::BridgeSession>,
        session_id: &str,
        mcp_session_id: &Option<String>,
    ) {
        self.persistent_sessions.write().await.remove(session_id);
        let _ = session.close().await;
        self.release_session(mcp_session_id).await;
    }
}

/// Select the CLI provider implementation for a configured provider.
/// Bridge-capable agents (`opencode`, `gemini`, `claude`) use the MCP bridge
/// adapter unless `bridge = false` is set in config (spec §6.2, §12).
pub fn build_cli_provider(
    config: &crate::config::ProviderConfig,
    bridge: Option<Arc<crate::bridge::BridgeServer>>,
    bridge_config: Option<&crate::config::BridgeConfig>,
    limits: Option<&crate::config::LimitsConfig>,
) -> Box<dyn CliProvider + Send + Sync> {
    // Ollama uses HTTP native by default; only `ollama_mode = "cli"` opts into
    // the bridge adapter (spec_ollama-cli.md) which uses `ollama run` via MCP bridge.
    let wants_cli = if config.id == "ollama" {
        config.ollama_mode.as_deref() == Some("cli")
    } else {
        config.bridge
    };

    if wants_cli {
        let mut cfg = config.clone();
        if cfg.id == "ollama" && cfg.bin.is_none() {
            cfg.bin = Some("ollama".to_string());
        }
        if let Some(bp) = BridgeCliProvider::with_bridge(cfg, bridge, bridge_config, limits) {
            return Box::new(bp);
        }
    }
    Box::new(CliProviderImpl::new(config.clone()))
}

#[async_trait]
impl CliProvider for BridgeCliProvider {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn is_bridge(&self) -> bool {
        true
    }

    fn bin(&self) -> Option<String> {
        self.bin.clone()
    }

    fn models(&self) -> Vec<String> {
        self.models.clone()
    }

    fn is_available(&self) -> bool {
        self.is_available
    }

    fn set_available(&mut self, available: bool) {
        self.is_available = available;
    }

    async fn check_available(&self) -> bool {
        // For bridge providers, check if the CLI binary is available
        let bin = match &self.bin {
            Some(b) => b,
            None => return false,
        };

        // Resolve to a runnable path first (handles Windows PATHEXT).
        let mut cmd = std::process::Command::new(bin);
        cmd.arg("--version");
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        match cmd.output() {
            Ok(out) => {
                tracing::info!(provider = %self.id, bin = %bin, status = %out.status, "check_available: resolved");
                true
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    tracing::warn!(provider = %self.id, bin = %bin, "check_available: NOT found on PATH");
                    false
                } else {
                    tracing::warn!(provider = %self.id, bin = %bin, error = %e, "check_available: spawn error, assuming present");
                    true
                }
            }
        }
    }

    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        // For bridge providers, return configured models (CLI agents don't typically have model listing)
        Ok(self.models.clone())
    }

    async fn send_chat_completion(
        &self,
        model: &str,
        request: crate::types::ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        use uuid::Uuid;
        // Ollama models may arrive as "ollama/<model>".
        let model = model.strip_prefix("ollama/").unwrap_or(model);

        // Extract or generate session_id from request
        let session_id = request.session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
        
        // Check if we have a persistent session for this session_id
        let keep_alive = self.keep_alive.is_some();
        let existing_session = if keep_alive {
            let sessions = self.persistent_sessions.read().await;
            sessions.get(&session_id).map(|p| p.session.clone())
        } else {
            None
        };

        let (session, should_store, mcp_session_id): (
            Arc<dyn crate::provider::bridge::BridgeSession>,
            bool,
            Option<String>,
        ) = if let Some(existing) = existing_session {
            // Reuse existing persistent session
            tracing::info!(target: "bridge.cli", session_id = %session_id, "Reusing persistent session");
            (existing, false, None)
        } else {
            // Register the MCP session before spawning so /_bridge/mcp/{id} is known
            // to the bridge server (spec §9.2).
            let mut tool_schemas = self.tool_schemas_from_request(&request);
            // Inject agentgate built-in discovery tools (check_mcp_local, skill_local)
            // so the model can inspect local MCP servers and skills.
            tool_schemas.extend(crate::bridge::agentgate_builtin_tools());
            // Determine max_inject based on model type: free models (beam-free, etc.) get limited injection
              // to avoid provider.free-limit errors, while paid models get full tool access.
              let is_free_model = model.to_lowercase().ends_with("-free");
              let max_inject = if is_free_model { self.limits.max_inject } else { self.limits.max_tools };
            let tool_schemas_for_bridge: Vec<_> = if tool_schemas.len() > max_inject {
                tracing::warn!(model = %model, total = tool_schemas.len(), max = max_inject, "Truncating injected tools");
                tool_schemas[..max_inject].to_vec()
            } else {
                tool_schemas.clone()
            };
            // FIX-SESSION-TOOLS: register the same truncated list that is handed
            // to the shim/config; registering the full 91-tool list made
            // preflight see client tools like `Read` and (before the name fix)
            // reject every tool call.
            let (mcp_session_id, session_token) = match self.register_session(tool_schemas_for_bridge.clone()).await {
                Some((id, token)) => (Some(id), Some(token)),
                None => (None, None),
            };
            let local_session_id = mcp_session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
            let data_dir = std::env::temp_dir().join("agentgate");
            let workdir = data_dir.join("sessions").join(&local_session_id).join("work");
            tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

            let bridge_port: u16 = std::env::var("AGENTGATE_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8786);
            tracing::info!(target: "bridge.cli", bridge_port = bridge_port, "Using bridge port");

            // FIX-SESSION: resume the captured OpenCode CLI conversation session
            // (e.g. `ses_...`) when one was remembered for this agentgate session.
            // On model reroute/quota failover, force a fresh CLI session to avoid model/context mismatch.
            let force_fresh_cli = request.agentgate_force_fresh_cli_session;
            if force_fresh_cli {
                self.remember_opencode_session(&session_id, None).await;
                clear_cli_session_for_chain(&session_id).await;
                tracing::info!(event="bridge.cli_session.force_fresh", session_id=%session_id, model=%model, "Forced fresh CLI session (model reroute/quota failover)");
            }
            let resume_cli_sid = if force_fresh_cli {
                None
            } else {
                self.stored_opencode_session(&session_id).await
            };

            // FIX-PROMPT-01: Default strip system prompt — do not forward to CLI
            // System messages are skipped in render_conversation (see function)
            let bridge_config = BridgeConfig {
                host: "127.0.0.1".to_string(),
                port: bridge_port,
                model: model.to_string(),
                profile_name: self.factory.default_profile().name.clone(),
                data_dir: Some(data_dir.clone()),
                session_id: mcp_session_id.clone(),
                session_token,
                keep_alive: self.keep_alive.clone(),
                prompt_tools_fallback: self.prompt_tools_fallback,
                transport: self.transport.clone(),
                tools: tool_schemas_for_bridge,
                isolation: Some(self.isolation_config.clone()),
                cli_session_id: resume_cli_sid.clone(),
                passthrough_host_mcp: self.passthrough_host_mcp,
                passthrough_mcp_allowlist: self.passthrough_mcp_allowlist.clone(),
                passthrough_mcp_denylist: self.passthrough_mcp_denylist.clone(),
                allow_client_mcp: self.allow_client_mcp,
                client_mcp_servers: request.mcp_servers.clone(),
            };

            tracing::info!(target: "bridge.cli", model = %model, "Creating bridge provider");
            // Create bridge provider from factory
            let provider = self.factory.create(bridge_config.clone()).await?;

            // Create bridge client with bridge reference for PR-S4 continuation
            let bridge = self.bridge.as_ref().ok_or_else(|| anyhow!("bridge required for bridge provider"))?.clone();
            let client = Arc::new(BridgeClient::new_with_bridge(
                "127.0.0.1".to_string(),
                bridge_port,
                bridge,
            ));

            // Start bridge session (spawns CLI with MCP config)
            let session = provider.start_bridge(bridge_config, client).await?;

            // Convert Box to Arc for persistent session storage
            let session: std::sync::Arc<dyn crate::provider::bridge::BridgeSession> = std::sync::Arc::from(session);

            // Register provider session with BridgeServer for cancellation support
            if let Some(ref bridge) = self.bridge {
                if let Some(ref mcp_id) = mcp_session_id {
                    bridge.register_provider_session(mcp_id.clone(), session.clone()).await;
                }
            }

            (session, keep_alive, mcp_session_id)
        };

        // Send the structured conversation to the CLI so system instructions,
        // assistant tool calls, and tool results survive (spec §11.2).
        let mut user_message = render_conversation(
            &request.messages,
            Some(self.max_prompt_chars),
            Some(self.max_result_bytes),
        );
        tracing::info!(
            model = %model,
            messages = request.messages.len(),
            prompt_bytes = user_message.len(),
            budget_bytes = self.max_prompt_chars,
            "rendered prompt for CLI"
        );
        if self.prompt_tools_fallback {
            if let Some(tools) = request.tools.as_deref().filter(|t| !t.is_empty()) {
                user_message.push_str(&tool_prompt_instructions(tools));
            }
        }

        // Preflight isolation check before admitting a tool-capable turn
        // (spec §13.2, fail-closed): refuse the turn when built-in tool
        // isolation could not be verified instead of running unverified tools.
        let isolation_ok = session.preflight_isolation_check().await?;
        if !isolation_ok {
            if !keep_alive {
                let _ = session.close().await;
            }
            if let Some(ref mcp_id) = mcp_session_id {
                self.release_session(&Some(mcp_id.clone())).await;
            }
            return Err(anyhow::anyhow!("tool_isolation_unavailable: isolation preflight failed"));
        }

        if !user_message.trim().is_empty() {
            if let Err(e) = session.send_user_message(&user_message).await {
                if is_pipe_broken(&e) {
                    // Reused CLI child already exited: its stdin pipe is dead.
                    // Drop the cache entry so the caller's transient retry spawns
                    // a fresh child instead of echoing the raw OS error.
                    tracing::warn!(
                        target: "agentgate.provider",
                        provider_id = %self.id,
                        "CLI stdin pipe closed (stale session); dropping session for fresh respawn"
                    );
                    self.retire_broken_session(&session, &session_id, &mcp_session_id).await;
                    return Err(anyhow::anyhow!(
                        "provider_error: broken pipe — CLI stdin closed; stale session dropped"
                    ));
                }
                return Err(e);
            }
        }

        // Wait for completion (handles tool calls via bridge)
        let output = match tokio::time::timeout(bridge_turn_timeout(), session.wait_for_completion()).await {
            Ok(result) => result?,
            Err(_) => {
                if !keep_alive {
                    let _ = session.close().await;
                }
                if let Some(ref mcp_id) = mcp_session_id {
                    self.release_session(&Some(mcp_id.clone())).await;
                }
                return Err(anyhow::anyhow!("provider_error: bridge turn timed out"));
            }
        };

        // FIX-SESSION: capture the CLI conversation session id (e.g. `ses_...`)
        // reported by the adapter. Only for keep_alive flows (fresh sessions are transient).
        let captured_cli_sid = session.cli_session_id();
        if keep_alive {
            self.remember_opencode_session(&session_id, captured_cli_sid.clone()).await;
        }

        // NEW: store CLI conversation session id in the global chain map keyed
        // by the request's agentgate session_id (used to pass --session to the
        // CLI on subsequent turns of the same chain).
        let chain_agentgate_sid = request.session_id.clone().unwrap_or_else(|| session_id.clone());
        register_pending_cli_session(&chain_agentgate_sid, captured_cli_sid.clone()).await;
        if let Some(sid) = captured_cli_sid.as_ref() {
            tracing::info!(event = "bridge.cli_session.captured", provider_id = %self.id,
                 session_id = %session_id, cli_session_id = %sid,
                 tool_calls = output.tool_calls.as_ref().is_some_and(|c| !c.is_empty()),
                 "Captured CLI conversation session id");
        }
        if !output.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) {
            mark_bridge_cli_session_active(&chain_agentgate_sid).await;
            tracing::info!(event = "bridge.cli_session.committed", provider_id = %self.id,
                 session_id = %chain_agentgate_sid, "CLI session marked active for next turn");
        }

        // PR-S4: opencode continuation (capability-gated). If tool_calls returned and session can
        // continue after MCP hold resolve, keep session alive, register continuation, spawn producer.
        // Replay fallback remains mandatory (service/bridge handle absence/timeout/loss).
        let has_tool_calls = output.tool_calls.as_ref().is_some_and(|v| !v.is_empty());
        let continuation_capable = session.continuation_capability() != crate::provider::bridge::ContinuationCapability::Unsupported;
        let continuation_key = request.agentgate_run_id.clone().or_else(|| mcp_session_id.clone());
        let continuation_registered = if has_tool_calls && continuation_capable && continuation_key.is_some() && self.bridge.is_some() {
            if let (Some(bridge), Some(key)) = (self.bridge.clone(), continuation_key.clone()) {
                let tx = bridge.register_continuation(key.clone()).await;
                let session_for_cont = session.clone();
                let key_for_task = key.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(bridge_turn_timeout(), session_for_cont.wait_for_continuation()).await {
                        Ok(Ok(out)) => { let _ = tx.send(out); }
                        Ok(Err(e)) => {
                            tracing::warn!(event = "continuation.producer.failed", key = %key_for_task, error = %e);
                        }
                        Err(_) => {
                            tracing::warn!(event = "continuation.producer.timeout", key = %key_for_task);
                        }
                    }
                    // tx drop on all paths => resume await returns Err, service replays (fallback mandatory).
                    let _ = bridge.remove_continuation(&key_for_task).await; // best-effort if never taken
                });
                tracing::info!(event = "continuation.registered", key = %key, has_tool_calls = true);
                true
            } else { false }
        } else { false };

        if continuation_registered {
            // Keep session + MCP registration alive for live continuation; do not close/release.
            if should_store {
                self.store_persistent_session(session_id.clone(), session.clone()).await;
            }
        } else if keep_alive {
            // Store the session for reuse, don't close it
            if should_store {
                self.store_persistent_session(session_id.clone(), session.clone()).await;
            }
            // Don't release MCP session for persistent sessions
        } else {
            // Close session and release MCP session for non-persistent mode
            let _ = session.close().await;
            if let Some(ref mcp_id) = mcp_session_id {
                self.release_session(&Some(mcp_id.clone())).await;
            }
        }

        let mut tool_calls = output.tool_calls;
        if let Some(calls) = tool_calls.as_mut() {
            tracing::info!(target: "bridge.unalias", before = ?calls, tools_count = request.tools.as_deref().map(|t| t.len()).unwrap_or(0), "Before unalias_tool_calls");
            crate::openai::tools::unalias_tool_calls(
                calls,
                request.tools.as_deref().unwrap_or(&[]),
            );
            tracing::info!(target: "bridge.unalias", after = ?calls, "After unalias_tool_calls");
        }
        Ok(ProviderResponse::Success {
            content: output.content,
            reasoning: output.reasoning,
            tool_calls,
            session_id: Some(session_id),
            mcp_session_id: mcp_session_id.clone(),
        })
    }

    async fn send_chat_completion_stream(
        &self,
        model: &str,
        request: crate::types::ChatCompletionRequest,
        tx: tokio::sync::mpsc::Sender<crate::reasoning::RunEvent>,
    ) -> Result<(), anyhow::Error> {
        use uuid::Uuid;
        // Ollama models may arrive as "ollama/<model>".
        let model = model.strip_prefix("ollama/").unwrap_or(model);

        // Extract or generate session_id from request
        let session_id = request.session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
        
        // Check if we have a persistent session for this session_id
        let keep_alive = self.keep_alive.is_some();
        let existing_session = if keep_alive {
            let sessions = self.persistent_sessions.read().await;
            sessions.get(&session_id).map(|p| p.session.clone())
        } else {
            None
        };

        let (session, should_store, mcp_session_id): (
            Arc<dyn crate::provider::bridge::BridgeSession>,
            bool,
            Option<String>,
        ) = if let Some(existing) = existing_session {
            // Reuse existing persistent session
            tracing::info!(target: "bridge.cli", session_id = %session_id, "Reusing persistent session (stream)");
            (existing, false, None)
        } else {
            // Register the MCP session before spawning so /_bridge/mcp/{id} is known
            // to the bridge server (spec §9.2).
            let mut tool_schemas = self.tool_schemas_from_request(&request);
            // Inject agentgate built-in discovery tools (check_mcp_local, skill_local)
            // so the model can inspect local MCP servers and skills.
            tool_schemas.extend(crate::bridge::agentgate_builtin_tools());
            // Determine max_inject based on model type: free models (beam-free, etc.) get limited injection
              // to avoid provider.free-limit errors, while paid models get full tool access.
              let is_free_model = model.to_lowercase().ends_with("-free");
              let max_inject = if is_free_model { self.limits.max_inject } else { self.limits.max_tools };
            let tool_schemas_for_bridge: Vec<_> = if tool_schemas.len() > max_inject {
                tracing::warn!(model = %model, total = tool_schemas.len(), max = max_inject, "Truncating injected tools");
                tool_schemas[..max_inject].to_vec()
            } else {
                tool_schemas.clone()
            };
            // FIX-SESSION-TOOLS: register the same truncated list that is handed
            // to the shim/config; registering the full 91-tool list made
            // preflight see client tools like `Read` and (before the name fix)
            // reject every tool call.
            let (mcp_session_id, session_token) = match self.register_session(tool_schemas_for_bridge.clone()).await {
                Some((id, token)) => (Some(id), Some(token)),
                None => (None, None),
            };
            let local_session_id = mcp_session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
            let data_dir = std::env::temp_dir().join("agentgate");
            let workdir = data_dir.join("sessions").join(&local_session_id).join("work");
            tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

            let bridge_port: u16 = std::env::var("AGENTGATE_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8786);
            tracing::info!(target: "bridge.cli", bridge_port = bridge_port, "Using bridge port");

            // FIX-SESSION: resume the captured OpenCode CLI conversation session
            // (e.g. `ses_...`) when one was remembered for this agentgate session.
            // On model reroute/quota failover, force a fresh CLI session to avoid model/context mismatch.
            let force_fresh_cli = request.agentgate_force_fresh_cli_session;
            if force_fresh_cli {
                self.remember_opencode_session(&session_id, None).await;
                clear_cli_session_for_chain(&session_id).await;
                tracing::info!(event="bridge.cli_session.force_fresh", session_id=%session_id, model=%model, "Forced fresh CLI session (model reroute/quota failover)");
            }
            let resume_cli_sid = if force_fresh_cli {
                None
            } else {
                self.stored_opencode_session(&session_id).await
            };

            // FIX-PROMPT-01: Default strip system prompt — do not forward to CLI
            // System messages are skipped in render_conversation (see function)
            let bridge_config = BridgeConfig {
                host: "127.0.0.1".to_string(),
                port: bridge_port,
                model: model.to_string(),
                profile_name: self.factory.default_profile().name.clone(),
                data_dir: Some(data_dir.clone()),
                session_id: mcp_session_id.clone(),
                session_token,
                keep_alive: self.keep_alive.clone(),
                prompt_tools_fallback: self.prompt_tools_fallback,
                transport: self.transport.clone(),
                tools: tool_schemas_for_bridge,
                isolation: Some(self.isolation_config.clone()),
                cli_session_id: resume_cli_sid.clone(),
                passthrough_host_mcp: self.passthrough_host_mcp,
                passthrough_mcp_allowlist: self.passthrough_mcp_allowlist.clone(),
                passthrough_mcp_denylist: self.passthrough_mcp_denylist.clone(),
                allow_client_mcp: self.allow_client_mcp,
                client_mcp_servers: request.mcp_servers.clone(),
            };

            tracing::info!(target: "bridge.cli", model = %model, "Creating bridge provider (stream)");
            // Create bridge provider from factory
            let provider = self.factory.create(bridge_config.clone()).await?;

            // Create bridge client with bridge reference for PR-S4 continuation
            let bridge = self.bridge.as_ref().ok_or_else(|| anyhow!("bridge required for bridge provider"))?.clone();
            let client = Arc::new(BridgeClient::new_with_bridge(
                "127.0.0.1".to_string(),
                bridge_port,
                bridge,
            ));

            // Start bridge session
            let session = provider.start_bridge(bridge_config, client).await?;

            // Convert Box to Arc for persistent session storage
            let session: std::sync::Arc<dyn crate::provider::bridge::BridgeSession> = std::sync::Arc::from(session);

            // Register provider session with BridgeServer for cancellation support
            if let Some(ref bridge) = self.bridge {
                if let Some(ref mcp_id) = mcp_session_id {
                    bridge.register_provider_session(mcp_id.clone(), session.clone()).await;
                }
            }

            (session, keep_alive, mcp_session_id)
        };

        // Send the structured conversation to the CLI so system instructions,
        // assistant tool calls, and tool results survive (spec §11.2).
        let mut user_message = render_conversation(
            &request.messages,
            Some(self.max_prompt_chars),
            Some(self.max_result_bytes),
        );
        tracing::info!(
            model = %model,
            messages = request.messages.len(),
            prompt_bytes = user_message.len(),
            budget_bytes = self.max_prompt_chars,
            "rendered prompt for CLI"
        );
        if self.prompt_tools_fallback {
            if let Some(tools) = request.tools.as_deref().filter(|t| !t.is_empty()) {
                user_message.push_str(&tool_prompt_instructions(tools));
            }
        }

        // Preflight isolation check before admitting a tool-capable turn
        // (spec §13.2, fail-closed): refuse the turn when built-in tool
        // isolation could not be verified instead of running unverified tools.
        let isolation_ok = session.preflight_isolation_check().await?;
        if !isolation_ok {
            if !keep_alive {
                let _ = session.close().await;
            }
            if let Some(ref mcp_id) = mcp_session_id {
                self.release_session(&Some(mcp_id.clone())).await;
            }
            return Err(anyhow::anyhow!("tool_isolation_unavailable: isolation preflight failed"));
        }

        if !user_message.trim().is_empty() {
            if let Err(e) = session.send_user_message(&user_message).await {
                if is_pipe_broken(&e) {
                    // Reused CLI child already exited: its stdin pipe is dead.
                    // Drop the cache entry so the caller's transient retry spawns
                    // a fresh child instead of echoing the raw OS error.
                    tracing::warn!(
                        target: "agentgate.provider",
                        provider_id = %self.id,
                        "CLI stdin pipe closed (stale session); dropping session for fresh respawn"
                    );
                    self.retire_broken_session(&session, &session_id, &mcp_session_id).await;
                    return Err(anyhow::anyhow!(
                        "provider_error: broken pipe — CLI stdin closed; stale session dropped"
                    ));
                }
                return Err(e);
            }
        }

        // Take event receiver for true streaming
        let mut event_rx = session.take_event_receiver().await?.unwrap_or_else(|| {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tx.send(serde_json::json!({"type": "warning", "message": "no event receiver available"})).ok();
            rx
        });

        // FIX-TOOL-ROUNDTRIP: merge held MCP tool calls into the CLI event
        // stream. The bridge `tools/call` handler broadcasts tool_calls events
        // for this session; without this merge the client never sees them and
        // cannot answer with a tool result.
        if let Some(ref bridge) = self.bridge {
            let session_key = mcp_session_id.clone().unwrap_or_default();
            let mut tool_rx = bridge.subscribe_tool_events();
            let cli_rx = event_rx;
            let (merged_tx, merged_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let mut cli_rx = cli_rx;
                loop {
                    tokio::select! {
                        ev = cli_rx.recv() => match ev {
                            Some(value) => { if merged_tx.send(value).is_err() { break; } }
                            None => break,
                        },
                        ev = tool_rx.recv() => match ev {
                            Ok((sid, value)) if sid == session_key => {
                                if merged_tx.send(value).is_err() { break; }
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!(target: "bridge.stream", skipped = n, "tool event broadcast lagged");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        },
                    }
                }
            });
            event_rx = merged_rx;
        }

        // FIX-SESSION: capture the CLI conversation session id (e.g. `ses_...`)
        // reported by the adapter and remember it for cross-turn resume via
        // `--session`. Only for keep_alive flows (fresh sessions are transient).
        let captured_cli_sid_stream = session.cli_session_id();
        if keep_alive {
            self.remember_opencode_session(&session_id, captured_cli_sid_stream.clone()).await;
        }

        // Spawn a task to wait for completion in background (for session cleanup)
        let session_for_completion = session.clone();
        let keep_alive_flag = keep_alive;
        let should_store_flag = should_store;
        let session_id_for_cleanup = session_id.clone();
        let mcp_session_id_for_cleanup = mcp_session_id.clone();
        let persistent_sessions_clone = self.persistent_sessions.clone();
        let keep_alive_ttl_for_cleanup = self.keep_alive_ttl;
        let bridge_turn_timeout_secs = bridge_turn_timeout();

        // Collect streaming output
        let mut collected_content = String::new();
        let mut collected_reasoning = String::new();
        let mut collected_tool_calls: Vec<crate::reasoning::event::ToolCall> = Vec::new();
        let mut done_sent = false;
        let mut stream_error: Option<anyhow::Error> = None;

        // Stream events in real-time. Use ReasoningParser so we handle the same
        // CLI event shapes as the non-stream path (OpenCode JSONL, errors, etc.)
        // and don't silently drop important events.
        let tx_stream = tx.clone();
        let request_tools = request.tools.as_deref().unwrap_or(&[]).to_vec();
        let streaming_result: Result<(), anyhow::Error> = async {
            use crate::reasoning::parser::ReasoningParser;
            use crate::reasoning::RunEvent;

            let mut timeout_timer = tokio::time::interval(bridge_turn_timeout_secs);
            timeout_timer.tick().await; // First tick after timeout
            let mut parser = ReasoningParser::new();
            parser.with_client_tool_defs(request_tools.as_slice());

            let mut should_continue = true;
            while should_continue {
                tokio::select! {
                    event_opt = event_rx.recv() => {
                        match event_opt {
                            Some(event) => {
                                // Reset timeout on each event
                                timeout_timer.reset();

                                let line = event.to_string();
                                for parsed in parser.parse_line(&line) {
                                    match parsed {
                                        RunEvent::Text { text } => {
                                            if !text.is_empty() {
                                                collected_content.push_str(&text);
                                                if tx_stream.send(RunEvent::Text { text }).await.is_err() {
                                                    should_continue = false;
                                                    break;
                                                }
                                            }
                                        }
                                        RunEvent::ReasoningStart { block, kind } => {
                                            if tx_stream.send(RunEvent::ReasoningStart { block, kind }).await.is_err() {
                                                should_continue = false;
                                                break;
                                            }
                                        }
                                        RunEvent::ReasoningDelta { block, text } => {
                                            if !text.is_empty() {
                                                collected_reasoning.push_str(&text);
                                                if tx_stream.send(RunEvent::ReasoningDelta { block, text }).await.is_err() {
                                                    should_continue = false;
                                                    break;
                                                }
                                            }
                                        }
                                        RunEvent::ReasoningEnd { block } => {
                                            if tx_stream.send(RunEvent::ReasoningEnd { block }).await.is_err() {
                                                should_continue = false;
                                                break;
                                            }
                                        }
                                        RunEvent::Activity { kind, label } => {
                                            if tx_stream.send(RunEvent::Activity { kind, label }).await.is_err() {
                                                should_continue = false;
                                                break;
                                            }
                                        }
                                        RunEvent::Usage { prompt_tokens, completion_tokens, total_tokens } => {
                                            if tx_stream
                                                .send(RunEvent::Usage { prompt_tokens, completion_tokens, total_tokens })
                                                .await
                                                .is_err()
                                            {
                                                should_continue = false;
                                                break;
                                            }
                                        }
                                        RunEvent::ToolCalls { mut calls } => {
                                            crate::openai::tools::unalias_tool_calls(
                                                &mut calls,
                                                request_tools.as_slice(),
                                            );
                                            collected_tool_calls.extend(calls.clone());
                                            if tx_stream.send(RunEvent::ToolCalls { calls }).await.is_err() {
                                                should_continue = false;
                                                break;
                                            }
                                        }
                                        RunEvent::Error { message, code } => {
                                            let msg = format!("provider_error: {}", message);
                                            let _ = tx_stream.send(RunEvent::Error { message: msg.clone(), code }).await;
                                            stream_error = Some(anyhow::anyhow!(msg));
                                            should_continue = false;
                                            break;
                                        }
                                        RunEvent::Done { reason } => {
                                            done_sent = true;
                                            let _ = tx_stream.send(RunEvent::Done { reason }).await;
                                            should_continue = false;
                                            break;
                                        }
                                    }
                                }
                            }
                            None => {
                                // Channel closed (stdout EOF)
                                break;
                            }
                        }
                    }
                    _ = timeout_timer.tick() => {
                        tracing::warn!(target: "bridge.stream", session_id = %session_id, "Streaming timeout reached");
                        let msg = "provider_error: bridge turn timed out";
                        let _ = tx_stream.send(RunEvent::Error { message: msg.to_string(), code: Some("timeout".to_string()) }).await;
                        stream_error = Some(anyhow::anyhow!(msg));
                        break;
                    }
                }
            }

            // Flush any buffered parser state
            if !done_sent && stream_error.is_none() {
                for parsed in parser.finish() {
                    match parsed {
                        RunEvent::Text { text } => {
                            if !text.is_empty() {
                                collected_content.push_str(&text);
                                let _ = tx_stream.send(RunEvent::Text { text }).await;
                            }
                        }
                        RunEvent::ToolCalls { mut calls } => {
                            crate::openai::tools::unalias_tool_calls(&mut calls, request_tools.as_slice());
                            collected_tool_calls.extend(calls.clone());
                            let _ = tx_stream.send(RunEvent::ToolCalls { calls }).await;
                        }
                        RunEvent::Done { reason } => {
                            done_sent = true;
                            let _ = tx_stream.send(RunEvent::Done { reason }).await;
                        }
                        RunEvent::Error { message, code } => {
                            let msg = format!("provider_error: {}", message);
                            let _ = tx_stream.send(RunEvent::Error { message: msg.clone(), code }).await;
                            stream_error = Some(anyhow::anyhow!(msg));
                        }
                        other => {
                            let _ = tx_stream.send(other).await;
                        }
                    }
                }
            }

            if !done_sent && stream_error.is_none() {
                if collected_content.trim().is_empty() && collected_tool_calls.is_empty() {
                    let msg = "provider_error: CLI produced no output (upstream error or unsupported model/config)";
                    let _ = tx_stream.send(RunEvent::Error { message: msg.to_string(), code: Some("empty_output".to_string()) }).await;
                    stream_error = Some(anyhow::anyhow!(msg));
                    done_sent = true;
                } else {
                    let _ = tx_stream.send(RunEvent::Done { reason: crate::reasoning::FinishReason::Stop }).await;
                    done_sent = true;
                }
            }

            if let Some(err) = stream_error.take() {
                return Err(err);
            }
            Ok(())
        }.await;

        let done_sent_final = done_sent;
        let collected_content_final = collected_content.clone();
        let request_tools_for_cleanup = request_tools.clone();

        // Wait for completion in background for session cleanup. If the client
        // disconnects mid-turn the provider channel closes: drop the wait future
        // (it owns the CLI child and spawn sites set `kill_on_drop`) and close the
        // session so `ollama run` / `opencode` is not left running after the
        // client is gone.
        let completion_task = tokio::spawn(async move {
            let mut client_disconnected = false;
            {
                let wait = tokio::time::timeout(
                    bridge_turn_timeout_secs,
                    session_for_completion.wait_for_completion(),
                );
                tokio::pin!(wait);
                let outcome = tokio::select! {
                    _ = tx.closed() => {
                        client_disconnected = true;
                        None
                    }
                    result = &mut wait => Some(result),
                };

                if let Some(result) = outcome {
                    match result {
                        Ok(Ok(output)) => {
                            tracing::debug!(target: "bridge.cli", session_id = %session_id_for_cleanup, "CLI completed");
                            // Streaming path already handled event delivery in most cases.
                            // Only attempt fallback if nothing was sent and session produced output.
                            if !done_sent_final {
                                if let Some(reasoning) = output.reasoning.filter(|r| !r.trim().is_empty()) {
                                    let _ = tx.send(crate::reasoning::RunEvent::ReasoningStart { block: 0, kind: crate::reasoning::ReasoningKind::Raw }).await;
                                    let _ = tx.send(crate::reasoning::RunEvent::ReasoningDelta { block: 0, text: reasoning }).await;
                                    let _ = tx.send(crate::reasoning::RunEvent::ReasoningEnd { block: 0 }).await;
                                }
                                if !output.content.is_empty() && output.content != collected_content_final {
                                    let _ = tx.send(crate::reasoning::RunEvent::Text { text: output.content }).await;
                                }
                                if let Some(mut calls) = output.tool_calls.filter(|v| !v.is_empty()) {
                                    crate::openai::tools::unalias_tool_calls(&mut calls, request_tools_for_cleanup.as_slice());
                                    let _ = tx.send(crate::reasoning::RunEvent::ToolCalls { calls }).await;
                                }
                                let _ = tx.send(crate::reasoning::RunEvent::Done { reason: crate::reasoning::FinishReason::Stop }).await;
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(target: "bridge.cli", session_id = %session_id_for_cleanup, error = %e, "CLI completion error after streaming");
                        }
                        Err(_) => {
                            tracing::warn!(target: "bridge.cli", session_id = %session_id_for_cleanup, "CLI completion timed out after streaming");
                        }
                    }
                }
            }
            if client_disconnected {
                tracing::info!(
                    target: "bridge.cli",
                    session_id = %session_id_for_cleanup,
                    "client disconnected; cancelling CLI turn"
                );
            }

            // Cleanup session
            if !keep_alive_flag {
                let _ = session_for_completion.close().await;
            } else if should_store_flag {
                // Bound the persistent-session cache the same way as the
                // non-stream path (TTL + cap) so CLI child processes don't pile up.
                let mut sessions = persistent_sessions_clone.write().await;
                let now = std::time::Instant::now();
                let expired: Vec<String> = sessions
                    .iter()
                    .filter(|(_, p)| now.duration_since(p.created_at) >= keep_alive_ttl_for_cleanup)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in expired {
                    if let Some(old) = sessions.remove(&key) {
                        let _ = old.session.close().await;
                    }
                }
                while sessions.len() >= max_persistent_sessions() {
                    let oldest = sessions
                        .iter()
                        .min_by_key(|(_, p)| p.created_at)
                        .map(|(k, _)| k.clone());
                    let Some(key) = oldest else { break };
                    if let Some(old) = sessions.remove(&key) {
                        let _ = old.session.close().await;
                    }
                }
                sessions.insert(
                    session_id_for_cleanup,
                    PersistentSession { session: session_for_completion, created_at: now },
                );
            }
            let _ = mcp_session_id_for_cleanup;
        });

        // Detach completion task (runs in background for cleanup)
        drop(completion_task);

        streaming_result
    }
}






