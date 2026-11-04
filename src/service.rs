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
use crate::types::{AddProviderRequest, ChatCompletionRequest, ChatCompletionResponse, Model, ModelsResponse};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tracing::info;

pub struct ServiceRegistry {
    providers: RwLock<HashMap<String, Arc<dyn CliProvider + Send + Sync>>>,
    add_providers: RwLock<Vec<AddProviderRequest>>,
    pub bridge: Arc<BridgeServer>,
    pub run_registry: RunRegistry,
    pub routing: RoutingConfig,
    pub bridge_config: BridgeConfig,
    /// Global reasoning configuration (`[reasoning]` in agentgate.toml).
    pub reasoning: ReasoningConfig,
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        let bridge_config = BridgeConfig::default();
        let bridge = Arc::new(BridgeServer::new(bridge_config.clone()));
        let run_registry = RunRegistry::new(bridge.clone(), bridge_config.clone(), crate::permission::ToolActivityPolicy::default());
        Self {
            providers: RwLock::new(HashMap::new()),
            add_providers: RwLock::new(Vec::new()),
            bridge,
            run_registry,
            routing: RoutingConfig::default(),
            bridge_config,
            reasoning: ReasoningConfig::default(),
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
        info!("ChatCompletion START: model='{}', provider_opt={:?}, messages={}", request.model, request.provider, request.messages.len());
        
        // Keep tool role as-is for OpenAI tool-calling compatibility (spec_tooling §7.1)
        let _ = &mut request.messages;

        let provider_id = if let Some(id) = request.provider.as_ref() {
            info!("Using explicit provider: {}", id);
            id.clone()
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

        let is_native = provider_id == "ollama" || request.model.starts_with("ollama/");
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
            let resp = ollama.forward_chat(&model, &request).await.map_err(|e| AppError::Internal(e.to_string()))?;
            return Ok(resp);
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
                self.resume_run(&provider_id, call_ids, request).await
            }
            crate::run::Classified::Replay { history } => {
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
        let response = provider.send_chat_completion(&model, request).await.map_err(|e| AppError::Internal(e.to_string()))?;
        match response {
            crate::provider::ProviderResponse::Success { content: text, reasoning } => {
                Ok(ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    model,
                    choices: vec![crate::types::Choice {
                        index: 0,
                        message: crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: Some(text),
                            reasoning_content: reasoning,
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: Some("stop".to_string()),
                    }],
                    usage: Some(crate::types::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    }),
                })
            }
            crate::provider::ProviderResponse::Error(e) => Err(AppError::Internal(e)),
        }
    }

    async fn resume_run(&self, provider_id: &str, call_ids: Vec<String>, mut request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Find parked run for any call_ids
        let run_id = {
            let parked_runs = self.bridge.parked_runs().lock().await;
            call_ids.iter().find_map(|id| {
                parked_runs.iter().find(|(_, run)| run.messages.iter().any(|msg| msg.tool_call_id.as_ref() == Some(id))).map(|(k, _)| k.clone())
            })
        };

        if let Some(run_id) = run_id {
            // Resume parked run
            let tool_result = crate::bridge::ToolResult {
                tool_call_id: call_ids[0].clone(),
                name: "resumed_tool".to_string(),
                content: serde_json::json!({"status": "resumed"}),
                is_error: false,
            };
            self.run_registry.resume_run(&run_id, tool_result).await.map_err(|e| AppError::Internal(e.to_string()))?;
            self.call_provider_native(provider_id, request).await
        } else {
            // No parked run found; create new run
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
        // Create new run with bridge
        let run = self.run_registry.create_run(
            "default".to_string(),
            request.model.clone(),
            request.messages.clone(),
            tools.iter().map(|t| crate::bridge::ToolSchema {
                name: t.function.name.clone(),
                description: t.function.description.clone().unwrap_or_default(),
                parameters: t.function.parameters.clone().unwrap_or_default(),
                provider: provider_id.to_string(),
                workspace: "default".to_string(),
            }).collect(),
            crate::openai::ToolChoice::from_value(request.tool_choice.clone()).map_err(|e| AppError::Internal(e.to_string()))?,
        ).await.map_err(|e| AppError::Internal(e.to_string()))?;

        // Spawn bridge process
        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).ok_or(AppError::ProviderNotFound(provider_id.to_string()))?;
        let model = request.model.clone();
        provider.send_chat_completion(&model, request.clone()).await.map_err(|e| AppError::Internal(e.to_string()))?;

        // Wait for response
        self.run_registry.step_run(&run.id, provider.as_ref()).await.map_err(|e| AppError::Internal(e.to_string()))?;
        self.call_provider_native(provider_id, request).await
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