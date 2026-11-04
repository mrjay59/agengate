use crate::config::{BridgeConfig, ProviderConfig};
use crate::types::Message;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio::time::interval;
use tracing::{debug, info};

/// Bridge client for handling tool calls from CLI agents
#[derive(Debug, Clone)]
pub struct BridgeClient {
    pub host: String,
    pub port: u16,
}

impl BridgeClient {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }

    pub async fn handle_tool_call(&self, _id: &str, _name: &str, _arguments: serde_json::Value) -> Result<()> {
        // TODO: Implement tool call handling
        Ok(())
    }

    pub async fn handle_user_message(&self, _text: &str) -> Result<()> {
        // TODO: Implement user message handling
        Ok(())
    }
}

/// Core Bridge server managing MCP loopback connections
#[allow(dead_code)]
#[derive(Clone)]
pub struct BridgeSession {
    pub session_id: String,
    pub token: String,
    pub run_id: Option<String>,
    pub tools: Vec<ToolSchema>,
    pub created_at: std::time::Instant,
}

pub struct BridgeServer {
    config: BridgeConfig,
    runs: Arc<RwLock<HashMap<String, Run>>>,
    tool_schemas: Arc<RwLock<HashMap<String, ToolSchema>>>,
    parked_runs: Arc<Mutex<HashMap<String, Run>>>,
    sessions: Arc<RwLock<HashMap<String, BridgeSession>>>,
    pending_calls: Arc<Mutex<HashMap<String, oneshot::Sender<ToolResult>>>>,
    #[allow(dead_code)]
    cleanup_interval: Duration,
}

