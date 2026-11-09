use std::path::PathBuf;
use std::sync::Arc;

use agentgate::api::api;
use agentgate::config::load_config;
use agentgate::service::ServiceRegistry;
use agentgate::types::{ChatCompletionRequest, ChatMessage, MessageRole};
use agentgate::provider::bridge::spike::{run_spike_bridge, SpikeConfig};
use agentgate::provider::bridge::shim::run_stdio_shim;
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
        /// Run full provider probe chain (D1-D4) per spec §16/§17
        #[arg(long, default_value_t = false)]
        probe_chain: bool,
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
    /// Spike-bridge prototype test for CLI bridge providers (spec_tooling.md §17)
    SpikeBridge { #[arg(long, default_value = "opencode")] cli: String, #[arg(long)] model: String, #[arg(long, default_value_t = 8799)] port: u16, #[arg(long, default_value = "")] prompt: String, #[arg(long, default_value_t = 600)] timeout: u64, },
    /// MCP stdio shim for CLI agents (bridge.transport = "stdio_shim")
    McpShim {
        /// Session ID
        #[arg(long)]
        session: String,
        /// Session token (Bearer)
        #[arg(long)]
        token: String,
        /// Bridge host
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Bridge port
        #[arg(long, default_value_t = 8799)]
        port: u16,
    },
}

