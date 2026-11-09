use crate::bridge::BridgeServer;
use crate::config::{BridgeConfig, ReasoningConfig, RoutingConfig, DefaultsConfig};
use crate::error::AppError;
use crate::config::ProviderConfig;
use crate::provider::{CliProvider};
use crate::provider::ollama_native::OllamaNativeProvider;
use crate::reasoning::RunEvent;
use crate::run::classify;
use crate::run::RunRegistry;
use crate::types::{AddProviderRequest, ChatCompletionRequest, ChatCompletionResponse, Model, ModelsResponse, ToolCall};
use futures::future::join_all;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    native_providers: RwLock<HashMap<String, Arc<dyn CliProvider + Send + Sync>>>,
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
    /// Model cache: provider_id -> (models, cached_at)
    model_cache: RwLock<HashMap<String, (Vec<String>, Instant)>>,
    /// Cache TTL for model scans (default 300s).
    model_cache_ttl: Duration,
}

impl ServiceRegistry {
    /// Create a new ServiceRegistry with the given config sections.
    pub fn with_config(
        routing: RoutingConfig,
        bridge_config: BridgeConfig,
        reasoning: ReasoningConfig,
        defaults: DefaultsConfig,
        ollama_cli_mode: bool,
    ) -> Self {
        let bridge = Arc::new(BridgeServer::new(bridge_config.clone()));
        let run_registry = RunRegistry::new(bridge.clone(), bridge_config.clone(), crate::permission::ToolActivityPolicy::default());
        Self {
            providers: RwLock::new(HashMap::new()),
            native_providers: RwLock::new(HashMap::new()),
            add_providers: RwLock::new(Vec::new()),
            bridge,
            run_registry,
            routing,
            bridge_config,
            reasoning,
            ollama_cli_mode,
            provider_semaphores: RwLock::new(HashMap::new()),
            max_concurrency_per_provider: defaults.max_concurrency_per_provider,
            model_cache: RwLock::new(HashMap::new()),
            model_cache_ttl: Duration::from_secs(300), // default 5 minutes
        }
    }