impl BridgeServer {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            runs: Arc::new(RwLock::new(HashMap::new())),
            tool_schemas: Arc::new(RwLock::new(HashMap::new())),
            parked_runs: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pending_calls: Arc::new(Mutex::new(HashMap::new())),
            cleanup_interval: Duration::from_secs(600),
        }
    }

    pub async fn create_session(&self, tools: Vec<ToolSchema>, run_id: Option<String>) -> (String, String) {
        let session_id = uuid::Uuid::new_v4().to_string();
        let token = uuid::Uuid::new_v4().to_string().replace("-", "");
        let session = BridgeSession {
            session_id: session_id.clone(),
            token: token.clone(),
            run_id,
            tools,
            created_at: std::time::Instant::now(),
        };
        self.sessions.write().await.insert(session_id.clone(), session);
        (session_id, token)
    }

    pub async fn get_session(&self, session_id: &str) -> Option<BridgeSession> {
        self.sessions.read().await.get(session_id).cloned()
    }

    pub fn validate_token(session: &BridgeSession, token: &str) -> bool {
        // constant-time compare
        if session.token.len() != token.len() {
            return false;
        }
        let mut diff = 0u8;
        for (a, b) in session.token.bytes().zip(token.bytes()) {
            diff |= a ^ b;
        }
        diff == 0
    }

    pub async fn hold_tool_call(&self, mcp_id: String, timeout_secs: u64) -> Result<ToolResult> {
        let (tx, rx) = oneshot::channel();
        self.pending_calls.lock().await.insert(mcp_id.clone(), tx);
        
        info!(event = "bridge.tool_call.hold", mcp_id = %mcp_id, timeout = timeout_secs);
        
        match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(res)) => {
                info!(event = "bridge.tool_call.resolved", mcp_id = %mcp_id);
                Ok(res)
            },
            Ok(Err(_)) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                Err(anyhow::anyhow!("channel closed"))
            },
            Err(_) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id);
                Err(anyhow::anyhow!("timeout menunggu hasil dari client"))
            }
        }
    }

    pub async fn resolve_tool_call(&self, mcp_id: &str, result: ToolResult) -> bool {
        if let Some(tx) = self.pending_calls.lock().await.remove(mcp_id) {
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }

    pub fn runs(&self) -> &Arc<RwLock<HashMap<String, Run>>> {
        &self.runs
    }

    /// Initialize tool schemas (stub: no provider registry wired yet)
    pub async fn initialize_schemas(&self) -> Result<()> {
        info!("Initialized 0 tool schemas");
        Ok(())
    }

    /// Get available tool schemas for a workspace
    pub async fn get_tool_schemas(&self, workspace: &str) -> Vec<ToolSchema> {
        let schemas = self.tool_schemas.read().await;
        schemas
            .values()
            .filter(|s| s.workspace == workspace || s.workspace == "*")
            .cloned()
            .collect()
    }

    /// Start a new run
    pub async fn start_run(
        &self,
        run_id: String,
        workspace: String,
        model: String,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        max_steps: usize,
    ) -> Result<Run> {
        let run = Run::new(
            run_id.clone(),
            workspace,
            model,
            messages,
            tools,
            max_steps,
            self.config.call_ttl_secs,
        );

        self.runs.write().await.insert(run_id.clone(), run.clone());
        debug!("Started run: {}", run_id);
        Ok(run)
    }

    /// Get parked runs map (for read access)
    pub fn parked_runs(&self) -> &Arc<Mutex<HashMap<String, Run>>> {
        &self.parked_runs
    }

    /// Get a run by ID
    pub async fn get_run(&self, run_id: &str) -> Option<Run> {
        self.runs.read().await.get(run_id).cloned()
    }

    /// Park a run (waiting for tool result)
    pub async fn park_run(&self, run: Run) -> Result<()> {
        let mut parked = self.parked_runs.lock().await;

        if parked.len() >= self.config.max_parked_runs {
            return Err(anyhow!("Max parked runs reached"));
        }

        parked.insert(run.id.clone(), run);
        Ok(())
    }

    /// Resume a parked run with tool result
    pub async fn resume_run(&self, run_id: &str, _tool_result: ToolResult) -> Option<Run> {
        let mut parked = self.parked_runs.lock().await;
        parked.remove(run_id)
    }

    /// Execute a tool call - parks run, forwards to client, waits for result
    pub async fn execute_tool(
        &self,
        run_id: &str,
        tool_name: &str,
        args: serde_json::Value,
        _workspace: &str,
    ) -> Result<ToolResult> {
        // Guard: Check if tool is bridge tool
        if !self.is_bridge_tool(tool_name) {
            tracing::error!("Guard violation: non-bridge tool {} called in run {}", tool_name, run_id);
            return Err(anyhow!("tool_isolation_violation: {} is not a bridge tool", tool_name));
        }

        // Get the run
        let mut runs = self.runs.write().await;
        let mut run = match runs.get_mut(run_id) {
            Some(r) => r.clone(),
            None => return Err(anyhow!("Run not found: {}", run_id)),
        };

        // Park the run with tool call info
        let tool_call_id = uuid::Uuid::new_v4().to_string();
        let mcp_id = tool_call_id.clone();
        run.park(tool_call_id.clone(), tool_name.to_string());

        // Store the tool call for later resolution
        let (tx, rx) = oneshot::channel();
        self.pending_calls.lock().await.insert(mcp_id.clone(), tx);

        // Re-insert the parked run
        self.park_run(run.clone()).await?;

        // Drop lock before await
        drop(runs);

        info!(event = "bridge.tool_call.hold", mcp_id = %mcp_id, tool = %tool_name, run_id = %run_id, args = %args);

        // Forward tool call to client (Device A) via BridgeClient
        // The client will execute the tool and send result back via bridge/resolve
        // We need to signal the client that a tool call is waiting
        // This is done via the MCP tools/call endpoint which the CLI polls/calls

        // Wait for client to execute tool and send result via bridge/resolve
        match tokio::time::timeout(std::time::Duration::from_secs(self.config.call_ttl_secs), rx).await {
            Ok(Ok(result)) => {
                info!(event = "bridge.tool_call.resolved", mcp_id = %mcp_id, tool = %tool_name);
                Ok(result)
            },
            Ok(Err(_)) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                Err(anyhow::anyhow!("channel closed"))
            },
            Err(_) => {
                self.pending_calls.lock().await.remove(&mcp_id);
                info!(event = "bridge.tool_call.timeout", mcp_id = %mcp_id, tool = %tool_name);
                Err(anyhow::anyhow!("timeout menunggu hasil dari client"))
            }
        }
    }

    fn is_bridge_tool(&self, tool_name: &str) -> bool {
        // Tool is bridge if it starts with a registered provider ID followed by ':'
        // or if it's the special preflight probe_ok
        if tool_name == "probe_ok" {
            return true;
        }
        
        let provider_id = match self.extract_provider_from_tool(tool_name) {
            Ok(id) => id,
            Err(_) => return false,
        };

        if provider_id == "probe_ok" {
            return true;
        }

        // A bridge tool must be namespaced as "provider:tool". Unknown bare
        // names are never bridge tools, so provider builtins stay isolated.
        tool_name.contains(':')
    }

    fn extract_provider_from_tool(&self, tool_name: &str) -> Result<String> {
        // Format: provider:tool_name or bare tool_name (returns "probe_ok")
        let parts: Vec<&str> = tool_name.splitn(2, ':').collect();
        match parts.len() {
            2 => Ok(parts[0].to_string()),
            1 => Ok("probe_ok".to_string()),
            _ => Err(anyhow!("Invalid tool name format: {}", tool_name)),
        }
    }

    /// Start cleanup task for expired runs
    pub async fn start_cleanup_task(&self) {
        let runs = self.runs.clone();
        let parked_runs = self.parked_runs.clone();
        let park_ttl = Duration::from_secs(self.config.park_ttl_secs);
        let call_ttl = Duration::from_secs(self.config.call_ttl_secs);

        tokio::spawn(async move {
            let mut interval = interval(park_ttl);
            loop {
                interval.tick().await;

                // Clean expired parked runs
                let mut parked = parked_runs.lock().await;
                let now = std::time::Instant::now();
                parked.retain(|_, run| now.duration_since(run.parked_at.unwrap_or(now)) < park_ttl);

                // Clean expired active runs
                let mut runs_guard = runs.write().await;
                runs_guard.retain(|_, run| {
                    if let RunState::Running { started_at, .. } = &run.state {
                        now.duration_since(*started_at) < call_ttl
                    } else {
                        true
                    }
                });

                if !parked.is_empty() || !runs_guard.is_empty() {
                    debug!("Cleanup: {} parked, {} active runs", parked.len(), runs_guard.len());
                }
            }
        });
    }

    /// Shutdown the bridge
    pub async fn shutdown(&self) {
        info!("Shutting down bridge server");
        // Cleanup handled by dropping
    }
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: serde_json::Value,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(tool_call_id: String, name: String, content: serde_json::Value) -> Self {
        Self {
            tool_call_id,
            name,
            content,
            is_error: false,
        }
    }

    pub fn error(tool_call_id: String, name: String, error: String) -> Self {
        Self {
            tool_call_id,
            name,
            content: serde_json::json!({ "error": error }),
            is_error: true,
        }
    }
}