/// Whether the config opts Ollama into CLI bridge mode.
fn ollama_cli_enabled(config: &agentgate::config::Config) -> bool {
    config
        .provider
        .iter()
        .any(|p| p.id == "ollama" && p.ollama_mode.as_deref() == Some("cli"))
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
    }.apply_env_overrides();

    match &args.command {
        Commands::Serve { host, port } => {
            use tokio::net::TcpListener;
            // Augment PATH with common CLI install locations (nvm node versions,
            // npm global, homebrew, ...) so providers like opencode (often
            // installed via nvm) are discoverable even in non-interactive
            // sessions (systemd, tmux without profile sourcing).
            let sep = if cfg!(target_os = "windows") { ';' } else { ':' };
            let mut path = std::env::var("PATH").unwrap_or_default();
            let mut extra: Vec<String> = Vec::new();
            if let Ok(home) = std::env::var("HOME") {
                let nvm_root = format!("{}/.nvm/versions/node", home);
                if let Ok(entries) = std::fs::read_dir(&nvm_root) {
                    for e in entries.flatten() {
                        let bin = e.path().join("bin");
                        if bin.is_dir() {
                            extra.push(bin.to_string_lossy().to_string());
                        }
                    }
                }
                let npm_global = format!("{}/.npm-global/bin", home);
                if std::path::Path::new(&npm_global).is_dir() {
                    extra.push(npm_global);
                }
            }
            for d in ["/usr/local/bin", "/usr/bin", "/opt/homebrew/bin"] {
                if std::path::Path::new(d).is_dir() {
                    extra.push(d.to_string());
                }
            }
            if cfg!(target_os = "windows") {
                if let Ok(appdata) = std::env::var("APPDATA") {
                    let npm = format!("{}\\npm", appdata);
                    if std::path::Path::new(&npm).is_dir() {
                        extra.push(npm);
                    }
                }
            }
            for d in extra {
                if !path.split(sep).any(|p| p == d) {
                    path = format!("{}{}{}", d, sep, path);
                }
            }
            std::env::set_var("PATH", &path);
            tracing::info!(path = %path, "Augmented PATH for CLI discovery");
            let host = host.as_deref().unwrap_or(&config.server.host);
            let port = port.unwrap_or(config.server.port);
            let bind_addr = format!("{}:{}", host, port);
            std::env::set_var("AGENTGATE_BRIDGE_PORT", port.to_string());
            let mut registry = ServiceRegistry::with_config(
                config.routing.clone(),
                config.bridge.clone(),
                config.reasoning.clone(),
                config.defaults.clone(),
                ollama_cli_enabled(&config),
            );
            for provider_cfg in &config.provider {
                // Bridge-capable agents use the MCP bridge adapter (spec_tooling.md §12);
                // set `bridge = false` on the provider to opt out.
                tracing::info!(id = %provider_cfg.id, transport = ?provider_cfg.transport, "Loading provider config");
                let provider = agentgate::provider::build_cli_provider(
                    provider_cfg,
                    Some(registry.bridge.clone()),
                );
                registry.register(provider).await;

                // Also register native (non-bridge) provider for chat-only requests
                // and for model scanning (models_cmd). Register for ALL providers
                // so /v1/models shows scanned models from CLI, not just configured.
                let native_provider = Some(Box::new(agentgate::provider::CliProviderImpl::new(provider_cfg.clone())) as Box<dyn agentgate::provider::CliProvider + Send + Sync>);
                if let Some(np) = native_provider {
                    registry.register_native(np).await;
                }
            }
            registry.bridge.sweep_orphan_session_dirs().await;
            registry.bridge.start_cleanup_task().await;
            let registry = Arc::new(registry);
            // Warm the model cache in the background so the first
            // `/v1/models` (and the UI's fetch-models check) is a cache hit
            // instead of paying the full CLI scan cost.
            {
                let warm = registry.clone();
                tokio::spawn(async move { warm.warm_model_cache().await });
            }
            let app = api().with_state(registry);
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
            let mut registry = ServiceRegistry::with_config(
                config.routing.clone(),
                config.bridge.clone(),
                config.reasoning.clone(),
                config.defaults.clone(),
                ollama_cli_enabled(&config),
            );
            for provider_cfg in &config.provider {
                // Bridge-capable agents use the MCP bridge adapter (spec_tooling.md §12);
                // set `bridge = false` on the provider to opt out.
                let provider = agentgate::provider::build_cli_provider(
                    provider_cfg,
                    Some(registry.bridge.clone()),
                );
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
            let mut registry = ServiceRegistry::new()
                .with_reasoning(config.reasoning.clone())
                .with_ollama_cli_mode(ollama_cli_enabled(&config));
            for provider_cfg in &config.provider {
                // Bridge-capable agents use the MCP bridge adapter (spec_tooling.md §12);
                // set `bridge = false` on the provider to opt out.
                let provider = agentgate::provider::build_cli_provider(
                    provider_cfg,
                    Some(registry.bridge.clone()),
                );
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
            probe_chain,
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
            } else if *probe_chain {
                let mut registry =
                    ServiceRegistry::with_config(
                        config.routing.clone(),
                        config.bridge.clone(),
                        config.reasoning.clone(),
                        config.defaults.clone(),
                        ollama_cli_enabled(&config),
                    );
                for provider_cfg in &config.provider {
                    let provider = agentgate::provider::build_cli_provider(
                        provider_cfg,
                        Some(registry.bridge.clone()),
                    );
                    registry.register(provider).await;
                }
                agentgate::doctor::run_probe_chain(&registry, host, *port, *seconds).await?;
            } else {
                let mut registry =
                    ServiceRegistry::with_config(
                        config.routing.clone(),
                        config.bridge.clone(),
                        config.reasoning.clone(),
                        config.defaults.clone(),
                        ollama_cli_enabled(&config),
                    );
                for provider_cfg in &config.provider {
                    let provider = agentgate::provider::build_cli_provider(
                        provider_cfg,
                        Some(registry.bridge.clone()),
                    );
                    registry.register(provider).await;
                }
                agentgate::doctor::run_report(&registry).await;
            }
        }
        Commands::SpikeBridge { cli, model, port, prompt, timeout } => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let config = match &args.config {
                Some(path) => {
                    let content = tokio::fs::read_to_string(path).await?;
                    toml::from_str::<agentgate::config::Config>(&content)?
                }
                None => agentgate::config::load_config().await?,
            }.apply_env_overrides();
            let _report = rt.block_on(run_spike_bridge(SpikeConfig {
                cli_type: cli.clone(),
                model: model.clone(),
                bridge_port: *port,
                session_id: None,
                keep_alive: Some(timeout.to_string()),
                prompt: prompt.clone(),
            }))?;
            println!("\nSpike-bridge completed.");
            eprintln!("Spike-bridge completed. (report saved if compat file written)");
        }
        Commands::McpShim { session, token, host, port } => {
            // Read tools from environment variable
            let tools_json = std::env::var("AGENTGATE_SHIM_TOOLS")
                .map_err(|e| anyhow::anyhow!("AGENTGATE_SHIM_TOOLS env var required: {}", e))?;
            let tools: Vec<agentgate::bridge::ToolSchema> = serde_json::from_str(&tools_json)
                .map_err(|e| anyhow::anyhow!("Failed to parse AGENTGATE_SHIM_TOOLS: {}", e))?;

            let config = agentgate::provider::bridge::shim::ShimConfig {
                session_id: session.clone(),
                session_token: token.clone(),
                bridge_host: host.clone(),
                bridge_port: *port,
                tools,
            };

            run_stdio_shim(config).await?;
        }
    }

    Ok(())
}

