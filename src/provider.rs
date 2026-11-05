use crate::config::ProviderConfig;
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
    fn scan_models(&self) -> Result<Vec<String>, anyhow::Error>;
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
            ProviderResponse::Success { content, reasoning, tool_calls, session_id: _ } => {
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

        // Direct path check (handles absolute/relative paths)
        if std::path::Path::new(bin).exists() {
            return true;
        }

        // PATH search via which crate
        if which::which(bin).is_ok() {
            return true;
        }

        // Windows: try common extensions for PATH search
        if cfg!(target_os = "windows") {
            for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                if !bin.to_lowercase().ends_with(ext) {
                    let candidate = format!("{}{}", bin, ext);
                    if which::which(&candidate).is_ok() {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        tracing::debug!("Scanning models for provider {}", self.id);
        if let Some(cmds) = &self.models_cmd {
            if cmds.is_empty() {
                tracing::debug!("models_cmd is empty, returning config models");
                return Ok(self.models.clone());
            }

            let bin = self.bin.as_ref().ok_or_else(|| {
                anyhow::anyhow!("No binary configured for provider {}", self.id)
            })?;
            tracing::debug!("Using bin: {}, cmd: {:?}", bin, cmds);

            let output = self.execute_cmd_inner(bin, cmds)?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            tracing::debug!("stdout: {}", stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                tracing::debug!("stderr: {}", stderr);
            }
            let models = self.parse_models_output(&stdout);
            tracing::debug!("Parsed {} models", models.len());
            if !models.is_empty() {
                return Ok(models);
            }

            tracing::debug!("No models found via models_cmd, falling back to config");
            return Ok(self.models.clone());
        }

        Ok(self.models.clone())
    }

    async fn send_chat_completion(
        &self,
        model: &str,
        request: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        // Create empty session workdir per spec §11.3
        let session_id = uuid::Uuid::new_v4().to_string();
        let data_dir = std::path::PathBuf::from("/tmp/agentgate");
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        let (mut cmd, stdin_prompt) = self.build_chat_command(model, &request, Some(&workdir))?;
        let mut child = cmd.spawn()?;

        // Write prompt to stdin if required, then close to signal EOF
        if let Some(prompt) = stdin_prompt {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(prompt.as_bytes()).await?;
                // Explicitly close stdin to signal EOF to CLI
                drop(stdin);
            }
        }

        let output = child.wait_with_output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let cleaned = if self.strip_ansi {
            String::from_utf8_lossy(&strip_ansi_escapes::strip(&*stdout)).to_string()
        } else {
            stdout.to_string()
        };
        // If stdout empty and stderr has content, treat as error
        if cleaned.trim().is_empty() && !stderr.trim().is_empty() {
            return Ok(ProviderResponse::Error(stderr.trim().to_string()));
        }
        let (content, reasoning) = parse_cli_output(cleaned.trim());
        Ok(ProviderResponse::Success { content, reasoning, tool_calls: None, session_id: None })
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
        let data_dir = std::path::PathBuf::from("/tmp/agentgate");
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        let (mut cmd, stdin_prompt) = self.build_chat_command(model, &request, Some(&workdir))?;
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
                result = reader.read_until(b'\n', &mut line_buf) => result,
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
fn parse_cli_output(cleaned: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = cleaned.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return (String::new(), None);
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
        for line in &lines {
            for event in parser.parse_line(line) {
                match event {
                    RunEvent::ReasoningDelta { text, .. } => reasoning.push_str(&text),
                    RunEvent::Text { text } => texts.push(text),
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
        return (content, reasoning);
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

    (content, reasoning)
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

        None
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
        if self.id == "opencode" {
            let disable = serde_json::json!({
                "tools": {
                    "bash": false, "edit": false, "write": false, "read": false,
                    "patch": false, "grep": false, "glob": false, "list": false,
                    "webfetch": false, "todowrite": false, "todoread": false, "task": false
                },
                "permission": {
                    "externalDirectory": "deny",
                    "network": "deny",
                    "edit": "deny",
                    "bash": "deny",
                    "webfetch": "deny"
                }
            });
            cmd.env("OPENCODE_CONFIG_CONTENT", disable.to_string());
        }

        cmd.args(&cmd_args)
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
                line.split_whitespace().next().unwrap_or("").to_string()
            })
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

/// Strip all ANSI escape codes from text
pub fn strip_ansi_codes(text: &str) -> String {
    String::from_utf8_lossy(&strip_ansi_escapes::strip(text)).to_string()
}

pub mod ollama_native;
pub mod bridge;

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
pub fn render_conversation(messages: &[crate::types::ChatMessage]) -> String {
    use crate::types::MessageRole;

    let mut out = String::new();
    for msg in messages {
        match msg.role {
            MessageRole::System => {
                if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push_str("[system]\n");
                    out.push_str(content.trim());
                    out.push_str("\n\n");
                }
            }
            MessageRole::User => {
                if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push_str("[user]\n");
                    out.push_str(content.trim());
                    out.push_str("\n\n");
                }
            }
            MessageRole::Assistant => {
                if let Some(calls) = msg.tool_calls.as_ref().filter(|c| !c.is_empty()) {
                    for call in calls {
                        out.push_str(&format!(
                            "[assistant tool_call id={} name={}]\n{}\n\n",
                            call.id, call.function.name, call.function.arguments
                        ));
                    }
                }
                if let Some(content) = msg.content.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push_str("[assistant]\n");
                    out.push_str(content.trim());
                    out.push_str("\n\n");
                }
            }
            MessageRole::Tool => {
                let id = msg.tool_call_id.as_deref().unwrap_or("");
                let name = msg.name.as_deref().unwrap_or("");
                let content = msg.content.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "[tool result id={} name={}]\n{}\n\n",
                    id,
                    name,
                    content.trim()
                ));
            }
        }
    }
    out.trim_end().to_string()
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
    /// Cache of persistent sessions (keyed by session_id) for keep_alive mode.
    /// When keep_alive is enabled, sessions are stored here instead of being closed.
    /// Using Arc to allow cloning and sharing sessions across requests.
    persistent_sessions: Arc<RwLock<HashMap<String, Arc<dyn crate::provider::bridge::BridgeSession>>>>,
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
            persistent_sessions: Arc::new(RwLock::new(HashMap::new())),
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
    // the bridge adapter (spec_ollama-cli.md).
    let wants_bridge = if config.id == "ollama" {
        config.ollama_mode.as_deref() == Some("cli")
    } else {
        config.bridge
    };

    if wants_bridge {
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

        // Direct path check
        if std::path::Path::new(bin).exists() {
            return true;
        }

        // PATH search via which crate
        if which::which(bin).is_ok() {
            return true;
        }

        // Windows: try common extensions
        if cfg!(target_os = "windows") {
            for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                if !bin.to_lowercase().ends_with(ext) {
                    let candidate = format!("{}{}", bin, ext);
                    if which::which(&candidate).is_ok() {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
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
            let (mcp_session_id, session_token) = match self.register_session(tool_schemas).await {
                Some((id, token)) => (Some(id), Some(token)),
                None => (None, None),
            };
            let local_session_id = mcp_session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
            let data_dir = std::path::PathBuf::from("/tmp/agentgate");
            let workdir = data_dir.join("sessions").join(&local_session_id).join("work");
            tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

            let bridge_port: u16 = std::env::var("AGENTGATE_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8786);

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
            };

            tracing::info!(target: "bridge.cli", model = %model, "Creating bridge provider");
            // Create bridge provider from factory
            let provider = self.factory.create(bridge_config.clone()).await?;

            // Create bridge client
            let client = Arc::new(BridgeClient {
                host: "127.0.0.1".to_string(),
                port: bridge_port,
            });

            // Start bridge session (spawns CLI with MCP config)
            let session = provider.start_bridge(bridge_config, client).await?;

            // Convert Box to Arc for persistent session storage
            let session = Arc::from(session);

            (session, keep_alive, mcp_session_id)
        };

        // Send the structured conversation to the CLI so system instructions,
        // assistant tool calls, and tool results survive (spec §11.2).
        let mut user_message = render_conversation(&request.messages);
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

        if keep_alive {
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
            let (mcp_session_id, session_token) = match self.register_session(tool_schemas).await {
                Some((id, token)) => (Some(id), Some(token)),
                None => (None, None),
            };
            let local_session_id = mcp_session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
            let data_dir = std::path::PathBuf::from("/tmp/agentgate");
            let workdir = data_dir.join("sessions").join(&local_session_id).join("work");
            tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

            let bridge_port: u16 = std::env::var("AGENTGATE_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8786);

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
            };

            tracing::info!(target: "bridge.cli", model = %model, "Creating bridge provider (stream)");
            // Create bridge provider from factory
            let provider = self.factory.create(bridge_config.clone()).await?;

            // Create bridge client
            let client = Arc::new(BridgeClient {
                host: "127.0.0.1".to_string(),
                port: bridge_port,
            });

            // Start bridge session
            let session = provider.start_bridge(bridge_config, client).await?;

            // Convert Box to Arc for persistent session storage
            let session = Arc::from(session);

            (session, keep_alive, mcp_session_id)
        };

        // Send the structured conversation to the CLI so system instructions,
        // assistant tool calls, and tool results survive (spec §11.2).
        let mut user_message = render_conversation(&request.messages);
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

        // Wait for completion and stream events
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

        if keep_alive {
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

        // Stream the result as events
        if let Some(reasoning) = output.reasoning.filter(|r| !r.trim().is_empty()) {
            if tx.send(crate::reasoning::RunEvent::ReasoningStart { block: 0, kind: crate::reasoning::ReasoningKind::Raw }).await.is_err() { return Ok(()); }
            if tx.send(crate::reasoning::RunEvent::ReasoningDelta { block: 0, text: reasoning }).await.is_err() { return Ok(()); }
            if tx.send(crate::reasoning::RunEvent::ReasoningEnd { block: 0 }).await.is_err() { return Ok(()); }
        }
        if !output.content.is_empty() {
            if tx.send(crate::reasoning::RunEvent::Text { text: output.content }).await.is_err() { return Ok(()); }
        }
        if let Some(mut calls) = output.tool_calls.filter(|v| !v.is_empty()) {
            crate::openai::tools::unalias_tool_calls(
                &mut calls,
                request.tools.as_deref().unwrap_or(&[]),
            );
            if tx.send(crate::reasoning::RunEvent::ToolCalls { calls }).await.is_err() { return Ok(()); }
        }
        let _ = tx.send(crate::reasoning::RunEvent::Done { reason: crate::reasoning::FinishReason::Stop }).await;

        Ok(())
    }
}