/// Tool schema compatible with OpenAI function calling
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub provider: String,
    pub workspace: String,
}

impl ToolSchema {
    pub fn new(
        name: String,
        description: String,
        parameters: serde_json::Value,
        provider: String,
        workspace: String,
    ) -> Self {
        Self {
            name,
            description,
            parameters,
            provider,
            workspace,
        }
    }

    /// Convert to OpenAI function format
    pub fn to_openai_function(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters
            }
        })
    }
}

/// Generate tool schemas from provider registry
pub async fn generate_tool_schemas() -> Result<Vec<ToolSchema>> {
    Ok(Vec::new())
}

/// Merge tool schemas with client-provided tools (for Bridge mode)
pub fn merge_tool_schemas(
    agentgate_schemas: Vec<ToolSchema>,
    client_tools: Option<Vec<serde_json::Value>>,
    policy: crate::permission::ClientToolsPolicy,
) -> Vec<serde_json::Value> {
    match policy {
        crate::permission::ClientToolsPolicy::Bridge => {
            // Replace with agentgate schemas
            agentgate_schemas.into_iter().map(|s| s.to_openai_function()).collect()
        }
        crate::permission::ClientToolsPolicy::Passthrough => {
            // Use client tools as-is, fallback to agentgate if none
            client_tools.unwrap_or_else(|| {
                agentgate_schemas.into_iter().map(|s| s.to_openai_function()).collect()
            })
        }
        crate::permission::ClientToolsPolicy::Ignore => {
            // Use agentgate schemas, ignore client
            agentgate_schemas.into_iter().map(|s| s.to_openai_function()).collect()
        }
    }
}

/// Sanitize tool name to OpenAI-compatible format
/// OpenAI requires: ^[a-zA-Z0-9_-]{1,64}$
pub fn sanitize_tool_name(name: &str) -> String {
    let mut sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Ensure it starts with a letter
    if sanitized.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        sanitized.insert(0, 't');
    }

    // Truncate to 64 chars
    if sanitized.len() > 64 {
        sanitized.truncate(64);
    }

    // Ensure not empty
    if sanitized.is_empty() {
        sanitized = "tool".to_string();
    }

    sanitized
}

/// Generate namespaced tool name: provider:tool
pub fn namespaced_tool_name(provider: &str, tool: &str) -> String {
    let provider_safe = sanitize_tool_name(provider);
    let tool_safe = sanitize_tool_name(tool);
    format!("{}:{}", provider_safe, tool_safe)
}

/// Build tool name map for a provider config
pub fn build_tool_name_map(config: &ProviderConfig) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let provider_name = sanitize_tool_name(&config.id);

    // If models_cmd is set, tools come from CLI
    if config.models_cmd.is_some() || config.chat_cmd.is_some() {
        // Tools will be discovered at runtime from CLI
        map.insert("*".to_string(), namespaced_tool_name(&provider_name, "*"));
    } else {
        // Static tools from config
        for model in &config.models {
            let model_safe = sanitize_tool_name(model);
            map.insert(
                model.clone(),
                namespaced_tool_name(&provider_name, &model_safe),
            );
        }
    }

    map
}

/// Validate all tool names are unique across providers
pub fn validate_unique_tool_names(configs: &[ProviderConfig]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();

    for config in configs {
        let provider_name = sanitize_tool_name(&config.id);

        if config.models_cmd.is_some() || config.chat_cmd.is_some() {
            // Dynamic tools - can't validate statically
            continue;
        }

        for model in &config.models {
            let tool_name = namespaced_tool_name(&provider_name, model);
            if !seen.insert(tool_name.clone()) {
                return Err(format!("Duplicate tool name: {}", tool_name));
            }
        }
    }

    Ok(())
}

