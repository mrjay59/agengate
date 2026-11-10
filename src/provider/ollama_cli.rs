//! Ollama CLI provider - uses `ollama run` command for chat completions
//!
//! This provider is used when `ollama_mode = "cli"` in provider config.
//! It spawns the `ollama` binary directly instead of using HTTP API.

use std::process::Command as StdCommand;
use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
// use serde_json::Value; // not used with plain text mode
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::config::ProviderConfig;
use crate::reasoning::{FinishReason, ReasoningKind, RunEvent};
use crate::types::{ChatCompletionRequest, ChatMessage, MessageRole};
use crate::provider::{CliProvider, CliProviderImpl, ProviderResponse};

/// Ollama CLI provider that uses `ollama run --format json` for chat completions.
/// This is an alternative to the native HTTP provider, useful when HTTP is slow.
#[derive(Clone)]
pub struct OllamaCliProvider {
    /// Base provider for common trait methods
    base: CliProviderImpl,
    /// Ollama binary path
    bin: String,
}

impl OllamaCliProvider {
    /// Create a new Ollama CLI provider from config
    pub fn new(config: ProviderConfig) -> Self {
        let bin = config.bin.clone().unwrap_or_else(|| "ollama".to_string());
        
        // Create base provider with common config
        let base = CliProviderImpl {
            id: config.id.clone(),
            bin: config.bin.clone(),
            models_cmd: Some(vec!["list".to_string()]),
            chat_cmd: None, // We use custom chat via stdin
            strip_ansi: true,
            models: config.models.clone(),
            is_available: true,
        };

        Self { base, bin }
    }

    /// Build the `ollama run` command for a model
    fn build_run_command(&self, model: &str, _stream: bool) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("run")
            .arg(model);
        // Note: --format json doesn't stream in current Ollama version (0.40+).
        // Using plain text output for better streaming support.
        // if stream {
        //     cmd.arg("--stream");
        // }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        cmd
    }

    /// Convert chat messages to a single prompt string for Ollama CLI
    fn messages_to_prompt(&self, messages: &[ChatMessage]) -> String {
        let mut prompt = String::new();
        for msg in messages {
            let role_str = match msg.role {
                MessageRole::System => "System",
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::Tool => "Tool",
            };
            if let Some(content) = &msg.content {
                if !content.is_empty() {
                    prompt.push_str(&format!("{}: {}\n\n", role_str, content));
                }
            }
        }
        prompt.push_str("Assistant:");
        prompt
    }

    /// Parse a single line of Ollama CLI plain text output
    fn parse_line(&self, line: &str) -> Option<RunEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Skip spinner/animation lines
        if trimmed.starts_with("Thinking") || trimmed.starts_with("...done thinking") {
            return None;
        }

        // Skip ANSI escape sequences (spinner)
        if trimmed.chars().next().map_or(false, |c| c.is_control()) {
            return None;
        }

        // Treat everything else as text output
        Some(RunEvent::Text {
            text: trimmed.to_string() + "\n",
        })
    }
}

#[async_trait::async_trait]
impl CliProvider for OllamaCliProvider {
    fn id(&self) -> String {
        self.base.id()
    }

    fn bin(&self) -> Option<String> {
        self.base.bin()
    }

    fn models(&self) -> Vec<String> {
        self.base.models()
    }

    fn is_available(&self) -> bool {
        self.base.is_available()
    }

    fn set_available(&mut self, available: bool) {
        self.base.set_available(available);
    }

    async fn check_available(&self) -> bool {
        self.base.check_available().await
    }

    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        // Run `ollama list` to get available models dynamically in a blocking task
        let bin = self.bin.clone();
        
        let result = tokio::task::spawn_blocking(move || -> Result<Vec<String>, anyhow::Error> {
            let output = StdCommand::new(&bin)
                .arg("list")
                .output()?;

            if !output.status.success() {
                return Err(anyhow!("ollama list failed: {}", String::from_utf8_lossy(&output.stderr)));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut models = Vec::new();

            // Parse ollama list output (tabular format)
            // NAME                    ID              SIZE      MODIFIED
            // nemotron-3-nano:4b      abc123          2.5 GB    2 hours ago
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if !parts.is_empty() {
                    models.push(parts[0].to_string());
                }
            }

            Ok(models)
        }).await;

