//! Provider bridge trait — adapter layer that connects
//! CLI agents (OpenCode, Gemini, Claude) to AgentGate's MCP bridge server.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

use crate::bridge::ContinuationOutput;

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
    /// Session id registered in `BridgeServer` (spec §9.2). When present, the
    /// adapter must reuse it instead of generating its own id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Bearer token registered in `BridgeServer` for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    /// Ollama CLI keep-alive (spec_ollama-cli.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<String>,
    /// Enable prompt-based tool calling for CLIs without native tool support.
    #[serde(default)]
    pub prompt_tools_fallback: bool,
    /// Transport type: "http" (default) or "stdio_shim"
    #[serde(default)]
    pub transport: Option<String>,
    /// Tool schemas for this session (used by stdio_shim transport)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<crate::bridge::ToolSchema>,
}

fn default_data_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir().join("agentgate"))
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            model: "default".to_string(),
            profile_name: "opencode".to_string(),
            data_dir: default_data_dir(),
            session_id: None,
            session_token: None,
            keep_alive: None,
            prompt_tools_fallback: false,
            transport: Some("http".to_string()),
            tools: Vec::new(),
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
    pub tool_calls: Option<Vec<crate::types::ToolCall>>,
}

/// Shared preflight isolation check (spec §13.2).
///
/// Verifies, before a tool-capable bridge turn is admitted, that the CLI
/// adapter has guard/isolation configuration in place. Fails closed (`Ok(false)`)
/// when no guard patterns are configured, i.e. when built-in tool isolation
/// could not be verified for the session.
pub async fn preflight_isolation_check(
    profile: &BridgeProfile,
    session_id: &str,
    source: &str,
) -> Result<bool> {
    let guard_patterns = &profile.guard_patterns;
    if guard_patterns.is_empty() {
        warn!(target: "bridge.preflight", session_id, "No guard patterns configured");
        return Ok(false);
    }

    info!(
        target: "bridge.preflight",
        session_id,
        source,
        "Preflight isolation check completed - CLI should have built-in tools disabled via config injection"
    );
    info!(
        target: "bridge.preflight",
        session_id,
        source,
        isolation_status = "config_injected",
        note = "Built-in tools should be disabled via CLI config injection"
    );
    Ok(true)
}

/// PR-S3: Continuation capability for Stage 2 live parked-session resume (§10.4).
/// Default is Unsupported; adapters/flows opt in when they can keep the session
/// alive across a tool-call boundary and continue without respawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationCapability {
    Unsupported,
    /// Session can accept tool results and produce a continued completion (best-effort).
    ToolResultContinuation,
}

/// Opaque continuation token/handle metadata (PR-S3 seam). Actual live continuation
/// wiring is implemented in PR-S4; this type allows capability-gated plumbing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationHandle {
    pub session_id: SessionId,
    pub provider: String,
    pub capability: ContinuationCapability,
}

/// Bridge session trait for managing CLI bridge processes
#[async_trait]
pub trait BridgeSession: Send + Sync + std::any::Any {
    fn session_id(&self) -> &SessionId;
    fn provider_profile(&self) -> &BridgeProfile;
    async fn send_user_message(&self, text: &str) -> Result<()>;
    async fn send_tool_result(&self, result: BridgeToolResult) -> Result<()>;
    async fn close(&self) -> Result<()>;
    async fn wait_for_completion(&self) -> Result<SessionOutput>;
    /// Stream events from the CLI in real-time for true streaming responses.
    /// Returns a receiver that yields events as they arrive from stdout.
    async fn take_event_receiver(&self) -> Result<Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>>;
    /// Preflight isolation check before a tool-capable turn is admitted
    /// (spec §13.2). Adapters override with CLI-specific verification; the
    /// default accepts (caller is expected to gate on tools separately).
    async fn preflight_isolation_check(&self) -> Result<bool> {
        Ok(true)
    }

    /// Check if the underlying CLI process/session is still alive.
    /// Used for deterministic replay-on-lost (spec §10.5).
    async fn check_liveness(&self) -> Result<bool> {
        Ok(true) // Default: assume alive; adapters can override with process check
    }

    /// PR-S3: continuation capability probe (default Unsupported).
    fn continuation_capability(&self) -> ContinuationCapability {
        ContinuationCapability::Unsupported
    }

    /// PR-S3: continuation handle for PR-S4 live resume. Returns None when unsupported.
    fn continuation_handle(&self) -> Option<ContinuationHandle> {
        None
    }

    /// PR-S4: wait for continuation output after MCP hold resolution.
    /// Only implemented by adapters that support live resume (e.g., opencode).
    async fn wait_for_continuation(&self) -> Result<ContinuationOutput> {
        Err(anyhow!("continuation not supported"))
    }
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

pub mod spike;

pub mod shim;

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
        "ollama" | "ollama-cli" => Some(Box::new(OllamaCliBridgeFactory)),
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
