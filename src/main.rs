use std::path::PathBuf;
use std::sync::Arc;

use agentgate::api::api;
use agentgate::config::load_config;
use agentgate::provider::CliProviderImpl;
use agentgate::service::ServiceRegistry;
use agentgate::types::{ChatCompletionRequest, ChatMessage, MessageRole};
use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Args {
    #[command(subcommand)]
    command: Commands,

    /// Path to config.toml
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the API server
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// Show available models from each provider
    Models {
        #[arg(long)]
        provider: Option<String>,
    },
    /// Test a provider chat completion
    Test {
        model: String,
        prompt: String,
    },
    /// Initialize config
    Init {
        /// Overwrite existing config file
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| "agentgate=debug,axum=debug,tower_http=debug".into(),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();

    let config = load_config().await?;

    match &args.command {
        Commands::Serve { host, port } => {
            use tokio::net::TcpListener;
            let host = host.as_deref().unwrap_or(&config.server.host);
            let port = port.unwrap_or(config.server.port);
            let bind_addr = format!("{}:{}", host, port);
            let mut registry = ServiceRegistry::new();
            for provider_cfg in &config.provider {
                registry
                    .register(Box::new(CliProviderImpl::new(provider_cfg.clone())))
                    .await;
            }
            let app = api().with_state(Arc::new(registry));
            tracing::info!("Starting server on {}", bind_addr);
            let listener = TcpListener::bind(bind_addr.as_str()).await?;
            axum::serve(listener, app).await?;
        }
        Commands::Models { provider } => {
            let mut registry = ServiceRegistry::new();
            for provider_cfg in &config.provider {
                registry
                    .register(Box::new(CliProviderImpl::new(provider_cfg.clone())))
                    .await;
            }
            let provider_filter = provider.as_deref();
            let models_response = registry.list_models(provider_filter).await?;

            println!("Available models:");
            for model in &models_response.data {
                println!("  {}/{}", model.provider, model.id);
            }
        }
        Commands::Test { model, prompt } => {
            let mut registry = ServiceRegistry::new();
            for provider_cfg in &config.provider {
                registry
                    .register(Box::new(CliProviderImpl::new(provider_cfg.clone())))
                    .await;
            }
            let request = ChatCompletionRequest {
                model: model.clone(),
                messages: vec![ChatMessage {
                    role: MessageRole::User,
                    content: prompt.clone(),
                }],
                ..Default::default()
            };
            let response = registry.chat_completion(request).await?;
            println!("Model: {}", response.model);
            println!("Content: {}", response.choices[0].message.content);
        }
        Commands::Init { force } => {
            agentgate::config::write_default_config(*force).await?;
            println!("Config file written to default location.");
        }
    }

    Ok(())
}

