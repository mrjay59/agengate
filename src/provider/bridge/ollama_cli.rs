//! Ollama CLI bridge provider — V2 adapter implementing spec_ollama-cli.md (RS9).

use crate::bridge::{BridgeClient, Run, ToolResult, ToolSchema};
use crate::provider::bridge::{
    BridgeConfig, BridgeProfile, BridgeProviderFactory, BridgeProviderV2, BridgeSession,
    BridgeToolResult, McpConfig, McpServerConfig, ToolCall,
};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

/// Ollama CLI bridge provider (V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaCliBridgeProvider {
    profile: BridgeProfile,
}

impl OllamaCliBridgeProvider {
    pub fn new() -> Self {
        Self {
            profile: BridgeProfile {
                name: "ollama-cli".into(),
                display_name: "Ollama CLI".into(),
                binary: "ollama".into(),
                args: vec!["run".into()],
                env: HashMap::new(),
                mcp_config_template: None,
                tool_schema_format: "openai".into(),
                supports_streaming: true,
                guard_patterns: vec![],
            },
        }
    }
}

impl Default for OllamaCliBridgeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BridgeProviderV2 for OllamaCliBridgeProvider {
    fn profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn start_bridge(
        &self,
        config: BridgeConfig,
        client: Arc<BridgeClient>,
    ) -> Result<Box<dyn BridgeSession>> {
        Ok(Box::new(OllamaCliSession::new(config, client, self.profile.clone())))
    }

    async fn health_check(&self) -> Result<bool> {
        // Check if ollama binary is available
        Ok(which::which("ollama").is_ok())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaCliSession {
    config: BridgeConfig,
    #[allow(dead_code)]
    client: Arc<BridgeClient>,
    profile: BridgeProfile,
    session_id: String,
    #[allow(dead_code)]
    process: Arc<Mutex<Option<Child>>>,
    closed: Arc<Mutex<bool>>,
}

impl OllamaCliSession {
    fn new(config: BridgeConfig, client: Arc<BridgeClient>, profile: BridgeProfile) -> Self {
        Self {
            config,
            client,
            profile,
            session_id: uuid::Uuid::new_v4().to_string(),
            process: Arc::new(Mutex::new(None)),
            closed: Arc::new(Mutex::new(false)),
        }
    }

    async fn spawn_process(&self) -> Result<Child> {
        let mut cmd = tokio::process::Command::new(&self.profile.binary);
        cmd.args(&self.profile.args)
            .arg(&self.config.model)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(&self.profile.env);

        let child = cmd.spawn()?;
        Ok(child)
    }
}

#[async_trait]
impl BridgeSession for OllamaCliSession {
    fn session_id(&self) -> &String {
        &self.session_id
    }

    fn provider_profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn send_user_message(&self, text: &str) -> Result<()> {
        let mut process_guard = self.process.lock().await;
        if process_guard.is_none() {
            *process_guard = Some(self.spawn_process().await?);
        }

        if let Some(child) = process_guard.as_mut() {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(text.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
            }
        }
        Ok(())
    }

    async fn send_tool_result(&self, result: BridgeToolResult) -> Result<()> {
        let json = serde_json::json!({
            "tool_call_id": result.tool_call_id,
            "result": result.result,
            "is_error": result.is_error,
        });
        self.send_user_message(&json.to_string()).await
    }

    async fn close(&self) -> Result<()> {
        *self.closed.lock().await = true;
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }

    async fn wait_for_completion(&self) -> Result<()> {
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            let _ = child.wait().await;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct OllamaCliBridgeFactory;

#[async_trait]
impl BridgeProviderFactory for OllamaCliBridgeFactory {
    fn provider_name(&self) -> &str {
        "ollama-cli"
    }

    async fn create(&self, _config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>> {
        Ok(Box::new(OllamaCliBridgeProvider::new()))
    }

    fn default_profile(&self) -> crate::provider::bridge::BridgeProfileSpec {
        crate::provider::bridge::BridgeProfileSpec {
            name: "ollama-cli".into(),
            display_name: "Ollama CLI".into(),
            binary: "ollama".into(),
            args: vec!["run".into()],
            env: HashMap::new(),
            mcp_config_template: None,
            tool_schema_format: "openai".into(),
            supports_streaming: true,
            guard_patterns: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_name() {
        let f = OllamaCliBridgeFactory;
        assert_eq!(f.provider_name(), "ollama-cli");
    }

    #[tokio::test]
    async fn health_check_when_not_installed() {
        let provider = OllamaCliBridgeProvider::new();
        // Should not panic even if ollama is not installed
        let _ = provider.health_check().await;
    }
}