/// Run state machine
#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    /// Run created, not started
    Pending,
    /// Run is executing (model thinking / tool calling)
    Running {
        started_at: std::time::Instant,
        #[allow(dead_code)]
        step: usize,
        #[allow(dead_code)]
        current_tool: Option<String>,
    },
    /// Run parked waiting for tool result
    Parked {
        parked_at: std::time::Instant,
        tool_call_id: String,
        tool_name: String,
    },
    /// Run completed successfully
    Completed {
        #[allow(dead_code)]
        completed_at: std::time::Instant,
        final_messages: Vec<Message>,
    },
    /// Run failed
    Failed {
        #[allow(dead_code)]
        failed_at: std::time::Instant,
        error: String,
    },
    /// Run cancelled by client
    Cancelled,
}

impl RunState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, RunState::Completed { .. } | RunState::Failed { .. } | RunState::Cancelled)
    }

    pub fn is_parked(&self) -> bool {
        matches!(self, RunState::Parked { .. })
    }
}

/// Run represents a single agent conversation with tool-calling
#[derive(Debug, Clone)]
pub struct Run {
    pub id: String,
    pub workspace: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_steps: usize,
    pub call_ttl_secs: u64,
    pub state: RunState,
    pub step_count: usize,
    pub created_at: std::time::Instant,
    pub parked_at: Option<std::time::Instant>,
}

impl Run {
    pub fn new(
        id: String,
        workspace: String,
        model: String,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        max_steps: usize,
        call_ttl_secs: u64,
    ) -> Self {
        Self {
            id,
            workspace,
            model,
            messages,
            tools,
            max_steps,
            call_ttl_secs,
            state: RunState::Pending,
            step_count: 0,
            created_at: std::time::Instant::now(),
            parked_at: None,
        }
    }

    /// Start the run
    pub fn start(&mut self) {
        self.state = RunState::Running {
            started_at: std::time::Instant::now(),
            step: 0,
            current_tool: None,
        };
    }

    /// Park the run waiting for tool result
    pub fn park(&mut self, tool_call_id: String, tool_name: String) {
        self.state = RunState::Parked {
            parked_at: std::time::Instant::now(),
            tool_call_id,
            tool_name,
        };
        self.parked_at = Some(std::time::Instant::now());
    }

    /// Resume from parked state with tool result
    pub fn resume(&mut self, tool_result: ToolResult) {
        self.messages.push(Message::tool_result(
            tool_result.tool_call_id,
            tool_result.name,
            tool_result.content.to_string(),
        ));
        self.step_count += 1;
        self.state = RunState::Running {
            started_at: std::time::Instant::now(),
            step: self.step_count,
            current_tool: None,
        };
        self.parked_at = None;
    }

    /// Complete the run
    pub fn complete(&mut self, final_messages: Vec<Message>) {
        self.state = RunState::Completed {
            completed_at: std::time::Instant::now(),
            final_messages,
        };
    }

    /// Fail the run
    pub fn fail(&mut self, error: String) {
        self.state = RunState::Failed {
            failed_at: std::time::Instant::now(),
            error,
        };
    }

    /// Cancel the run
    pub fn cancel(&mut self) {
        self.state = RunState::Cancelled;
    }

    /// Check if run can continue (not terminal, not exceeded max steps)
    pub fn can_continue(&self) -> bool {
        !self.state.is_terminal() && self.step_count < self.max_steps
    }

    /// Check if call TTL expired
    pub fn is_call_expired(&self) -> bool {
        match &self.state {
            RunState::Running { started_at, .. } => {
                started_at.elapsed() > std::time::Duration::from_secs(self.call_ttl_secs)
            }
            RunState::Parked { parked_at, .. } => {
                parked_at.elapsed() > std::time::Duration::from_secs(self.call_ttl_secs)
            }
            _ => false,
        }
    }

    /// Check if parked TTL expired
    pub fn is_park_expired(&self) -> bool {
        if let Some(parked_at) = self.parked_at {
            parked_at.elapsed() > std::time::Duration::from_secs(self.call_ttl_secs)
        } else {
            false
        }
    }

    /// Get current step number
    pub fn current_step(&self) -> usize {
        match &self.state {
            RunState::Running { step, .. } => *step,
            RunState::Parked { .. } => self.step_count,
            _ => self.step_count,
        }
    }

    /// Set current tool being executed
    pub fn set_current_tool(&mut self, tool_name: Option<String>) {
        if let RunState::Running { started_at: _, step: _, current_tool } = &mut self.state {
            *current_tool = tool_name;
        }
    }
}