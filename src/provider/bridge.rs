//! Provider bridge trait — adapter layer that connects
//! CLI agents (OpenCode, Gemini, Claude) to AgentGate's MCP bridge server.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::Mutex;

/// MCP server configuration for bridge connection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// MCP configuration for CLI agents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

// ===========================================================================
// Schema / result types (V2 - per spec_tooling.md)
// ===========================================================================

/// Bridge configuration for starting a bridge session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub host: String,
    pub port: u16,
    pub model: String,
    pub profile_name: String,
    /// Base directory for session workdirs (default: /tmp/agentgate)
    #[serde(default = "default_data_dir")]
    pub data_dir: Option<PathBuf>,
}

fn default_data_dir() -> Option<PathBuf> {
    Some(PathBuf::from("/tmp/agentgate"))
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            model: "default".to_string(),
            profile_name: "opencode".to_string(),
            data_dir: default_data_dir(),
        }
    }
}

/// Session identifier
pub type SessionId = String;

/// Bridge profile specification (data-driven configuration)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeProfileSpec {
    pub name: String,
    pub display_name: String,
    pub binary: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub mcp_config_template: Option<String>,
    pub tool_schema_format: String,
    pub supports_streaming: bool,
    pub guard_patterns: Vec<String>,
}

/// Bridge profile (runtime)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeProfile {
    pub name: String,
    pub display_name: String,
    pub binary: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub mcp_config_template: Option<String>,
    pub tool_schema_format: String,
    pub supports_streaming: bool,
    pub guard_patterns: Vec<String>,
}

/// Tool call from bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool result to bridge
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeToolResult {
    pub tool_call_id: String,
    pub result: serde_json::Value,
    pub is_error: bool,
}

/// Output from a bridge session completion
#[derive(Debug, Clone, Default)]
pub struct SessionOutput {
    pub content: String,
    pub reasoning: Option<String>,
}

/// Bridge session trait for managing CLI bridge processes
#[async_trait]
pub trait BridgeSession: Send + Sync {
    fn session_id(&self) -> &SessionId;
    fn provider_profile(&self) -> &BridgeProfile;
    async fn send_user_message(&self, text: &str) -> Result<()>;
    async fn send_tool_result(&self, result: BridgeToolResult) -> Result<()>;
    async fn close(&self) -> Result<()>;
    async fn wait_for_completion(&self) -> Result<SessionOutput>;
}

/// Bridge provider factory trait
#[async_trait]
pub trait BridgeProviderFactory: Send + Sync {
    fn provider_name(&self) -> &str;
    async fn create(&self, config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>>;
    fn default_profile(&self) -> BridgeProfileSpec;
}

/// New BridgeProvider trait (V2) - per spec_tooling.md
#[async_trait]
pub trait BridgeProviderV2: Send + Sync {
    fn profile(&self) -> &BridgeProfile;
    async fn start_bridge(&self, config: BridgeConfig, client: Arc<crate::bridge::BridgeClient>) -> Result<Box<dyn BridgeSession>>;
    async fn health_check(&self) -> Result<bool>;
}

// ===========================================================================
// Provider modules
// ===========================================================================

pub mod opencode;
pub mod gemini;
pub mod claude;
pub mod ollama_cli;

// ===========================================================================
// Factory functions
// ===========================================================================

pub use opencode::OpenCodeBridgeFactory;
pub use gemini::GeminiBridgeFactory;
pub use claude::ClaudeBridgeFactory;
pub use ollama_cli::OllamaCliBridgeFactory;

/// Create a V2 bridge provider factory by name
pub fn create_bridge_factory_v2(provider_type: &str) -> Option<Box<dyn BridgeProviderFactory>> {
    match provider_type {
        "opencode" => Some(Box::new(OpenCodeBridgeFactory)),
        "gemini" => Some(Box::new(GeminiBridgeFactory)),
        "claude" => Some(Box::new(ClaudeBridgeFactory)),
        "ollama-cli" => Some(Box::new(OllamaCliBridgeFactory)),
        _ => None,
    }
}

/// Create a V2 bridge provider by name
pub async fn create_bridge_provider_v2(provider_type: &str, config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>> {
    if let Some(factory) = create_bridge_factory_v2(provider_type) {
        factory.create(config).await
    } else {
        Err(anyhow::anyhow!("Unknown bridge provider: {}", provider_type))
    }
}
