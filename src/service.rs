use crate::bridge::BridgeServer;
use crate::config::{BridgeConfig, ReasoningConfig, RoutingConfig};
use crate::error::AppError;
use crate::config::ProviderConfig;
use crate::provider::{CliProvider};
use crate::provider::ollama_native::OllamaNativeProvider;
use crate::reasoning::RunEvent;
use crate::run::classify;
use std::collections::HashSet;
use crate::run::RunRegistry;
use crate::types::{AddProviderRequest, ChatCompletionRequest, ChatCompletionResponse, Model, ModelsResponse, ToolCall};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc, Semaphore};
use tracing::info;

/// Serial-mode gate (spec §10.3, `parallel_tool_calls: false`): keep at most
/// one tool call per assistant turn. The client then executes them one at a
/// time; the model re-issues the rest on subsequent turns with history
/// intact.
///
/// Applies to any tool-call list (bridge `ProviderResponse::Success.tool_calls`
/// and native responses alike). In serial mode a batch of N calls is truncated
/// to the first call so the run only ever parks one open tool call at a time.
pub fn truncate_tool_calls(calls: &mut Option<Vec<ToolCall>>, parallel_tool_calls: Option<bool>) {
    if parallel_tool_calls != Some(false) {
        return;
    }
    if let Some(calls) = calls.as_mut() {
        if calls.len() > 1 {
            calls.truncate(1);
        }
    }
}

/// Serial-mode gate on a full native response (spec §10.3).
pub fn truncate_parallel_tool_calls(
    resp: &mut crate::types::ChatCompletionResponse,
    parallel_tool_calls: Option<bool>,
) {
    for choice in resp.choices.iter_mut() {
        truncate_tool_calls(&mut choice.message.tool_calls, parallel_tool_calls);
    }
}

/// Map a provider error to the API error envelope, keeping the spec §7.6
/// error semantics (e.g. bridge isolation failures surface as 503
/// `tool_isolation_unavailable`).
fn provider_error_to_app_error(e: anyhow::Error) -> AppError {
    let msg = e.to_string();
    if msg.contains("tool_isolation_unavailable") {
        AppError::ToolIsolationUnavailable
    } else {
        AppError::Internal(msg)
    }
}

