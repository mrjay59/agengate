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
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AppError> {
        let provider_id = request.provider.as_ref().map(|s| s.clone())
            .or_else(|| {
                let models = futures::executor::block_on(self.list_models(None)).ok();
                models.and_then(|m| {
                    m.data
                        .iter()
                        .find(|model| model.id == request.model)
                        .map(|model| model.provider.clone())
                })
            });

        let provider_id = match provider_id {
            Some(id) => id,
            None => return Err(AppError::ModelNotFound(request.model.clone())),
        };

        let providers = self.providers.read().await;
        let provider = providers
            .get(&provider_id)
            .ok_or(AppError::ProviderNotFound(provider_id.clone()))?;

        let model = request.model.clone();
        let response = provider
            .send_chat_completion(&model, request)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?;

        match response {
            ProviderResponse::Success(text) => Ok(ChatCompletionResponse {
                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                object: "chat.completion".to_string(),
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
            }),
            ProviderResponse::Error(e) => Err(AppError::Internal(e)),
        }
    }

    pub async fn add_provider(&mut self, request: AddProviderRequest) -> Result<(), AppError> {
        info!("Adding provider: {}", request.id);
        self.add_providers.write().await.push(request);
        Ok(())
    }
}