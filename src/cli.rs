use crate::config::Config;
use crate::provider::{CliProvider, CliProviderImpl};
use crate::service::ServiceRegistry;
use crate::types::{ChatCompletionRequest, MessageRole};
use crate::Result;
use tracing::error;

pub async fn execute_providers(config: &Config) -> Result<()> {
    let mut registry = ServiceRegistry::new();
    for provider_cfg in &config.provider {
        let provider = CliProviderImpl::new(provider_cfg.clone());
        let available = provider.check_available().await;
        registry.register(Box::new(provider)).await;
        if available {
            tracing::info!("Provider {} is available", provider_cfg.id);
        } else {
            tracing::info!("Provider {} is NOT available", provider_cfg.id);
        }
    }

    let providers = registry.list_providers().await?;
    println!("Found {} providers:", providers.len());
    for p in &providers {
        let status = if p.is_available { "OK" } else { "MISSING" };
        println!("  {} ({})", p.id, status);
    }
    Ok(())
}

pub async fn execute_models(config: &Config, provider_filter: Option<&str>) -> Result<()> {
    let mut registry = ServiceRegistry::new();
    for provider_cfg in &config.provider {
        registry.register(Box::new(CliProviderImpl::new(provider_cfg.clone()))).await;
    }

    let models = registry
        .list_models(provider_filter)
        .await
        .map_err(|e| {
            error!("Failed to list models: {}", e);
            e
        })?;

    println!("Available models:");
    for model in &models.data {
        println!("  {}/{}", model.provider, model.id);
    }
    Ok(())
}

pub async fn execute_test(config: &Config, model: &str, prompt: &str) -> Result<()> {
    let mut registry = ServiceRegistry::new();
    for provider_cfg in &config.provider {
        registry.register(Box::new(CliProviderImpl::new(provider_cfg.clone()))).await;
    }

    let request = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![crate::types::ChatMessage {
            role: MessageRole::User,
            content: prompt.to_string(),
            reasoning_content: None,
        }],
        ..Default::default()
    };

    let response = registry.chat_completion(request).await?;
    println!("Model: {}", response.model);
    println!("Content: {}", response.choices[0].message.content);
    Ok(())
}

pub async fn execute_init(force: bool) -> Result<()> {
    crate::config::write_default_config(force).await?;
    println!("Config file written to default location.");
    Ok(())
}
