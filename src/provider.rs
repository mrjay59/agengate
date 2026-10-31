use crate::config::ProviderConfig;
use crate::types::ChatCompletionRequest;
use async_trait::async_trait;
use std::process::Command;

#[derive(Debug, Clone)]
pub enum ProviderResponse {
    Success(String),
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
                if bin.to_lowercase().ends_with(ext) {
                    continue;
                }
                let candidate = format!("{}{}", bin, ext);
                if which::which(&candidate).is_ok() {
                    return true;
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
        // Prepare command args
        let cmd_template = match &self.chat_cmd {
            Some(c) => c.clone(),
            None => return Err(anyhow::anyhow!("No chat_cmd defined for provider {}", self.id)),
        };
        // Build prompt from messages
        let prompt = request
            .messages
            .into_iter()
            .map(|msg| msg.content)
            .collect::<Vec<String>>()
            .join("\n");

        let use_stdin = cmd_template.contains(&"--prompt-stdin".to_string()) || !cmd_template.iter().any(|arg| arg.contains("{prompt}"));

        // Replace placeholders {model} and {prompt}
        let cmd_args: Vec<String> = cmd_template
            .into_iter()
            .map(|arg| {
                let mut a = arg.replace("{model}", model);
                a = a.replace("{prompt}", &prompt);
                a
            })
            .collect();

        let bin = self.bin.as_ref().ok_or_else(|| anyhow::anyhow!("No binary configured for provider {}", self.id))?;

        // Spawn process
        let mut child = {
            let bin_path = if std::path::Path::new(bin).exists() {
                Some(bin.to_string())
            } else if cfg!(target_os = "windows") {
                let mut found = None;
                for ext in &[".cmd", ".bat", ".exe", ".ps1"] {
                    if bin.to_lowercase().ends_with(ext) { continue; }
                    let candidate = format!("{}{}", bin, ext);
                    if which::which(&candidate).is_ok() {
                        found = Some(candidate);
                        break;
                    }
                }
                found
            } else if which::which(bin).is_ok() {
                Some(bin.to_string())
            } else {
                None
            };

            match bin_path {
                Some(b) => {
                    let mut cmd = if b.contains(".cmd") || b.contains(".bat") || b.contains(".ps1") {
                        let mut c = Command::new("cmd");
                        c.arg("/C").arg(&b);
                        c
                    } else {
                        Command::new(&b)
                    };
                    cmd.args(&cmd_args)
                        .stdin(if use_stdin { std::process::Stdio::piped() } else { std::process::Stdio::null() })
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()?
                }
                None => return Err(anyhow::anyhow!("Binary not found for provider {}", self.id)),
            }
        };

        // Write prompt to stdin if required
        if use_stdin {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(prompt.as_bytes())?;
            }
        }

        let output = child.wait_with_output()?;
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
        Ok(ProviderResponse::Success(cleaned.trim().to_string()))
    }
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