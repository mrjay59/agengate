use crate::bridge::{Run, ToolSchema, ToolResult};
use crate::config::{BridgeConfig, RoutingConfig};
use crate::openai::{ToolChoice, ToolCallIdGenerator, tool_schema_to_openai, tool_schemas_to_tool_definitions};
use crate::permission::{ClientToolsPolicy, NoToolsPolicy, ToolActivityPolicy};
use crate::provider::CliProvider;
use crate::provider::ProviderResponse;
use crate::types::{ChatCompletionRequest, Message, MessageRole};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::info;

/// Preflight probe result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightResult {
    pub provider: String,
    pub version: String,
    pub supports_mcp: bool,
    pub supports_streaming: bool,
    pub probe_time_ms: u64,
}

/// Simple cache for preflight probe results
struct ProbeCache {
    data: Arc<RwLock<HashMap<String, PreflightResult>>>,
}

impl ProbeCache {
    fn new() -> Self {
        Self { data: Arc::new(RwLock::new(HashMap::new())) }
    }
    async fn get(&self, key: &str) -> Option<PreflightResult> {
        self.data.read().await.get(key).cloned()
    }
    async fn set(&self, key: String, value: PreflightResult) {
        self.data.write().await.insert(key, value);
    }
}

/// Request classification result
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestClass {
    /// Client sent tools, will use bridge mode
    HasClientTools,
    /// No client tools but agentgate has tools (bridge/native)
    HasAgentTools,
    /// No tools available, chat only
    ChatOnly,
}

/// Classify incoming request to determine tool handling mode
pub fn classify_request(
    request: &ChatCompletionRequest,
    agentgate_tools: &[ToolSchema],
    _routing: &RoutingConfig,
) -> RequestClass {
    let has_client_tools = request.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false);
    let has_agent_tools = !agentgate_tools.is_empty();

    if has_client_tools {
        RequestClass::HasClientTools
    } else if has_agent_tools {
        RequestClass::HasAgentTools
    } else {
        RequestClass::ChatOnly
    }
}

/// Determine tool handling mode from classification
pub fn determine_tool_mode(
    class: &RequestClass,
    routing: &RoutingConfig,
) -> ToolMode {
    match class {
        RequestClass::HasClientTools => {
            match routing.on_client_tools {
                ClientToolsPolicy::Bridge => ToolMode::Bridge,
                ClientToolsPolicy::Passthrough => ToolMode::Passthrough,
                ClientToolsPolicy::Ignore => ToolMode::Bridge, // Ignore client, use agentgate
                ClientToolsPolicy::Reject => ToolMode::Reject, // Fail-closed: reject client tools
            }
        }
        RequestClass::HasAgentTools => ToolMode::Bridge,
        RequestClass::ChatOnly => {
            match routing.no_tools_policy {
                NoToolsPolicy::ChatOnly => ToolMode::ChatOnly,
                NoToolsPolicy::Bridge => ToolMode::Bridge,
                NoToolsPolicy::Reject => ToolMode::Reject,
            }
        }
    }
}

/// Classified request per spec §7.2
#[derive(Debug, Clone)]
pub enum Classified {
    Native,
    Resume { call_ids: Vec<String> },
    Replay { history: Vec<Message> },
    NewBridge { tools: Vec<crate::types::ToolDefinition> },
    NewChatOnly,
}

/// Extract trailing tool messages (spec §7.2)
pub fn trailing_tool_messages(messages: &[Message]) -> Vec<Message> {
    let mut out = Vec::new();
    for m in messages.iter().rev() {
        if m.role == MessageRole::Tool {
            out.push(m.clone());
        } else {
            break;
        }
    }
    out.reverse();
    out
}

/// Classify request per spec §7.2
pub fn classify(
    request: &ChatCompletionRequest,
    _agentgate_tools: &[crate::bridge::ToolSchema],
    is_native_provider: bool,
    known_ids: &HashSet<String>,
) -> Classified {
    if is_native_provider {
        return Classified::Native;
    }
    let trailing = trailing_tool_messages(&request.messages);
    if !trailing.is_empty() {
        let ids: Vec<String> = trailing.iter().filter_map(|m| m.tool_call_id.clone()).collect();
        if !ids.is_empty() && ids.iter().all(|id| known_ids.contains(id)) {
            return Classified::Resume { call_ids: ids };
        }
        info!(event = "run.replay", message_count = request.messages.len());
        return Classified::Replay { history: request.messages.clone() };
    }
    if let Some(tools) = &request.tools {
        if !tools.is_empty() && request.tool_choice.as_ref().map(|v| v != &serde_json::json!("none")).unwrap_or(true) {
            return Classified::NewBridge { tools: tools.clone() };
        }
    }
    Classified::NewChatOnly
}