    /// Create a new ServiceRegistry with default config (for tests/backwards compat).
    pub fn new() -> Self {
        Self::with_config(
            RoutingConfig::default(),
            BridgeConfig::default(),
            ReasoningConfig::default(),
            DefaultsConfig::default(),
            false,
        )
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

    /// Set the routing configuration from config file [routing] section.
    pub fn with_routing(mut self, routing: RoutingConfig) -> Self {
        self.routing = routing;
        self
    }

    /// Set the bridge configuration from config file [bridge] section.
    /// Note: this recreates the bridge and run_registry, so should be called before registering providers.
    pub fn with_bridge_config(mut self, bridge_config: BridgeConfig) -> Self {
        self.bridge_config = bridge_config.clone();
        self.bridge = Arc::new(BridgeServer::new(bridge_config.clone()));
        self.run_registry = RunRegistry::new(self.bridge.clone(), bridge_config, crate::permission::ToolActivityPolicy::default());
        self
    }

    /// Resolve the provider for a model without spawning model-discovery commands.
    pub async fn provider_for_model(&self, model: &str) -> Option<String> {
        // Strip "agg/" gateway namespace prefix if present
        let model = model.strip_prefix("agg/").unwrap_or(model);
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

    /// Resolve the provider for a model, including `agg/`-namespaced ids that
    /// were discovered from a provider CLI (e.g. `agg/9router/ag-poli` belongs
    /// to the `opencode` provider even though its namespace is `9router`).
    pub async fn resolve_provider(&self, model: &str) -> Option<String> {
        if let Some(id) = self.provider_for_model(model).await {
            return Some(id);
        }
        // Fall back to the cached model list, which carries the discovering
        // provider for every published id.
        if let Ok(models) = self.list_models(None).await {
            if let Some(m) = models.data.iter().find(|m| m.id == model) {
                return Some(m.provider.clone());
            }
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

    /// Register a native (non-bridge) provider for chat-only requests.
    pub async fn register_native(&mut self, mut provider: Box<dyn CliProvider + Send + Sync>) {
        let id = provider.id();
        let available = provider.check_available().await;
        provider.set_available(available);
        let mut providers = self.native_providers.write().await;
        providers.insert(id, Arc::from(provider));
    }

    pub async fn list_models(&self, _provider_filter: Option<&str>) -> Result<ModelsResponse, AppError> {
        let start = Instant::now();
        
        // Get all providers (bridge + native)
        let bridge_providers = self.providers.read().await;
        let native_providers = self.native_providers.read().await;
        
        // Collect all providers with their IDs. Prefer the native provider for
        // each ID: bridge adapters just echo the configured `models` list, while
        // the native CLI provider actually runs `models_cmd` (`opencode models`,
        // `gemini list`, `ollama list`, …).
        let mut all_providers: Vec<(String, Arc<dyn CliProvider + Send + Sync>)> = Vec::new();
        for (id, provider) in native_providers.iter() {
            all_providers.push((id.clone(), provider.clone()));
        }
        for (id, provider) in bridge_providers.iter() {
            if !all_providers.iter().any(|(pid, _)| pid == id) {
                all_providers.push((id.clone(), provider.clone()));
            }
        }
        drop(bridge_providers);
        drop(native_providers);
        
        // Check cache for all providers
        let mut cached_models = HashMap::new();
        let mut providers_to_scan = Vec::new();
        let cache_ttl = self.model_cache_ttl;
        let now = Instant::now();
        
        {
            let cache = self.model_cache.read().await;
            for (id, provider) in &all_providers {
                if let Some((models, cached_at)) = cache.get(id) {
                    if now.duration_since(*cached_at) < cache_ttl {
                        cached_models.insert(id.clone(), models.clone());
                        continue;
                    }
                }
                providers_to_scan.push((id.clone(), provider.clone()));
            }
        }
        
        // Scan missing/expired providers in parallel. Each scan is bounded so a
        // hung CLI (`opencode models`, `gemini list`, …) cannot stall the whole
        // request past the client's gateway timeout.
        let scan_timeout = Duration::from_secs(10);
        let scan_futures: Vec<_> = providers_to_scan.into_iter().map(|(id, provider)| {
            let id_clone = id.clone();
            tokio::spawn(async move {
                let scan_start = Instant::now();
                let result = match tokio::time::timeout(scan_timeout, provider.scan_models()).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!(
                        "scan_models timed out after {}s",
                        scan_timeout.as_secs()
                    )),
                };
                let elapsed = scan_start.elapsed();
                tracing::info!(provider = %id_clone, elapsed_ms = elapsed.as_millis(), "scan_models completed");
                (id_clone, result, elapsed)
            })
        }).collect();
        
        let scan_results = futures::future::join_all(scan_futures).await;
        
        // Update cache with new results
        let mut cache = self.model_cache.write().await;
        for result in scan_results {
            match result {
                Ok((id, Ok(models), elapsed)) => {
                    cache.insert(id.clone(), (models.clone(), Instant::now()));
                    cached_models.insert(id, models);
                }
                Ok((id, Err(e), elapsed)) => {
                    tracing::warn!(provider = %id, elapsed_ms = elapsed.as_millis(), error = %e, "scan_models failed, using config models");
                    // Fallback to config models
                    if let Some((_, provider)) = all_providers.iter().find(|(pid, _)| pid == &id) {
                        cache.insert(id.clone(), (provider.models(), Instant::now()));
                        cached_models.insert(id, provider.models());
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "scan_models task join error");
                }
            }
        }
        
        // Build models response from cached results.
        //
        // Namespacing rules (`agg/` prevents collisions with 9router's own CLI):
        // - `provider/model` (e.g. `opencode/beam-free`, `ollama/llama3`): keep the
        //   upstream namespace as-is → `agg/opencode/beam-free`. If that namespace
        //   is a registered gateway provider, attribute the model to it so the
        //   same model discovered from two providers is not misattributed.
        // - bare `model` → `agg/<discovering provider>/<model>`.
        let mut models = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for (id, model_list) in &cached_models {
            for model_id in model_list {
                let (prefixed_id, owner) = match model_id.split_once('/') {
                    Some((prefix, _)) if all_providers.iter().any(|(pid, _)| pid == prefix) => {
                        (format!("agg/{}", model_id), prefix.to_string())
                    }
                    Some(_) => (format!("agg/{}", model_id), id.clone()),
                    None => (format!("agg/{}/{}", id, model_id), id.clone()),
                };
                if seen_ids.insert(prefixed_id.clone()) {
                    models.push(Model {
                        id: prefixed_id,
                        provider: owner,
                        name: model_id.clone(),
                    });
                }
            }
        }
        
        // Ollama native provider: include models from /api/tags.
        // Skip when ollama_cli_mode is on (the `ollama list` scan above already
        // covers it) and always bound the HTTP probe so a hung Ollama daemon
        // cannot turn /v1/models into a gateway timeout.
        let has_ollama = all_providers.iter().any(|(id, _)| id == "ollama");
        if has_ollama && !self.ollama_cli_mode {
            let cfg = ProviderConfig {
                id: "ollama".to_string(),
                base_url: Some("http://localhost:11434".to_string()),
                ..Default::default()
            };
            let ollama = OllamaNativeProvider::new(cfg);
            match tokio::time::timeout(Duration::from_secs(2), ollama.get_models()).await {
                Ok(Ok(ollama_models)) => {
                    for m in ollama_models {
                        let prefixed = format!("agg/{}", m.name);
                        if seen_ids.insert(prefixed.clone()) {
                            models.push(Model {
                                id: prefixed,
                                provider: "ollama".to_string(),
                                name: m.name.clone(),
                            });
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "ollama /api/tags unavailable, skipping");
                }
                Err(_) => {
                    tracing::warn!("ollama /api/tags timed out after 2s, skipping");
                }
            }
        }

        // Stable ordering so clients that diff the model list do not see
        // HashMap iteration order churn between requests.
        models.sort_by(|a, b| a.id.cmp(&b.id));

        let cached_count = cached_models.len();
        tracing::info!(total_providers = all_providers.len(), cached = cached_count, total_models = models.len(), elapsed_ms = start.elapsed().as_millis(), "list_models completed");
        
        Ok(ModelsResponse {
            object: "list".to_string(),
            data: models,
        })
    }

    /// Warm the model-scan cache in the background at startup.
    ///
    /// The first `/v1/models` call used to pay the full CLI scan cost
    /// (`opencode models`, `gemini list`, `ollama list`, …), which could exceed
    /// a client's gateway timeout and surface as a 504 on
    /// `POST /api/providers/fetch-models`. Calling this once after providers are
    /// registered makes the first real request a cache hit.
    pub async fn warm_model_cache(&self) {
        let start = Instant::now();
        match self.list_models(None).await {
            Ok(resp) => {
                tracing::info!(
                    models = resp.data.len(),
                    elapsed_ms = start.elapsed().as_millis(),
                    "model cache warmed"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "model cache warm-up failed");
            }
        }
    }

    pub async fn list_providers(&self) -> Result<Vec<crate::types::Provider>, AppError> {
        let providers = self.providers.read().await;
        let result: Vec<crate::types::Provider> = futures::future::join_all(
            providers.iter().map(|(id, provider)| {
                let id = id.clone();
                let provider = provider.clone();
                async move {
                    let raw_models = provider.scan_models().await.unwrap_or_else(|_| provider.models());
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
                }
            })
        ).await;
        Ok(result)
    }

    pub async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AppError> {
        // Keep the published (possibly `agg/`-namespaced) id for provider
        // resolution, then strip the gateway namespace for the CLI invocation.
        let requested_model = request.model.clone();
        if let Some(stripped) = request.model.strip_prefix("agg/") {
            request.model = stripped.to_string();
        }
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
        } else if let Some(id) = self.resolve_provider(&requested_model).await {
            info!("Resolved provider via resolve_provider: {}", id);
            id
        } else {
            info!("Provider not found for model='{}'", requested_model);
            return Err(AppError::ModelNotFound(requested_model));
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
                tracing::warn!(event = "replay_fallback", provider = %provider_id, reason = "unknown_tool_call_id");
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
        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).ok_or(AppError::ProviderNotFound(provider_id.to_string()))?.clone();
        drop(providers);
        self.call_provider_native_with(provider, request).await
    }

    async fn call_provider_native_with(&self, provider: Arc<dyn CliProvider + Send + Sync>, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        let model = request.model.clone();
        tracing::info!(model, provider_id = %provider.id(), "call_provider_native_with: calling send_chat_completion");
        let response = provider.send_chat_completion(&model, request).await.map_err(provider_error_to_app_error)?;
        tracing::info!(model, provider_id = %provider.id(), "call_provider_native_with: got response");
        match response {
            crate::provider::ProviderResponse::Success { content: text, reasoning, tool_calls, session_id, .. } => {
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
                Err(AppError::ProviderError(e))
            }
        }
    }

    async fn resume_run(&self, provider_id: &str, call_ids: Vec<String>, mut request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Extract tool results from trailing tool messages of the request
        let trailing = crate::run::trailing_tool_messages(&request.messages);
        let tool_results: Vec<(String, String)> = trailing.iter()
            .filter_map(|m| m.tool_call_id.clone().map(|id| (id, m.content.clone().unwrap_or_default())))
            .collect();

        // Find parked run for any call_ids (PR-S1 linkage + PR-S2 liveness)
        let (run_id, session_key) = {
            let parked_runs = self.bridge.parked_runs().lock().await;
            call_ids.iter().find_map(|id| {
                parked_runs.iter().find(|(_, run)| {
                    run.pending_tool_call_ids.iter().any(|p| p == id)
                        || run.messages.iter().any(|msg| {
                            msg.tool_call_id.as_ref() == Some(id)
                                || msg.tool_calls.as_ref().is_some_and(|tcs| tcs.iter().any(|tc| &tc.id == id))
                        })
                }).map(|(k, run)| (k.clone(), run.mcp_session_id.clone().or(run.provider_session_key.clone())))
            }).map(|(rid, skey)| (Some(rid), skey)).unwrap_or((None, None))
        };
        let session_lost_reason = if run_id.is_some() {
            if let Some(ref key) = session_key {
                if self.bridge.has_provider_session(key).await { None } else { Some("parked_session_lost") }
            } else {
                None // unknown linkage: keep current fallback behavior
            }
        } else {
            Some("parked_run_not_found")
        };

        if let (Some(run_id), None) = (run_id.as_ref(), session_lost_reason) {
            // Parked run found and no known session loss.
            for (id, content) in &tool_results {
                let res = crate::bridge::ToolResult {
                    tool_call_id: id.clone(),
                    name: "resumed_tool".to_string(),
                    content: serde_json::json!(content),
                    is_error: false,
                };
                let _ = self.run_registry.resume_run(&run_id, res.clone()).await;
                // PR-S4: deliver real results to held MCP tools/call (unblocks live CLI continuation).
                let _ = self.bridge.resolve_tool_result_by_id(id, res).await;
            }
            // PR-S4: capability-gated live continuation (opencode). Await continuation if registered;
            // replay fallback mandatory on absence/timeout/error/session loss.
            let mut replay_reason = "continuation_not_available";
            let mut has_live_session = self.bridge.has_provider_session(run_id).await;
            if let Some(ref k) = session_key {
                if self.bridge.has_provider_session(k).await { has_live_session = true; }
            }
            if has_live_session {
                let cont = match self.bridge.take_continuation(run_id).await {
                    Some(c) => Some(c),
                    None => match session_key.as_deref() {
                        Some(k) => self.bridge.take_continuation(k).await,
                        None => None,
                    },
                };
                if let Some(cont) = cont {
                    let timeout = crate::provider::bridge_turn_timeout();
                    match tokio::time::timeout(timeout, cont.rx).await {
                        Ok(Ok(out)) => {
                            tracing::info!(event = "resume.live_continuation.ok", provider = %provider_id, run_id = %run_id);
                            let has_tool_calls = out.tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                            let finish_reason = if has_tool_calls { "tool_calls" } else { "stop" };
                            // PR-S5: best-effort cleanup when continuation completes without further tool calls.
                            if !has_tool_calls {
                                self.bridge.remove_continuation(run_id).await;
                                if let Some(ref k) = session_key { self.bridge.unregister_provider_session(k).await; }
                                self.bridge.unregister_provider_session(run_id).await;
                                // Remove from run registries best-effort.
                                let _ = self.bridge.runs().write().await.remove(run_id);
                            }
                            return Ok(ChatCompletionResponse {
                                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                                object: "chat.completion".to_string(),
                                created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                                model: request.model.clone(),
                                choices: vec![crate::types::Choice {
                                    index: 0,
                                    message: crate::types::ChatMessage {
                                        role: crate::types::MessageRole::Assistant,
                                        content: if out.content.is_empty() { None } else { Some(out.content) },
                                        reasoning_content: out.reasoning,
                                        tool_calls: out.tool_calls,
                                        tool_call_id: None,
                                        name: None,
                                    },
                                    finish_reason: Some(finish_reason.to_string()),
                                }],
                                usage: Some(crate::types::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }),
                                session_id: out.session_id,
                            });
                        }
                        Ok(Err(_)) => {
                            replay_reason = "continuation_channel_closed";
                            if let Some(ref k) = session_key { self.bridge.unregister_provider_session(k).await; }
                            self.bridge.unregister_provider_session(run_id).await;
                        }
                        Err(_) => {
                            replay_reason = "continuation_timeout";
                            if let Some(ref k) = session_key { self.bridge.unregister_provider_session(k).await; }
                            self.bridge.unregister_provider_session(run_id).await;
                        }
                    }
                } else {
                    tracing::info!(event = "resume.live_continuation.pending", provider = %provider_id, run_id = %run_id, session_key = ?session_key);
                }
            }
            let mut tools = request.tools.take().unwrap_or_default();
            // Preserve parked-run tool schemas for replay/re-invocation when client omits tools.
            if tools.is_empty() {
                if let Some(run) = self.run_registry.get_run(run_id).await {
                    tools = run.tools.into_iter().map(|t| crate::types::ToolDefinition {
                        r#type: "function".to_string(),
                        function: crate::types::FunctionDefinition {
                            name: t.name,
                            description: if t.description.is_empty() { None } else { Some(t.description) },
                            parameters: Some(t.parameters),
                        },
                    }).collect();
                }
            }
            tracing::warn!(event = "replay_fallback", provider = %provider_id, run_id = %run_id, session_key = ?session_key, reason = %replay_reason);
            // PR-S5: rotate session on replay fallback (avoid stale CLI/persistent session reuse).
            request.session_id = None;
            request.agentgate_run_id = None;
            self.new_bridge_run(provider_id, tools, request).await
        } else {
            let reason = session_lost_reason.unwrap_or("parked_session_lost");
            // Best-effort: release any MCP holds for these tool_call_ids to avoid leaks on replay.
            for (id, _) in &tool_results {
                let _ = self.bridge.resolve_by_openai_tool_call_id(id).await;
            }
            tracing::warn!(event = "replay_fallback", provider = %provider_id, run_id = ?run_id, session_key = ?session_key, reason = %reason);
            // PR-S5: rotate session on replay fallback.
            request.session_id = None;
            request.agentgate_run_id = None;
            let tools = request.tools.take().unwrap_or_default();
            self.new_bridge_run(provider_id, tools, request).await
        }
    }

    async fn replay_run(&self, provider_id: &str, history: Vec<crate::types::Message>, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Flatten history and create new run (spec §10.5 replay fallback).
        // Session rotation (cli_gen++/new CLI session) is provider-dependent; full replay
        // is performed by reissuing the request with flattened history.
        tracing::warn!(event = "replay_fallback", provider = %provider_id, reason = "explicit_replay");
        let mut new_request = request.clone();
        new_request.messages.extend(history);
        // PR-S5: force fresh session/CLI context on replay.
        new_request.session_id = None;
        new_request.agentgate_run_id = None;
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
        let mut req_for_provider = request.clone();
        req_for_provider.agentgate_run_id = run.as_ref().map(|r| r.id.clone());
        let response = provider.send_chat_completion(&model, req_for_provider).await.map_err(provider_error_to_app_error)?;
        drop(providers);

        match response {
            crate::provider::ProviderResponse::Success { content: text, reasoning, mut tool_calls, session_id, mcp_session_id } => {
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
                        if let Some(calls) = tool_calls.as_ref() {
                            if let Some(first) = calls.first() {
                                run.park(first.id.clone(), first.function.name.clone());
                                // PR-S4: linkage for continuation/replay routing (capability-gated).
                                run.mcp_session_id = mcp_session_id.clone();
                                run.provider_session_key = mcp_session_id.clone().or(session_id.clone());
                                run.pending_tool_call_ids = calls.iter().map(|c| c.id.clone()).collect();
                                run.continuation_capable = mcp_session_id.is_some();
                                let _ = self.bridge.park_run(run.clone()).await;
                            }
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
                Err(AppError::ProviderError(e))
            }
        }
    }

    /// Acquire a permit from the provider semaphore to limit concurrent runs (spec §10.3).
    /// Returns an owned permit that can be held for the duration of the provider turn.
    /// Semaphore size is clamped to >= 1: a 0-sized semaphore would deadlock every caller.
    /// Bounded timeout remains as a last-resort guard against permit leaks/starvation.
    async fn acquire_provider_permit(&self, provider_id: &str) -> Result<tokio::sync::OwnedSemaphorePermit, AppError> {
        let limit = self.max_concurrency_per_provider.max(1);
        let semaphore = {
            let mut semaphores = self.provider_semaphores.write().await;
            semaphores
                .entry(provider_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(limit)))
                .clone()
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            semaphore.acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) => Err(AppError::Internal("Provider semaphore closed".to_string())),
            Err(_) => Err(AppError::Internal(format!(
                "Provider semaphore acquisition timed out (provider={}, limit={})",
                provider_id, limit
            ))),
        }
    }

