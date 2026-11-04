use std::path::PathBuf;
use std::sync::Arc;

use agentgate::api::api;
use agentgate::config::load_config;
use agentgate::provider::{CliProviderImpl, BridgeCliProvider};
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
    /// Diagnostics: provider/reasoning report, client matrix, synthetic probe
    Doctor {
        /// Serve timestamped synthetic reasoning streams (no model call)
        #[arg(long, default_value_t = false)]
        probe_reasoning: bool,
        /// Print the embedded client/9router matrix and exit
        #[arg(long, default_value_t = false)]
        matrix: bool,
        /// Bind host for the probe server
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Bind port for the probe server
        #[arg(long, default_value_t = 8799)]
        port: u16,
        /// Stop the probe server after N seconds
        #[arg(long, default_value_t = 120)]
        seconds: u64,
        /// Keep the probe server running until Ctrl+C
        #[arg(long, default_value_t = false)]
        follow: bool,
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

    let config = match &args.config {
        Some(path) => {
            let content = tokio::fs::read_to_string(path).await?;
            toml::from_str::<agentgate::config::Config>(&content)?
        }
        None => load_config().await?,
    };

    match &args.command {
        Commands::Serve { host, port } => {
            use tokio::net::TcpListener;
            let host = host.as_deref().unwrap_or(&config.server.host);
            let port = port.unwrap_or(config.server.port);
            let bind_addr = format!("{}:{}", host, port);
            let mut registry = ServiceRegistry::new().with_reasoning(config.reasoning.clone());
            for provider_cfg in &config.provider {
                // Use BridgeCliProvider for providers that support bridge architecture (spec_tooling.md)
                let provider: Box<dyn agentgate::provider::CliProvider> = match provider_cfg.id.as_str() {
                    "opencode" | "gemini" | "claude" => {
                        if let Some(bp) = BridgeCliProvider::new(provider_cfg.clone()) {
                            Box::new(bp)
                        } else {
                            Box::new(CliProviderImpl::new(provider_cfg.clone()))
                        }
                    }
                    _ => Box::new(CliProviderImpl::new(provider_cfg.clone())),
                };
                registry.register(provider).await;
            }
            registry.bridge.start_cleanup_task().await;
            let app = api().with_state(Arc::new(registry));
            // Wire CORS if enabled in config
            let app = if config.server.cors {
                use tower_http::cors::{Any, CorsLayer};
                app.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
            } else {
                app
            };
            // Bearer auth guard when api_keys configured
            let api_keys = config.server.api_keys.clone();
            let app = if !api_keys.is_empty() {
                app.layer(axum::middleware::from_fn(move |req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
                    let keys = api_keys.clone();
                    async move {
                        // Allow health/root without auth
                        let path = req.uri().path();
                        if path == "/health" || path == "/" {
                            return next.run(req).await;
                        }
                        let auth = req.headers().get(axum::http::header::AUTHORIZATION);
                        let ok = auth
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.strip_prefix("Bearer "))
                            .map(|t| keys.iter().any(|k| k == t))
                            .unwrap_or(false);
                        if ok {
                            next.run(req).await
                        } else {
                            axum::response::Response::builder()
                                .status(axum::http::StatusCode::UNAUTHORIZED)
                                .header(axum::http::header::CONTENT_TYPE, "application/json")
                                .body(axum::body::Body::from(r#"{"error":{"message":"Invalid API key","type":"invalid_request_error"}}"#))
                                .unwrap()
                        }
                    }
                }))
            } else {
                app
            };
            // Warn if binding non-loopback without keys
            if config.server.api_keys.is_empty() && (host == "0.0.0.0" || host == "::") {
                tracing::warn!("Binding to {} without api_keys: service is open to the network", host);
            }
            tracing::info!("Starting server on {}", bind_addr);
            let listener = TcpListener::bind(bind_addr.as_str()).await?;
            axum::serve(listener, app).await?;
        }
        Commands::Models { provider } => {
            let mut registry = ServiceRegistry::new().with_reasoning(config.reasoning.clone());
            for provider_cfg in &config.provider {
                // Use BridgeCliProvider for providers that support bridge architecture (spec_tooling.md)
                let provider: Box<dyn agentgate::provider::CliProvider> = match provider_cfg.id.as_str() {
                    "opencode" | "gemini" | "claude" => {
                        if let Some(bp) = BridgeCliProvider::new(provider_cfg.clone()) {
                            Box::new(bp)
                        } else {
                            Box::new(CliProviderImpl::new(provider_cfg.clone()))
                        }
                    }
                    _ => Box::new(CliProviderImpl::new(provider_cfg.clone())),
                };
                registry.register(provider).await;
            }
            let provider_filter = provider.as_deref();
            let models_response = registry.list_models(provider_filter).await?;

            println!("Available models:");
            for model in &models_response.data {
                println!("  {}/{}", model.provider, model.id);
            }
        }
        Commands::Test { model, prompt } => {
            let mut registry = ServiceRegistry::new().with_reasoning(config.reasoning.clone());
            for provider_cfg in &config.provider {
                // Use BridgeCliProvider for providers that support bridge architecture (spec_tooling.md)
                let provider: Box<dyn agentgate::provider::CliProvider> = match provider_cfg.id.as_str() {
                    "opencode" | "gemini" | "claude" => {
                        if let Some(bp) = BridgeCliProvider::new(provider_cfg.clone()) {
                            Box::new(bp)
                        } else {
                            Box::new(CliProviderImpl::new(provider_cfg.clone()))
                        }
                    }
                    _ => Box::new(CliProviderImpl::new(provider_cfg.clone())),
                };
                registry.register(provider).await;
            }
            let request = ChatCompletionRequest {
                model: model.clone(),
                messages: vec![ChatMessage {
                    role: MessageRole::User,
                    content: Some(prompt.clone()),
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
        }
        Commands::Init { force } => {
            agentgate::config::write_default_config(*force).await?;
            println!("Config file written to default location.");
        }
        Commands::Doctor {
            probe_reasoning,
            matrix,
            host,
            port,
            seconds,
            follow,
        } => {
            if *matrix {
                agentgate::doctor::print_matrix();
            } else if *probe_reasoning {
                let window = if *follow { None } else { Some(*seconds) };
                agentgate::doctor::run_probe(host, *port, window).await?;
            } else {
                let mut registry =
                    ServiceRegistry::new().with_reasoning(config.reasoning.clone());
                for provider_cfg in &config.provider {
                    registry
                        .register(Box::new(CliProviderImpl::new(provider_cfg.clone())))
                        .await;
                }
                agentgate::doctor::run_report(&registry).await;
            }
        }
    }

    Ok(())
}