/// Tool handling mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolMode {
    ChatOnly,
    Bridge,
    Passthrough,
    Native,
    Reject,
}

/// Run registry managing all active runs
#[derive(Clone)]
pub struct RunRegistry {
    bridge: Arc<crate::bridge::BridgeServer>,
    config: BridgeConfig,
    #[allow(dead_code)]
    tool_activity: ToolActivityPolicy,
    tool_choice_generator: Arc<Mutex<ToolCallIdGenerator>>,
}

impl RunRegistry {
    pub fn new(bridge: Arc<crate::bridge::BridgeServer>, config: BridgeConfig, tool_activity: ToolActivityPolicy) -> Self {
        Self {
            bridge,
            config,
            tool_activity,
            tool_choice_generator: Arc::new(Mutex::new(ToolCallIdGenerator::new(
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0),
    "run",
))),
        }
    }

    /// Create and start a new run
    pub async fn create_run(
        &self,
        workspace: String,
        model: String,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        _tool_choice: ToolChoice,
    ) -> Result<crate::bridge::Run> {
        let run_id = {
            let mut gen = self.tool_choice_generator.lock().unwrap();
            gen.generate()
        };
        let max_steps = self.config.max_steps;

        let run = self.bridge.start_run(
            run_id,
            workspace,
            model,
            messages,
            tools,
            max_steps,
        ).await?;

        Ok(run)
    }

    /// Get runs map (for read access)
    pub fn runs(&self) -> &Arc<RwLock<HashMap<String, Run>>> {
        self.bridge.runs()
    }

    /// Get a run by ID
    pub async fn get_run(&self, run_id: &str) -> Option<Run> {
        self.bridge.get_run(run_id).await
    }

    /// Execute a step in the run (call model, handle tool calls)
    pub async fn step_run(
        &self,
        run_id: &str,
        provider: &dyn CliProvider,
    ) -> Result<RunStepResult> {
        let mut run = self.get_run(run_id).await
            .ok_or_else(|| anyhow::anyhow!("Run not found: {}", run_id))?;

        if !run.can_continue() {
            return Ok(RunStepResult::Completed);
        }

        // Check if parked waiting for tool result
        if run.state.is_parked() {
            return Ok(RunStepResult::Parked);
        }

        // Call provider with current messages and tools
        let response = match provider.send_chat_completion(&run.model, ChatCompletionRequest {
            model: run.model.clone(),
            messages: run.messages.clone(),
            tools: Some(tool_schemas_to_tool_definitions(&run.tools)),
            tool_choice: Some(serde_json::Value::String("auto".to_string())),
            ..Default::default()
        }).await {
            Ok(r) => r,
            Err(e) => return Ok(RunStepResult::Failed(format!("Provider error: {}", e))),
        };

        match response {
            ProviderResponse::Success { content: text, reasoning, tool_calls: _, session_id: _ } => {
                let msg = Message {
                    role: MessageRole::Assistant,
                    content: Some(text),
                    reasoning_content: reasoning,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                };
                run.complete(vec![msg]);
            }
            ProviderResponse::Error(e) => {
                return Ok(RunStepResult::Failed(e));
            }
        }
        self.bridge.runs().write().await.insert(run_id.to_string(), run);
        Ok(RunStepResult::Completed)
    }

    /// Resume a parked run with tool result
    pub async fn resume_run(&self, run_id: &str, tool_result: ToolResult) -> Result<()> {
        let mut run = self.get_run(run_id).await
            .ok_or_else(|| anyhow::anyhow!("Run not found: {}", run_id))?;

        run.resume(tool_result);
        self.bridge.runs().write().await.insert(run_id.to_string(), run);
        Ok(())
    }

    /// Preflight probe: check provider CLI version and capabilities
    /// Caches results per provider version to avoid repeated calls
    pub async fn preflight_probe(&self, provider_id: &str) -> Result<PreflightResult> {
        static CACHE: OnceLock<ProbeCache> = OnceLock::new();
        let cache = CACHE.get_or_init(ProbeCache::new);

        let cache_key = format!("{}:{}", provider_id, "default");

        // Check cache first
        if let Some(cached) = cache.get(&cache_key).await {
            return Ok(cached);
        }

        let start = std::time::Instant::now();
        let (version, supports_mcp, supports_streaming) = match provider_id {
            "opencode" => {
                let output = tokio::process::Command::new("opencode")
                    .arg("--version")
                    .output().await?;
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (version, true, true)
            }
            "gemini" => {
                let output = tokio::process::Command::new("gemini")
                    .arg("--version")
                    .output().await?;
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (version, true, true)
            }
            "claude" => {
                let output = tokio::process::Command::new("claude")
                    .arg("--version")
                    .output().await?;
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                (version, true, true)
            }
            _ => {
                return Ok(PreflightResult {
                    provider: provider_id.to_string(),
                    version: "unknown".into(),
                    supports_mcp: false,
                    supports_streaming: false,
                    probe_time_ms: start.elapsed().as_millis() as u64,
                });
            }
        };

        let result = PreflightResult {
            provider: provider_id.to_string(),
            version,
            supports_mcp,
            supports_streaming,
            probe_time_ms: start.elapsed().as_millis() as u64,
        };

        cache.set(cache_key, result.clone()).await;
        Ok(result)
    }