        match result {
            Ok(Ok(models)) => Ok(models),
            Ok(Err(e)) => {
                tracing::warn!("ollama scan_models failed: {}", e);
                Err(e)
            }
            Err(e) => {
                tracing::error!("ollama scan_models task join error: {}", e);
                Err(anyhow::anyhow!("task join error: {}", e))
            }
        }
    }

    async fn send_chat_completion(
        &self,
        model: &str,
        request: ChatCompletionRequest,
    ) -> Result<ProviderResponse> {
        // Strip ollama/ prefix if present
        let model = model.strip_prefix("ollama/").unwrap_or(model);

        let mut cmd = self.build_run_command(model, false);
        let mut child = cmd.spawn().context("Failed to spawn ollama run")?;

        // Send prompt to ollama via stdin (plain text, not JSON)
        let prompt = self.messages_to_prompt(&request.messages);

        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }

        // Read response from stdout with timeout
        let read_fut = async {
            let mut reader = BufReader::new(child.stdout.take().context("no stdout")?);
            let mut line = String::new();
            let mut full_content = String::new();
            let mut reasoning_content = String::new();
            let mut in_reasoning = false;

            while reader.read_line(&mut line).await? > 0 {
                let trimmed = line.trim();
                if let Some(event) = self.parse_line(trimmed) {
                    match event {
                        RunEvent::Text { text } => full_content.push_str(&text),
                        RunEvent::ReasoningDelta { text, .. } => {
                            if !in_reasoning {
                                in_reasoning = true;
                            }
                            reasoning_content.push_str(&text);
                        }
                        RunEvent::Done { .. } => break,
                        RunEvent::Error { message, .. } => {
                            return Err(anyhow!("Ollama error: {}", message));
                        }
                        _ => {}
                    }
                }
                line.clear();
            }

            // Wait for process to exit
            let _ = child.wait().await;

            Ok(ProviderResponse::Success {
                content: full_content,
                reasoning: if reasoning_content.is_empty() { None } else { Some(reasoning_content) },
                tool_calls: None,
                session_id: None,
                mcp_session_id: None,
            })
        };

        // Apply 60s timeout for fail-closed behavior
        match timeout(Duration::from_secs(60), read_fut).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                Err(anyhow!("Ollama CLI timeout after 60s"))
            }
        }
    }

    async fn send_chat_completion_stream(
        &self,
        model: &str,
        request: ChatCompletionRequest,
        tx: tokio::sync::mpsc::Sender<RunEvent>,
    ) -> Result<()> {
        // Strip ollama/ prefix if present
        let model = model.strip_prefix("ollama/").unwrap_or(model);

        let mut cmd = self.build_run_command(model, true);
        let mut child = cmd.spawn().context("Failed to spawn ollama run (stream)")?;

        // Send prompt to ollama via stdin (plain text, not JSON)
        let prompt = self.messages_to_prompt(&request.messages);

        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;
        }

        let read_fut = async {
            let mut reader = BufReader::new(child.stdout.take().context("no stdout")?);
            let mut line = String::new();
            let mut in_reasoning = false;
            let mut block = 0u32;

            while reader.read_line(&mut line).await? > 0 {
                let trimmed = line.trim();
                if let Some(event) = self.parse_line(trimmed) {
                    match event {
                        RunEvent::Text { text } => {
                            if in_reasoning {
                                in_reasoning = false;
                                let _ = tx.send(RunEvent::ReasoningEnd { block }).await;
                                block = block.wrapping_add(1);
                            }
                            let _ = tx.send(RunEvent::Text { text }).await;
                        }
                        RunEvent::ReasoningDelta { text, .. } => {
                            if !in_reasoning {
                                in_reasoning = true;
                                let _ = tx.send(RunEvent::ReasoningStart {
                                    block,
                                    kind: ReasoningKind::Raw,
                                }).await;
                            }
                            let _ = tx.send(RunEvent::ReasoningDelta { block, text }).await;
                        }
                        RunEvent::Done { .. } => {
                            if in_reasoning {
                                let _ = tx.send(RunEvent::ReasoningEnd { block }).await;
                            }
                            let _ = tx.send(RunEvent::Done {
                                reason: FinishReason::Stop,
                            }).await;
                            break;
                        }
                        RunEvent::Error { message, code } => {
                            let _ = tx.send(RunEvent::Error { message, code }).await;
                            break;
                        }
                        _ => {}
                    }
                }
                line.clear();
            }

            // Wait for process to exit
            let _ = child.wait().await;

            Ok(())
        };

        // Apply 300s timeout for streaming (matching native provider)
        match timeout(Duration::from_secs(300), read_fut).await {
            Ok(result) => result,
            Err(_) => {
                let _ = child.kill().await;
                let _ = tx.send(RunEvent::Error {
                    message: "Ollama CLI stream timeout after 300s".to_string(),
                    code: Some("timeout".to_string()),
                }).await;
                Err(anyhow!("Ollama CLI stream timeout after 300s"))
            }
        }
    }
}