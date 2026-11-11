use crate::config::Config;
use crate::provider::{CliProvider, CliProviderImpl};
use crate::service::ServiceRegistry;
use crate::types::{ChatCompletionRequest, MessageRole};
use crate::provider::bridge::spike::{run_spike_bridge, SpikeConfig};
use anyhow::Result;
use std::time::{Duration, Instant};
use tracing::error;

pub async fn execute_providers(config: &Config) -> Result<()> {
    let mut registry = ServiceRegistry::with_config(
        config.routing.clone(),
        config.bridge.clone(),
        config.reasoning.clone(),
        config.defaults.clone(),
        false, // ollama_cli_mode - not used for providers command
    );
    for provider_cfg in &config.provider {
        // Bridge-capable agents use the MCP bridge adapter (spec_tooling.md §12).
        // Also register the native impl so `providers` reports the real binary
        // availability resolved through `resolve_binary` (nvm/npm fallbacks).
        let provider = crate::provider::build_cli_provider(
            provider_cfg,
            Some(registry.bridge.clone()),
            Some(&config.bridge),
        );
        let available = provider.check_available().await;
        let native_available = crate::provider::CliProviderImpl::new(provider_cfg.clone())
            .check_available()
            .await;
        registry.register(provider).await;
        registry
            .register_native(Box::new(crate::provider::CliProviderImpl::new(
                provider_cfg.clone(),
            )))
            .await;
        let available = available || native_available;
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
    let mut registry = ServiceRegistry::with_config(
        config.routing.clone(),
        config.bridge.clone(),
        config.reasoning.clone(),
        config.defaults.clone(),
        false,
    );
    registry.register_configured(&config.provider).await;

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
    let mut registry = ServiceRegistry::with_config(
        config.routing.clone(),
        config.bridge.clone(),
        config.reasoning.clone(),
        config.defaults.clone(),
        false,
    );
    registry.register_configured(&config.provider).await;

    let request = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![crate::types::ChatMessage {
            role: MessageRole::User,
            content: Some(prompt.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        ..Default::default()
    };

    let response = registry.chat_completion(request).await?;
    println!("Model: {}", response.model);
    println!("Content: {}", response.choices[0].message.content.as_deref().unwrap_or(""));
    Ok(())
}

pub async fn execute_init(force: bool) -> Result<()> {
    crate::config::write_default_config(force).await?;
    println!("Config file written to default location.");
    Ok(())
}

pub async fn execute_spike_bridge(cli: String, model: String, port: u16, prompt: String, timeout: u64) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let _config = crate::config::load_config().await?.apply_env_overrides();
    let _report = rt.block_on(run_spike_bridge(SpikeConfig {
        cli_type: cli,
        model,
        bridge_port: port,
        session_id: None,
        keep_alive: Some(timeout.to_string()),
        prompt,
    }))?;
    println!("\nSpike-bridge completed.");
    eprintln!("Spike-bridge completed. (report saved if compat file written)");
    Ok(())
}
