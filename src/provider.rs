use crate::config::ProviderConfig;
use crate::reasoning::parser::ReasoningParser;
use crate::reasoning::{FinishReason, ReasoningKind, RunEvent};
use crate::types::ChatCompletionRequest;
use anyhow::Context;
use async_trait::async_trait;
use std::process::Command;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;
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
            ProviderResponse::Success { content, reasoning } => {
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
        Ok(ProviderResponse::Success { content, reasoning })
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

use crate::provider::bridge::{BridgeProviderV2, BridgeConfig, BridgeSession, create_bridge_factory_v2, SessionOutput};
use crate::bridge::BridgeClient;
use std::sync::Arc;

/// A CLI provider that uses the bridge architecture (MCP bridge to Device A).
/// Used for OpenCode, Gemini, and Claude CLI agents per spec_tooling.md.
pub struct BridgeCliProvider {
    pub id: String,
    pub bin: Option<String>,
    pub models: Vec<String>,
    pub is_available: bool,
    factory: Box<dyn crate::provider::bridge::BridgeProviderFactory + Send + Sync>,
}

impl BridgeCliProvider {
    pub fn new(config: crate::config::ProviderConfig) -> Option<Self> {
        // Determine which bridge factory to use based on provider ID
        let factory = create_bridge_factory_v2(&config.id)?;

        Some(Self {
            id: config.id,
            bin: config.bin.clone(),
            models: config.models.clone(),
            is_available: false,
            factory,
        })
    }
}

#[async_trait]
impl CliProvider for BridgeCliProvider {
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

        // Create bridge config
        let session_id = Uuid::new_v4().to_string();
        let data_dir = std::path::PathBuf::from("/tmp/agentgate");
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        let bridge_config = BridgeConfig {
            host: "127.0.0.1".to_string(),
            port: 8786, // AgentGate bridge port
            model: model.to_string(),
            profile_name: self.factory.default_profile().name.clone(),
            data_dir: Some(data_dir.clone()),
        };

        // Create bridge provider from factory
        let provider = self.factory.create(bridge_config.clone()).await?;

        // Create bridge client
        let client = Arc::new(BridgeClient {
            host: "127.0.0.1".to_string(),
            port: 8786,
        });

        // Start bridge session (spawns CLI with MCP config)
        let session = provider.start_bridge(bridge_config, client).await?;

        // Send user message to CLI
        let user_message = request
            .messages
            .iter()
            .filter_map(|msg| msg.content.clone())
            .collect::<Vec<String>>()
            .join("\n");

        if !user_message.trim().is_empty() {
            session.send_user_message(&user_message).await?;
        }

        // Wait for completion (handles tool calls via bridge)
        let output = session.wait_for_completion().await?;

        Ok(ProviderResponse::Success {
            content: output.content,
            reasoning: output.reasoning,
        })
    }

    async fn send_chat_completion_stream(
        &self,
        model: &str,
        request: crate::types::ChatCompletionRequest,
        tx: tokio::sync::mpsc::Sender<crate::reasoning::RunEvent>,
    ) -> Result<(), anyhow::Error> {
        use uuid::Uuid;

        // Create bridge config
        let session_id = Uuid::new_v4().to_string();
        let data_dir = std::path::PathBuf::from("/tmp/agentgate");
        let workdir = data_dir.join("sessions").join(&session_id).join("work");
        tokio::fs::create_dir_all(&workdir).await.context("Failed to create session workdir")?;

        let bridge_config = BridgeConfig {
            host: "127.0.0.1".to_string(),
            port: 8786,
            model: model.to_string(),
            profile_name: self.factory.default_profile().name.clone(),
            data_dir: Some(data_dir.clone()),
        };

        // Create bridge provider from factory
        let provider = self.factory.create(bridge_config.clone()).await?;

        // Create bridge client
        let client = Arc::new(BridgeClient {
            host: "127.0.0.1".to_string(),
            port: 8786,
        });

        // Start bridge session
        let session = provider.start_bridge(bridge_config, client).await?;

        // Send user message
        let user_message = request
            .messages
            .iter()
            .filter_map(|msg| msg.content.clone())
            .collect::<Vec<String>>()
            .join("\n");

        if !user_message.trim().is_empty() {
            session.send_user_message(&user_message).await?;
        }

        // Wait for completion and stream events
        let output = session.wait_for_completion().await?;

        // Stream the result as events
        if let Some(reasoning) = output.reasoning.filter(|r| !r.trim().is_empty()) {
            if tx.send(crate::reasoning::RunEvent::ReasoningStart { block: 0, kind: crate::reasoning::ReasoningKind::Raw }).await.is_err() { return Ok(()); }
            if tx.send(crate::reasoning::RunEvent::ReasoningDelta { block: 0, text: reasoning }).await.is_err() { return Ok(()); }
            if tx.send(crate::reasoning::RunEvent::ReasoningEnd { block: 0 }).await.is_err() { return Ok(()); }
        }
        if !output.content.is_empty() {
            if tx.send(crate::reasoning::RunEvent::Text { text: output.content }).await.is_err() { return Ok(()); }
        }
        let _ = tx.send(crate::reasoning::RunEvent::Done { reason: crate::reasoning::FinishReason::Stop }).await;

        Ok(())
    }
}