/// Get all known tool call IDs from active runs (for resume/replay)
    pub async fn get_known_tool_call_ids(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        let runs = self.bridge.runs().read().await;
        for run in runs.values() {
            for msg in &run.messages {
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        if !tc.id.is_empty() {
                            ids.insert(tc.id.clone());
                        }
                    }
                }
            }
        }
        ids
    }

    /// Cancel a run
    pub async fn cancel_run(&self, run_id: &str) -> Result<()> {
        let mut run = self.get_run(run_id).await
            .ok_or_else(|| anyhow::anyhow!("Run not found: {}", run_id))?;

        run.cancel();
        self.bridge.runs().write().await.insert(run_id.to_string(), run);
        Ok(())
    }
}

/// Result of a run step
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStepResult {
    Continue,
    Parked,
    Completed,
    Failed(String),
}

/// System prompt builder for tool-calling
#[derive(Clone)]
pub struct PromptBuilder {
    tool_schemas: Vec<ToolSchema>,
    #[allow(dead_code)]
    tool_activity: ToolActivityPolicy,
}

impl PromptBuilder {
    pub fn new(tool_schemas: Vec<ToolSchema>, tool_activity: ToolActivityPolicy) -> Self {
        Self {
            tool_schemas,
            tool_activity,
        }
    }

    pub fn with_custom_instructions(self, instructions: String) -> Self {
        // ponytail: no-op placeholder, replace when PromptBuilder supports custom instructions
        let _ = instructions;
        self
    }

    /// Build system prompt with tool definitions
    pub fn build_system_prompt(&self) -> String {
        let mut prompt = String::new();

        prompt.push_str("You are an AI assistant with access to tools.\n\n");

        if !self.tool_schemas.is_empty() {
            prompt.push_str("Available tools:\n");
            for schema in &self.tool_schemas {
                prompt.push_str(&format!("- {}: {}\n", schema.name, schema.description));
                if let Some(props) = schema.parameters.get("properties") {
                    if let Some(obj) = props.as_object() {
                        for (prop_name, prop_schema) in obj {
                            let desc = prop_schema.get("description")
                                .and_then(|d| d.as_str())
                                .unwrap_or("");
                            prompt.push_str(&format!("  - {}: {}\n", prop_name, desc));
                        }
                    }
                }
            }
            prompt.push_str("\n");
        }

        prompt.push_str("When you need to use a tool, respond with a tool call in the appropriate format.\n");

        prompt
    }

    /// Build tool descriptions for OpenAI function calling
    pub fn build_tool_definitions(&self) -> Vec<serde_json::Value> {
        self.tool_schemas.iter().map(tool_schema_to_openai).collect()
    }
}

/// Request batching for multiple tool calls
pub struct RequestBatcher {
    max_batch_size: usize,
    pending: Vec<BatchedRequest>,
}

#[derive(Debug, Clone)]
pub struct BatchedRequest {
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

impl RequestBatcher {
    pub fn new(max_batch_size: usize) -> Self {
        Self {
            max_batch_size,
            pending: Vec::new(),
        }
    }

    pub fn add(&mut self, req: BatchedRequest) {
        self.pending.push(req);
    }

    pub fn take_batch(&mut self) -> Vec<BatchedRequest> {
        let batch_size = self.max_batch_size.min(self.pending.len());
        let batch = self.pending.drain(..batch_size).collect::<Vec<_>>();
        if !batch.is_empty() {
            info!(event = "batch.issued", batch_size = %batch.len());
        }
        batch
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }
}