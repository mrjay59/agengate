use crate::config::ProviderConfig;
use crate::provider::bridge::BridgeSession as ProviderBridgeSession;
use crate::reasoning::parser::ReasoningParser;
use crate::reasoning::{FinishReason, ReasoningKind, RunEvent};
use crate::types::ChatCompletionRequest;
use anyhow::Context;
use async_trait::async_trait;
use std::collections::HashMap;
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
        if let Err(e) = read_result.unwrap() {
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
        } else {
            model
        };

        // Build prompt from messages.
        let prompt = request
            .messages
            .iter()
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

        // Disable OpenCode built-in tools so nothing executes on server B.
        // opencode 2.x reads project config from opencode.json in the working
        // directory (OPENCODE_CONFIG_CONTENT is not honored), so write it to the
        // session workdir when available.
        if self.id == "opencode" {
            let disable = serde_json::json!({
                "tools": {
                    "bash": false, "edit": false, "write": false, "read": false,
                    "patch": false, "grep": false, "glob": false, "list": false,
                    "webfetch": false, "todowrite": false, "todoread": false, "task": false,
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
            });
            if let Some(dir) = workdir {
                let cfg = dir.join("opencode.json");
                if let Err(e) = std::fs::write(&cfg, disable.to_string()) {
                    tracing::warn!(target: "agentgate::provider", error = %e, path = %cfg.display(), "Failed to write opencode.json; built-in tool disabling may not apply");
                }
            }
        }

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
pub fn bridge_turn_timeout() -> std::time::Duration {
    let secs = std::env::var("AGENTGATE_BRIDGE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600u64);
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

    let mut out = String::new();
    let mut total_chars = 0usize;
    for msg in messages {
        match msg.role {
            MessageRole::System => {
                if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    let trimmed = content.trim();
                    let (truncated, exceeded) = limit_chars(trimmed, max_prompt_chars, total_chars);
                    if exceeded {
                        break;
                    }
                    total_chars += truncated.len();
                    out.push_str("[system]\n");
                    out.push_str(truncated);
                    out.push_str("\n\n");
                }
            }
            MessageRole::User => {
                if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    let trimmed = content.trim();
                    let (truncated, exceeded) = limit_chars(trimmed, max_prompt_chars, total_chars);
                    if exceeded {
                        break;
                    }
                    total_chars += truncated.len();
                    out.push_str("[user]\n");
                    out.push_str(truncated);
                    out.push_str("\n\n");
                }
            }
            MessageRole::Assistant => {
                if let Some(calls) = msg.tool_calls.as_ref().filter(|c| !c.is_empty()) {
                    for call in calls {
                        out.push_str(&format!(
                            "[assistant tool_call id={} name={}]\n{}\n\n",
                            call.id,
                            call.function.name,
                            call.function.arguments
                        ));
                    }
                }
                if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    let trimmed = content.trim();
                    let (truncated, exceeded) = limit_chars(trimmed, max_prompt_chars, total_chars);
                    if exceeded {
                        break;
                    }
                    total_chars += truncated.len();
                    out.push_str("[assistant]\n");
                    out.push_str(truncated);
                    out.push_str("\n\n");
                }
            }
            MessageRole::Tool => {
                let id = msg.tool_call_id.as_deref().unwrap_or("");
                let name = msg.name.as_deref().unwrap_or("");
                let content = msg.content.as_deref().unwrap_or("");
                // Enforce max_result_bytes on tool result content
                let truncated = limit_bytes(content, max_result_bytes);
                out.push_str(&format!(
                    "[tool result id={} name={}]\n{}\n\n",
                    id,
                    name,
                    truncated
                ));
            }
        }
    }
    out.trim_end().to_string()
}

/// Limit character count within remaining budget. Returns (content, exceeded).
fn limit_chars(content: &str, max_prompt_chars: Option<usize>, used: usize) -> (&str, bool) {
    let Some(limit) = max_prompt_chars else {
        return (content, false);
    };
    let remaining = limit.saturating_sub(used);
    if remaining == 0 {
        return ("", true);
    }
    let bytes = content.as_bytes();
    let take = bytes.len().min(remaining);
    let truncated = &content[..take];
    (truncated, take < bytes.len())
}

/// Limit byte count for tool result content. Returns truncated content.
fn limit_bytes(content: &str, max_result_bytes: Option<usize>) -> &str {
    let Some(limit) = max_result_bytes else {
        return content;
    };
    if content.len() <= limit {
        return content;
    }
    // Truncate at a character boundary near the limit
    &content[..limit.min(content.len())]
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
    /// Cache of persistent sessions (keyed by session_id) for keep_alive mode.
    /// When keep_alive is enabled, sessions are stored here instead of being closed.
    /// Using Arc to allow cloning and sharing sessions across requests.
    persistent_sessions: Arc<RwLock<HashMap<String, Arc<dyn crate::provider::bridge::BridgeSession>>>>,
    /// Limits for prompt rendering
    max_prompt_chars: usize,
    max_result_bytes: usize,
}

