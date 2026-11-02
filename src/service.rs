use crate::error::AppError;
use crate::provider::{CliProvider, ProviderResponse};
use crate::types::{AddProviderRequest, ChatCompletionRequest, ChatCompletionResponse, Model, ModelsResponse};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Default)]
pub struct ServiceRegistry {
    providers: RwLock<HashMap<String, Box<dyn CliProvider + Send + Sync>>>,
    add_providers: RwLock<Vec<AddProviderRequest>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
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
        providers.insert(id, provider);
    }

    pub async fn list_models(&self, _provider_filter: Option<&str>) -> Result<ModelsResponse, AppError> {
        let providers = self.providers.read().await;
        let mut models = Vec::new();

        for (id, provider) in providers.iter() {
            if let Ok(model_list) = provider.scan_models() {
                for model_id in model_list {
                    models.push(Model {
                        id: model_id.clone(),
                        provider: id.clone(),
                        name: model_id.clone(),
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
            .map(|(id, provider)| crate::types::Provider {
                id: id.clone(),
                name: id.clone(),
                enabled: provider.is_available(),
                bin: provider.bin().unwrap_or_default(),
                models: provider.models(),
                is_available: provider.is_available(),
                last_checked: None,
            })
            .collect();
        Ok(result)
    }

    pub async fn chat_completion(
        &self,
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AppError> {
        info!("ChatCompletion request: model='{}', provider_opt={:?}, messages={}", request.model, request.provider, request.messages.len());
        
        // Sanitize messages: map 'tool' role to 'assistant' for compatibility
        for msg in &mut request.messages {
            if msg.role == crate::types::MessageRole::Tool {
                msg.role = crate::types::MessageRole::Assistant;
            }
        }

        // Log request payload to file before sending to CLI
        {
            use std::fs::OpenOptions;
            use std::io::Write;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open("log.txt")
                .expect("cannot open log.txt");
            writeln!(file, "=== REQUEST ===\nmodel: {}\nprovider: {:?}\nmessages: {:?}\n", request.model, request.provider, request.messages).ok();
        }

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

        let providers = self.providers.read().await;
        let provider = providers
            .get(&provider_id)
            .ok_or(AppError::ProviderNotFound(provider_id.clone()))?;

        let is_stream = request.stream.unwrap_or(false);
        let model = request.model.clone();
        let response = provider
            .send_chat_completion(&model, request)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        // Log response payload after CLI execution
        let result = match response {
            ProviderResponse::Success(text) => {
                {
                    use std::fs::OpenOptions;
                    use std::io::Write;
                    let mut resp_file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("log.txt")
                        .expect("cannot open log.txt");
                    writeln!(resp_file, "=== RESPONSE ===\nmodel: {}\nprovider: {}\nresponse: {}\n", model, provider_id, text).ok();
                }
                info!("Provider '{}' success, response length={}", provider_id, text.len());
                
                // If stream is requested, we can optionally format or wrap, but for OpenAI compatibility on non-streaming/streaming:
                ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: if is_stream { "chat.completion.chunk".to_string() } else { "chat.completion".to_string() },
                    created: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    model: model.clone(),
                    choices: vec![crate::types::Choice {
                        index: 0,
                        message: crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: text,
                        },
                        finish_reason: "stop".to_string(),
                    }],
                    usage: crate::types::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    },
                }
            }
            ProviderResponse::Error(e) => {
                info!("Provider '{}' error: {}", provider_id, e);
                return Err(AppError::Internal(e));
            }
        };

        Ok(result)
    }

    pub async fn add_provider(&mut self, request: AddProviderRequest) -> Result<(), AppError> {
        println!("Adding provider: {}", request.id);
        self.add_providers.write().await.push(request);
        Ok(())
    }
}