    async fn chat_only_run(&self, provider_id: &str, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Chat-only requests (no client tools) use the native CLI provider
        // directly (`opencode run`, `gemini generate`, `ollama run`, …). The
        // bridge is only needed when client tools must be isolated/executed, so
        // avoiding it here removes the MCP session spawn/wait from the hot path.
        let native_providers = self.native_providers.read().await;
        tracing::info!(provider_id, native_count = native_providers.len(), "chat_only_run: checking native providers");
        if let Some(provider) = native_providers.get(provider_id).cloned() {
            drop(native_providers);
            tracing::info!(provider_id, "chat_only_run: using native CLI provider");
            return self.call_provider_native_with(provider, request).await;
        }
        drop(native_providers);

        tracing::info!(provider_id, "chat_only_run: no native provider, falling back to bridge");
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

        // Strip "agg/" gateway namespace prefix from model id
        let mut request = request;
        if let Some(stripped) = request.model.strip_prefix("agg/") {
            request.model = stripped.to_string();
        }

        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).cloned();
        drop(providers);

        // Chat-only requests (no client tools) should stream from the native
        // CLI provider, mirroring `chat_only_run`. Without this, streaming a
        // tool-less opencode/gemini/claude turn went through the bridge adapter
        // (which spawns an MCP session and waits for it) and could hang.
        let has_client_tools = request
            .tools
            .as_ref()
            .map(|tools| !tools.is_empty())
            .unwrap_or(false);
        let provider = if has_client_tools {
            provider
        } else {
            let native = self.native_providers.read().await.get(provider_id).cloned();
            native.or(provider)
        };

        let is_ollama = provider_id == "ollama" || request.model.starts_with("ollama/");
        // When ollama_cli_mode=true, use bridge provider (OllamaCliProvider) instead of HTTP native
        let use_ollama_cli = self.ollama_cli_mode && is_ollama;
        
        if use_ollama_cli {
            // Use bridge provider which has OllamaCliProvider registered
            if let Some(provider) = provider {
                let model = request.model.clone();
                let req = request.clone();
                tokio::spawn(async move {
                    if let Err(err) = provider.send_chat_completion_stream(&model, req, tx.clone()).await {
                        tracing::error!("ollama CLI stream error: {}", err);
                        let _ = tx
                            .send(RunEvent::Error {
                                message: err.to_string(),
                                code: Some("provider_error".to_string()),
                            })
                            .await;
                    } else {
                        tracing::info!("ollama CLI stream DONE: model='{}'", model);
                    }
                });
            } else {
                let _ = tx
                    .send(RunEvent::Error {
                        message: "ollama provider not found in bridge".to_string(),
                        code: Some("provider_not_found".to_string()),
                    })
                    .await;
            }
        } else if is_ollama {
            // HTTP native fallback
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