impl BridgeCliProvider {
    pub fn new(config: crate::config::ProviderConfig) -> Option<Self> {
        Self::with_bridge(config, None)
    }

    /// Create a bridge provider bound to the shared `BridgeServer` so each run
    /// registers its MCP session (id + token) before spawning the CLI.
    pub fn with_bridge(
        config: crate::config::ProviderConfig,
        bridge: Option<Arc<crate::bridge::BridgeServer>>,
    ) -> Option<Self> {
        // Determine which bridge factory to use based on provider ID
        let factory = create_bridge_factory_v2(&config.id)?;

        Some(Self {
            id: config.id,
            bin: config.bin.clone(),
            models: config.models.clone(),
            is_available: false,
            factory,
            bridge,
            keep_alive: config.keep_alive.clone(),
            prompt_tools_fallback: config.prompt_tools_fallback,
            transport: config.transport.clone(),
            persistent_sessions: Arc::new(RwLock::new(HashMap::new())),
            max_prompt_chars: 400_000,
            max_result_bytes: 262_144,
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

    /// Check if keep_alive (persistent session) is enabled for this provider.
    pub fn is_keep_alive(&self) -> bool {
        matches!(self.keep_alive.as_deref(), Some("true"))
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
                        let name = openai_name_alias(&t.function.name, index);
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
}

/// Select the CLI provider implementation for a configured provider.
/// Bridge-capable agents (`opencode`, `gemini`, `claude`) use the MCP bridge
/// adapter unless `bridge = false` is set in config (spec §6.2, §12).
pub fn build_cli_provider(
    config: &crate::config::ProviderConfig,
    bridge: Option<Arc<crate::bridge::BridgeServer>>,
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
        if let Some(bp) = BridgeCliProvider::with_bridge(cfg, bridge) {
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
            sessions.get(&session_id).map(|s| s.clone())
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
            let tool_schemas = self.tool_schemas_from_request(&request);
            let tool_schemas_for_bridge = tool_schemas.clone();
            let (mcp_session_id, session_token) = match self.register_session(tool_schemas).await {
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
            };

            tracing::info!(target: "bridge.cli", model = %model, "Creating bridge provider");
            // Create bridge provider from factory
            let provider = self.factory.create(bridge_config.clone()).await?;

            // Create bridge client with bridge reference for PR-S4 continuation
            let client = Arc::new(BridgeClient::new_with_bridge(
                "127.0.0.1".to_string(),
                bridge_port,
                self.bridge.as_ref().expect("bridge required for bridge provider").clone(),
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
            session.send_user_message(&user_message).await?;
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
                let mut sessions = self.persistent_sessions.write().await;
                sessions.insert(session_id.clone(), session.clone());
            }
        } else if keep_alive {
            // Store the session for reuse, don't close it
            if should_store {
                let mut sessions = self.persistent_sessions.write().await;
                sessions.insert(session_id.clone(), session.clone());
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
            crate::openai::tools::unalias_tool_calls(
                calls,
                request.tools.as_deref().unwrap_or(&[]),
            );
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
            sessions.get(&session_id).map(|s| s.clone())
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
            let tool_schemas = self.tool_schemas_from_request(&request);
            let tool_schemas_for_bridge = tool_schemas.clone();
            let (mcp_session_id, session_token) = match self.register_session(tool_schemas).await {
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
            };

            tracing::info!(target: "bridge.cli", model = %model, "Creating bridge provider (stream)");
            // Create bridge provider from factory
            let provider = self.factory.create(bridge_config.clone()).await?;

            // Create bridge client with bridge reference for PR-S4 continuation
            let client = Arc::new(BridgeClient::new_with_bridge(
                "127.0.0.1".to_string(),
                bridge_port,
                self.bridge.as_ref().expect("bridge required for bridge provider").clone(),
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
            session.send_user_message(&user_message).await?;
        }

        // Take event receiver for true streaming
        let mut event_rx = session.take_event_receiver().await?.unwrap_or_else(|| {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tx.send(serde_json::json!({"type": "warning", "message": "no event receiver available"})).ok();
            rx
        });

        // Spawn a task to wait for completion in background (for session cleanup)
        let session_for_completion = session.clone();
        let keep_alive_flag = keep_alive;
        let should_store_flag = should_store;
        let session_id_for_cleanup = session_id.clone();
        let mcp_session_id_for_cleanup = mcp_session_id.clone();
        let persistent_sessions_clone = self.persistent_sessions.clone();
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
                let mut sessions = persistent_sessions_clone.write().await;
                sessions.insert(session_id_for_cleanup, session_for_completion);
            }
            let _ = mcp_session_id_for_cleanup;
        });

        // Detach completion task (runs in background for cleanup)
        drop(completion_task);

        streaming_result
    }
}