pub struct ServiceRegistry {
    providers: RwLock<HashMap<String, Arc<dyn CliProvider + Send + Sync>>>,
    add_providers: RwLock<Vec<AddProviderRequest>>,
    pub bridge: Arc<BridgeServer>,
    pub run_registry: RunRegistry,
    pub routing: RoutingConfig,
    pub bridge_config: BridgeConfig,
    /// Global reasoning configuration (`[reasoning]` in agentgate.toml).
    pub reasoning: ReasoningConfig,
    /// When true, Ollama requests are served by the CLI bridge instead of the
    /// HTTP native proxy (`ollama_mode = "cli"`).
    pub ollama_cli_mode: bool,
    /// Semaphore to limit concurrent runs per provider (spec §10.3).
    pub provider_semaphores: RwLock<HashMap<String, Arc<Semaphore>>>,
    /// Default max concurrency per provider from config.
    pub max_concurrency_per_provider: usize,
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        let bridge_config = BridgeConfig::default();
        let bridge = Arc::new(BridgeServer::new(bridge_config.clone()));
        let run_registry = RunRegistry::new(bridge.clone(), bridge_config.clone(), crate::permission::ToolActivityPolicy::default());
        let defaults = crate::config::DefaultsConfig::default();
        Self {
            providers: RwLock::new(HashMap::new()),
            add_providers: RwLock::new(Vec::new()),
            bridge,
            run_registry,
            routing: RoutingConfig::default(),
            bridge_config,
            reasoning: ReasoningConfig::default(),
            ollama_cli_mode: false,
            provider_semaphores: RwLock::new(HashMap::new()),
            max_concurrency_per_provider: defaults.max_concurrency_per_provider,
        }
    }
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the global reasoning configuration.
    pub fn with_reasoning(mut self, reasoning: ReasoningConfig) -> Self {
        self.reasoning = reasoning;
        self
    }

    pub fn reasoning_config(&self) -> &ReasoningConfig {
        &self.reasoning
    }

    /// Select CLI bridge for Ollama instead of the HTTP native proxy.
    pub fn with_ollama_cli_mode(mut self, enabled: bool) -> Self {
        self.ollama_cli_mode = enabled;
        self
    }

    /// Resolve the provider for a model without spawning model-discovery commands.
    pub async fn provider_for_model(&self, model: &str) -> Option<String> {
        if model.starts_with("ollama/") {
            return Some("ollama".to_string());
        }
        let providers = self.providers.read().await;
        if let Some((id, _)) = providers
            .iter()
            .find(|(_, p)| p.models().iter().any(|m| m == model))
        {
            return Some(id.clone());
        }
        // Common OpenAI-compatible convention: "provider/model".
        if let Some((prefix, _)) = model.split_once('/') {
            if providers.contains_key(prefix) {
                return Some(prefix.to_string());
            }
        }
        if providers.contains_key(model) {
            return Some(model.to_string());
        }
        None
    }

    pub async fn register(&mut self, mut provider: Box<dyn CliProvider + Send + Sync>) {
        let id = provider.id();
        info!("Registering provider: {}", id);
        let available = provider.check_available().await;
        provider.set_available(available);
        if available {
            info!("Provider {} is available", id);
        } else {
            info!("Provider {} is NOT available", id);
        }
        let mut providers = self.providers.write().await;
        providers.insert(id, Arc::from(provider));
    }

    pub async fn list_models(&self, _provider_filter: Option<&str>) -> Result<ModelsResponse, AppError> {
        let providers = self.providers.read().await;
        let mut models = Vec::new();

        for (id, provider) in providers.iter() {
            if let Ok(model_list) = provider.scan_models() {
                for model_id in model_list {
                    // Prefix model ID with provider ID (e.g., "ollama/qwen2.5-coder:7b")
                    let prefixed_id = if model_id.contains('/') {
                        model_id.clone() // Already has provider prefix
                    } else {
                        format!("{}/{}", id, model_id)
                    };
                    models.push(Model {
                        id: prefixed_id,
                        provider: id.clone(),
                        name: model_id.clone(),
                    });
                }
            }
        }

        // Ollama native provider: include models from /api/tags
        if providers.contains_key("ollama") {
            let cfg = ProviderConfig {
                id: "ollama".to_string(),
                base_url: Some("http://localhost:11434".to_string()),
                ..Default::default()
            };
            let ollama = OllamaNativeProvider::new(cfg);
            if let Ok(ollama_models) = ollama.get_models().await {
                for m in ollama_models {
                    models.push(Model {
                        id: m.name.clone(),
                        provider: "ollama".to_string(),
                        name: m.name.clone(),
                    });
                }
            }
        }

        Ok(ModelsResponse {
            object: "list".to_string(),
            data: models,
        })
    }

    pub async fn list_providers(&self) -> Result<Vec<crate::types::Provider>, AppError> {
        let providers = self.providers.read().await;
        let result: Vec<crate::types::Provider> = providers
            .iter()
            .map(|(id, provider)| {
                let raw_models = provider.scan_models().unwrap_or_else(|_| provider.models());
                let models: Vec<String> = raw_models
                    .into_iter()
                    .map(|m| {
                        if m.contains('/') {
                            m
                        } else {
                            format!("{}/{}", id, m)
                        }
                    })
                    .collect();
                crate::types::Provider {
                    id: id.clone(),
                    name: id.clone(),
                    provider_kind: "cli".to_string(),
                    base_url: None,
                    enabled: provider.is_available(),
                    bin: provider.bin().unwrap_or_default(),
                    models,
                    is_available: provider.is_available(),
                    last_checked: None,
                }
            })
            .collect();
        Ok(result)
    }

    pub async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AppError> {
        if request.n.unwrap_or(1) > 1 {
            return Err(AppError::ToolError("n>1 is not supported".to_string()));
        }
        if let Some(tools) = &request.tools {
            // Fail-closed limits (spec §14, [bridge.limits]).
            let limits = &self.bridge_config.limits;
            if tools.len() > limits.max_tools {
                return Err(AppError::ToolError(format!(
                    "too_many_tools ({} > {})",
                    tools.len(),
                    limits.max_tools
                )));
            }
            for t in tools {
                let schema_bytes = t.function
                    .parameters
                    .as_ref()
                    .and_then(|p| serde_json::to_vec(p).ok())
                    .map(|v| v.len())
                    .unwrap_or(0);
                if schema_bytes > limits.max_schema_bytes {
                    return Err(AppError::ToolError(format!(
                        "tool schema too large ({} > {} bytes)",
                        schema_bytes, limits.max_schema_bytes
                    )));
                }
                if t.r#type != "function" {
                    return Err(AppError::ToolError("unsupported tool type".to_string()));
                }
            }
        }
        info!("ChatCompletion START: model='{}', provider_opt={:?}, messages={}", request.model, request.provider, request.messages.len());
        
        // Keep tool role as-is for OpenAI tool-calling compatibility (spec_tooling §7.1)
        let _ = &mut request.messages;

        let provider_id = if let Some(id) = request.provider.as_ref() {
            info!("Using explicit provider: {}", id);
            id.clone()
        } else if let Some(id) = self.provider_for_model(&request.model).await {
            info!("Resolved provider via provider_for_model: {}", id);
            id
        } else {
            let models = self.list_models(None).await?;
            models.data
                .iter()
                .find(|model| model.id == request.model)
                .map(|model| model.provider.clone())
                .ok_or_else(|| {
                    info!("Provider not found for model='{}'", request.model);
                    AppError::ModelNotFound(request.model.clone())
                })?
        };

        let is_native = !self.ollama_cli_mode
            && (provider_id == "ollama" || request.model.starts_with("ollama/"));
        let known_ids: HashSet<String> = self.run_registry.get_known_tool_call_ids().await;
        let classified = classify(&request, &[], is_native, &known_ids);
        info!("Classified request: {:?}", classified);

        // Handle tool_choice="none" => no tools exposed (chat_only)
        if request.tool_choice.as_ref().and_then(|v| v.as_str()) == Some("none") {
            request.tools = None;
        }

        // Native provider (ollama): passthrough without CLI
        if is_native {
            let cfg = ProviderConfig {
                id: "ollama".to_string(),
                base_url: Some("http://localhost:11434".to_string()),
                ..Default::default()
            };
            let ollama = OllamaNativeProvider::new(cfg);
            let model = request.model.strip_prefix("ollama/").unwrap_or(&request.model).to_string();
            let mut resp = ollama.forward_chat(&model, &request).await.map_err(|e| AppError::Internal(e.to_string()))?;
            truncate_parallel_tool_calls(&mut resp, request.parallel_tool_calls);
            return Ok(resp);
        }

        // Fail-closed (spec §7.6 error table + spec §13.1): during the bridge
        // flow, client-sent `tools[]` are only honoured when the policy allows
        // and the resolved provider is bridge-capable.
        //   - `on_client_tools = "reject"` -> HTTP 501 `tool_bridge_unsupported`
        //   - provider tidak bridge-capable     -> HTTP 501 `tool_bridge_unsupported`
        // Native (ollama) path already returned above; here we are in bridge mode.
        if request.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false) && !is_native {
            let provider_bridge_ok = self
                .providers
                .read()
                .await
                .get(&provider_id)
                .map(|p| p.is_bridge())
                .unwrap_or(false);
            if self.routing.on_client_tools == crate::permission::ClientToolsPolicy::Reject || !provider_bridge_ok {
                return Err(AppError::ToolBridgeUnsupported);
            }
        }

        // Apply routing policy
        let routing = &self.routing;
        if request.tools.is_none() {
            match routing.no_tools_policy {
                crate::permission::NoToolsPolicy::Reject => {
                    return Err(AppError::ToolError("no tools requested".to_string()));
                }
                crate::permission::NoToolsPolicy::Bridge => {
                    // fall through to bridge mode with agentgate tools
                }
                crate::permission::NoToolsPolicy::ChatOnly => {
                    // chat only path below
                }
            }
        }

        // Determine classification-based routing
        match classified {
            crate::run::Classified::Native => {
                // Should have been handled above; fallback to provider call
                self.call_provider_native(&provider_id, request).await
            }
            crate::run::Classified::Resume { call_ids } => {
                info!(event = "run.resumed", provider = %provider_id);
                self.resume_run(&provider_id, call_ids, request).await
            }
            crate::run::Classified::Replay { history } => {
                info!(event = "run.replay", provider = %provider_id, reason = "unknown_tool_call_id");
                self.replay_run(&provider_id, history, request).await
            }
            crate::run::Classified::NewBridge { tools } => {
                self.new_bridge_run(&provider_id, tools, request).await
            }
            crate::run::Classified::NewChatOnly => {
                self.chat_only_run(&provider_id, request).await
            }
        }
    }

    async fn call_provider_native(&self, provider_id: &str, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        let model = request.model.clone();
        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).ok_or(AppError::ProviderNotFound(provider_id.to_string()))?;
        let response = provider.send_chat_completion(&model, request).await.map_err(provider_error_to_app_error)?;
        match response {
            crate::provider::ProviderResponse::Success { content: text, reasoning, tool_calls, session_id } => {
                crate::metrics::METRICS.record_cli_exit(true);
                let has_tool_calls = tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                let finish_reason = if has_tool_calls { "tool_calls" } else { "stop" };
                Ok(ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    model,
                    choices: vec![crate::types::Choice {
                        index: 0,
                        message: crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: if text.is_empty() { None } else { Some(text) },
                            reasoning_content: reasoning,
                            tool_calls: tool_calls,
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: Some(finish_reason.to_string()),
                    }],
                    usage: Some(crate::types::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    }),
                    session_id,
                })
            }
            crate::provider::ProviderResponse::Error(e) => {
                crate::metrics::METRICS.record_cli_exit(false);
                Err(AppError::Internal(e))
            }
        }
    }

    async fn resume_run(&self, provider_id: &str, call_ids: Vec<String>, mut request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Extract tool results from trailing tool messages of the request
        let trailing = crate::run::trailing_tool_messages(&request.messages);
        let tool_results: Vec<(String, String)> = trailing.iter()
            .filter_map(|m| m.tool_call_id.clone().map(|id| (id, m.content.clone().unwrap_or_default())))
            .collect();

        // Find parked run for any call_ids
        let run_id = {
            let parked_runs = self.bridge.parked_runs().lock().await;
            call_ids.iter().find_map(|id| {
                parked_runs.iter().find(|(_, run)| run.messages.iter().any(|msg| msg.tool_call_id.as_ref() == Some(id))).map(|(k, _)| k.clone())
            })
        };

        if let Some(run_id) = run_id {
            // Resolve each tool result into the parked run
            for (id, content) in &tool_results {
                let res = crate::bridge::ToolResult {
                    tool_call_id: id.clone(),
                    name: "resumed_tool".to_string(),
                    content: serde_json::json!(content),
                    is_error: false,
                };
                let _ = self.run_registry.resume_run(&run_id, res).await;
            }
            // Re-run provider with current request (carries full history)
            let tools = request.tools.take().unwrap_or_default();
            if tools.is_empty() && request.tools.is_none() {
                // keep tools from original? try extract from run not available; fall through native
            }
            self.call_provider_native(provider_id, request).await
        } else {
            // No parked run found; replay as new bridge run with full history
            let tools = request.tools.take().unwrap_or_default();
            self.new_bridge_run(provider_id, tools, request).await
        }
    }

    async fn replay_run(&self, provider_id: &str, history: Vec<crate::types::Message>, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Flatten history and create new run
        let mut new_request = request.clone();
        new_request.messages.extend(history);
        let tools = new_request.tools.take().unwrap_or_default();
        self.new_bridge_run(provider_id, tools, new_request).await
    }

    async fn new_bridge_run(&self, provider_id: &str, tools: Vec<crate::types::ToolDefinition>, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Acquire provider semaphore to limit concurrent runs per provider (spec §10.3)
        let _permit = self.acquire_provider_permit(provider_id).await?;

        // Create new run with bridge (best-effort; do not block on failure)
        let tool_schemas: Vec<crate::bridge::ToolSchema> = tools.iter().map(|t| crate::bridge::ToolSchema {
            name: t.function.name.clone(),
            description: t.function.description.clone().unwrap_or_default(),
            parameters: t.function.parameters.clone().unwrap_or_default(),
            provider: provider_id.to_string(),
            workspace: "default".to_string(),
        }).collect();
        let run = self.run_registry.create_run(
            "default".to_string(),
            request.model.clone(),
            request.messages.clone(),
            tool_schemas,
            crate::openai::ToolChoice::from_value(request.tool_choice.clone()).unwrap_or_default(),
        ).await.ok();

        // Spawn bridge process and get the single response (carries tool_calls)
        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).ok_or(AppError::ProviderNotFound(provider_id.to_string()))?;
        let model = request.model.clone();
        let response = provider.send_chat_completion(&model, request.clone()).await.map_err(provider_error_to_app_error)?;
        drop(providers);

        match response {
            crate::provider::ProviderResponse::Success { content: text, reasoning, mut tool_calls, session_id } => {
                crate::metrics::METRICS.record_cli_exit(true);
                let has_tool_calls = tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                let finish_reason = if has_tool_calls { "tool_calls" } else { "stop" };

                // Serial-mode gate (spec §10.3, `parallel_tool_calls: false`):
                // keep at most one tool call per assistant turn, enforced on the
                // CLI-bridge path as well (truncate before parking so only one
                // tool call stays open per assistant turn).
                truncate_tool_calls(&mut tool_calls, request.parallel_tool_calls);

                let has_tool_calls = tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);

                // Persist the assistant turn (including tool_calls) so a follow-up
                // request carrying role:"tool" can be classified as Resume.
                if let Some(mut run) = run {
                    let assistant_msg = crate::types::ChatMessage {
                        role: crate::types::MessageRole::Assistant,
                        content: if text.is_empty() { None } else { Some(text.clone()) },
                        reasoning_content: reasoning.clone(),
                        tool_calls: tool_calls.clone(),
                        tool_call_id: None,
                        name: None,
                    };
                    run.messages.push(assistant_msg);
                    if has_tool_calls {
                        if let Some(calls) = tool_calls.as_ref() {
                            if calls.len() > 1 {
                                // Parallel batch issued in a single assistant turn
                                // (spec §10.3) — structured event with batch size.
                                info!(event = "batch.issued", batch_size = calls.len());
                            }
                        }
                        if let Some(first) = tool_calls.as_ref().and_then(|v| v.first()) {
                            run.park(first.id.clone(), first.function.name.clone());
                            let _ = self.bridge.park_run(run.clone()).await;
                        }
                    } else {
                        run.complete(Vec::new());
                    }
                    self.run_registry.runs().write().await.insert(run.id.clone(), run);
                }

                Ok(ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    model,
                    choices: vec![crate::types::Choice {
                        index: 0,
                        message: crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: if text.is_empty() { None } else { Some(text) },
                            reasoning_content: reasoning,
                            tool_calls: tool_calls,
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: Some(finish_reason.to_string()),
                    }],
                    usage: Some(crate::types::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    }),
                    session_id,
                })
            }
            crate::provider::ProviderResponse::Error(e) => {
                crate::metrics::METRICS.record_cli_exit(false);
                Err(AppError::Internal(e))
            }
        }
    }

    /// Acquire a permit from the provider semaphore to limit concurrent runs (spec §10.3).
    /// Returns an owned permit that can be held for the duration of the provider turn.
    async fn acquire_provider_permit(&self, provider_id: &str) -> Result<tokio::sync::OwnedSemaphorePermit, AppError> {
        let semaphore = {
            let mut semaphores = self.provider_semaphores.write().await;
            semaphores
                .entry(provider_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.max_concurrency_per_provider)))
                .clone()
        };
        semaphore.acquire_owned().await.map_err(|_| AppError::Internal("Provider semaphore closed".to_string()))
    }

    async fn chat_only_run(&self, provider_id: &str, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        self.call_provider_native(provider_id, request).await
    }

    /// Stream provider output as [`RunEvent`]s.
    ///
    /// The channel is bounded (128) so a slow client creates backpressure
    /// instead of unbounded memory growth (spec_reasoning §6.6 / RS8).
    pub async fn stream_run_events(
        &self,
        provider_id: &str,
        request: ChatCompletionRequest,
    ) -> mpsc::Receiver<RunEvent> {
        info!("stream_run_events START: provider='{}', model='{}'", provider_id, request.model);
        let (tx, rx) = mpsc::channel(128);

        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).cloned();
        drop(providers);

        let is_ollama = provider_id == "ollama" || request.model.starts_with("ollama/");
        if is_ollama {
            let cfg = ProviderConfig {
                id: "ollama".to_string(),
                base_url: Some("http://localhost:11434".to_string()),
                ..Default::default()
            };
            let ollama = OllamaNativeProvider::new(cfg);
            let model = request.model.strip_prefix("ollama/").unwrap_or(&request.model).to_string();
            let req = request.clone();
            tokio::spawn(async move {
                if let Err(err) = ollama.stream_events(&model, &req, tx.clone()).await {
                    tracing::error!("ollama stream_events error: {}", err);
                    let _ = tx
                        .send(RunEvent::Error {
                            message: err.to_string(),
                            code: Some("provider_error".to_string()),
                        })
                        .await;
                } else {
                    tracing::info!("ollama stream_events DONE: model='{}'", model);
                }
            });
        } else if let Some(provider) = provider {
            let model = request.model.clone();
            let req = request.clone();
            tokio::spawn(async move {
                if let Err(err) = provider.send_chat_completion_stream(&model, req, tx.clone()).await {
                    tracing::error!("provider stream error: {}", err);
                    let _ = tx
                        .send(RunEvent::Error {
                            message: err.to_string(),
                            code: Some("provider_error".to_string()),
                        })
                        .await;
                } else {
                    tracing::info!("provider stream DONE: model='{}'", model);
                }
            });
        } else {
            let _ = tx
                .send(RunEvent::Error {
                    message: format!("provider not found: {}", provider_id),
                    code: Some("provider_not_found".to_string()),
                })
                .await;
        }

        rx
    }

    pub async fn add_provider(&mut self, request: AddProviderRequest) -> Result<(), AppError> {
        println!("Adding provider: {}", request.id);
        self.add_providers.write().await.push(request);
        Ok(())
    }
}