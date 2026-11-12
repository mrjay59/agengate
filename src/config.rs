use crate::permission::{ClientToolsPolicy, NoToolsPolicy};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub bridge: BridgeConfig,
    #[serde(default)]
    pub reasoning: ReasoningConfig,
    #[serde(default)]
    pub provider: Vec<ProviderConfig>,
    /// `[workspace.<name>]` sections (spec §14).
    #[serde(default)]
    pub workspace: std::collections::HashMap<String, WorkspaceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub no_tools_policy: NoToolsPolicy,
    #[serde(default)]
    pub on_client_tools: ClientToolsPolicy,
    #[serde(default = "default_workspace")]
    pub default_workspace: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceConfig {
    /// Only used in passthrough mode (spec §14).
    #[serde(default = "default_dot")]
    pub path: String,
    #[serde(default = "default_workspace_permission_mode")]
    pub permission_mode: String,
}

fn default_dot() -> String { ".".to_string() }
fn default_workspace_permission_mode() -> String { "read-only".to_string() }

#[derive(Debug, Clone, Deserialize)]
pub struct BatchConfig {
    /// Window to wait for parallel tool calls before issuing a batch.
    #[serde(default = "default_150")]
    pub window_ms: u64,
    /// Maximum tool calls per batch.
    #[serde(default = "default_16")]
    pub max_calls: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self { window_ms: default_150(), max_calls: default_16() }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResumeConfig {
    /// On unknown tool_call_id, replay with a fresh bridge session.
    #[serde(default = "default_true")]
    pub replay: bool,
    /// Wait for missing batch results (ms). 0 = don't wait.
    #[serde(default)]
    pub partial_grace_ms: u64,
    /// Allow replaying the same request idempotently after a broken response.
    #[serde(default)]
    pub allow_retry: bool,
}

impl Default for ResumeConfig {
    fn default() -> Self {
        Self { replay: true, partial_grace_ms: 0, allow_retry: false }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    /// Maximum client tools per request.
    #[serde(default = "default_128")]
    pub max_tools: usize,
    /// Maximum serialized bytes per tool schema.
    #[serde(default = "default_65536")]
    pub max_schema_bytes: usize,
    /// Maximum bytes for a tool result.
    #[serde(default = "default_262144")]
    pub max_result_bytes: usize,
    /// Maximum prompt characters.
    #[serde(default = "default_400000")]
    pub max_prompt_chars: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_tools: default_128(),
            max_schema_bytes: default_65536(),
            max_result_bytes: default_262144(),
            max_prompt_chars: default_400000(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuardConfig {
    /// Kill the run when a non-bridge tool is observed.
    #[serde(default = "default_true")]
    pub enforce: bool,
    /// Run the isolation preflight probe before admitting a bridge provider.
    #[serde(default = "default_true")]
    pub preflight_probe: bool,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self { enforce: true, preflight_probe: true }
    }
}

impl Config {
    /// Apply environment overrides (highest priority after CLI flags).
    /// Supported: AGENTGATE_BRIDGE_PORT, AGENTGATE_BRIDGE_CALL_TTL_SECS,
    /// AGENTGATE_BRIDGE_PARK_TTL_SECS, AGENTGATE_BRIDGE_MAX_PARKED_RUNS,
    /// AGENTGATE_BRIDGE_TRANSPORT.
    pub fn apply_env_overrides(mut self) -> Self {
        if let Ok(v) = std::env::var("AGENTGATE_BRIDGE_PORT") {
            if let Ok(p) = v.parse::<u16>() {
                self.bridge.port = p;
            }
        }
        if let Ok(v) = std::env::var("AGENTGATE_BRIDGE_CALL_TTL_SECS") {
            if let Ok(p) = v.parse() {
                self.bridge.call_ttl_secs = p;
            }
        }
        if let Ok(v) = std::env::var("AGENTGATE_BRIDGE_PARK_TTL_SECS") {
            if let Ok(p) = v.parse() {
                self.bridge.park_ttl_secs = p;
            }
        }
        if let Ok(v) = std::env::var("AGENTGATE_BRIDGE_MAX_PARKED_RUNS") {
            if let Ok(p) = v.parse() {
                self.bridge.max_parked_runs = p;
            }
        }
        if let Ok(v) = std::env::var("AGENTGATE_BRIDGE_TRANSPORT") {
            self.bridge.transport = Some(v);
        }
        self
    }
}

fn default_150() -> u64 { 150 }
fn default_16() -> usize { 16 }
fn default_128() -> usize { 128 }
fn default_65536() -> usize { 65536 }
fn default_262144() -> usize { 262144 }
fn default_400000() -> usize { 400_000 }

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            no_tools_policy: NoToolsPolicy::default(),
            on_client_tools: ClientToolsPolicy::default(),
            default_workspace: default_workspace(),
        }
    }
}

fn default_workspace() -> String { "default".to_string() }

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_bridge_host")]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_600")]
    pub park_ttl_secs: u64,
    #[serde(default = "default_600")]
    pub call_ttl_secs: u64,
    #[serde(default = "default_15")]
    pub keepalive_interval_secs: u64,
    #[serde(default = "default_8")]
    pub max_parked_runs: usize,
    #[serde(default = "default_4")]
    pub max_runs_per_key: usize,
    #[serde(default = "default_3600")]
    pub max_run_lifetime_secs: u64,
    #[serde(default = "default_64")]
    pub max_steps: usize,
    #[serde(default = "default_true")]
    pub parked_holds_permit: bool,
    #[serde(default = "default_server_name")]
    pub server_name: String,
    /// `[bridge.batch]` (spec §14).
    #[serde(default)]
    pub batch: BatchConfig,
    /// `[bridge.resume]` (spec §14).
    #[serde(default)]
    pub resume: ResumeConfig,
    /// `[bridge.limits]` (spec §14).
    #[serde(default)]
    pub limits: LimitsConfig,
    /// `[bridge.guard]` (spec §14).
    #[serde(default)]
    pub guard: GuardConfig,
    /// MCP transport: `"http"` (loopback MCP over HTTP) or `"stdio_shim"`
    /// (stdlib MCP shim for CLI agents without native MCP support).
    /// Spec §9.7 / §12.2.
    #[serde(default)]
    pub transport: Option<String>,
    /// `[bridge.isolation]` (spec §13.2 / TB-TOOL-02).
    /// Controls selective disabling of built-in OpenCode tools.
    /// Modes:
    ///   - "selective" (default): disable conflicting built-ins (read, write, bash/execute, glob, list, patch), allow grep, webfetch, todo*, task
    ///   - "strict": disable all built-in tools
    ///   - "off": no tool disabling
    #[serde(default)]
    pub isolation: IsolationConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IsolationConfig {
    #[serde(default = "default_isolation_mode")]
    pub mode: String,
    /// Additional built-in tools to allow beyond the mode defaults.
    #[serde(default)]
    pub allowed_builtins: Vec<String>,
    /// Override: explicitly disable these built-in tools.
    #[serde(default)]
    pub disabled_builtins: Vec<String>,
    /// When true (default), skip verbose `permission` block in opencode.json for free models
    /// to avoid provider.free-limit errors (beam-free etc.). Keep `tools` block for safety.
    #[serde(default = "default_true")]
    pub minimal_free_model: bool,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            mode: default_isolation_mode(),
            allowed_builtins: Vec::new(),
            disabled_builtins: Vec::new(),
            minimal_free_model: true,
        }
    }
}

fn default_isolation_mode() -> String { "selective".to_string() }

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            host: default_bridge_host(),
            port: 0,
            park_ttl_secs: default_600(),
            call_ttl_secs: default_600(),
            keepalive_interval_secs: default_15(),
            max_parked_runs: default_8(),
            max_runs_per_key: default_4(),
            max_run_lifetime_secs: default_3600(),
            max_steps: default_64(),
            parked_holds_permit: default_true(),
            server_name: default_server_name(),
            transport: None,
            batch: BatchConfig::default(),
            resume: ResumeConfig::default(),
            limits: LimitsConfig::default(),
            guard: GuardConfig::default(),
            isolation: IsolationConfig::default(),
        }
    }
}

fn default_true() -> bool { true }
fn default_bridge_host() -> String { "127.0.0.1".to_string() }
fn default_600() -> u64 { 600 }
fn default_15() -> u64 { 15 }
fn default_8() -> usize { 8 }
fn default_4() -> usize { 4 }
fn default_3600() -> u64 { 3600 }
fn default_64() -> usize { 64 }
fn default_server_name() -> String { "agentgate".to_string() }

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default)]
    pub cors: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DefaultsConfig {
    #[serde(default = "default_cwd")]
    pub workdir: PathBuf,
    #[serde(default = "default_concurrency")]
    pub max_concurrency_per_provider: usize,
    #[serde(default = "default_cache_ttl")]
    pub model_cache_ttl_secs: u64,
    #[serde(default = "default_first_token_timeout")]
    pub first_token_timeout_secs: u64,
}

fn default_first_token_timeout() -> u64 { 120 }

#[derive(Debug, Clone, Deserialize)]
pub struct ReasoningConfig {
    #[serde(default = "default_reasoning_format")]
    pub format: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_40")]
    pub flush_ms: u64,
    #[serde(default = "default_256")]
    pub flush_bytes: usize,
    /// Send `"content": null` while reasoning deltas are being streamed.
    #[serde(default = "default_true")]
    pub null_content: bool,
    /// Emit the assistant role chunk before the CLI produces output.
    #[serde(default = "default_true")]
    pub early_role_chunk: bool,
    /// Hard cap on reasoning characters forwarded for a single turn.
    #[serde(default = "default_max_reasoning_chars")]
    pub max_chars_per_turn: usize,
    /// Hard cap on reasoning blocks per turn.
    #[serde(default = "default_max_reasoning_blocks")]
    pub max_blocks_per_turn: u32,

    /// Fallback behaviour when the CLI/model exposes no reasoning: `activity` | `none` | `induce`.
    #[serde(default = "default_activity")]
    pub fallback: String,
    #[serde(default = "default_true")]
    pub activity_via_reasoning: bool,
    #[serde(default)]
    pub activity_args: bool,
    #[serde(default = "default_activity_interval")]
    pub activity_min_interval_ms: u64,
    #[serde(default = "default_activity_lang")]
    pub activity_lang: String,
    #[serde(default = "default_activity_prefix")]
    pub activity_prefix: String,
    /// Marker for omitted thinking blocks: `activity` | `none`.
    #[serde(default = "default_activity")]
    pub omitted_marker: String,

    /// Best-effort redaction patterns applied to reasoning/activity text before it reaches clients.
    #[serde(default = "default_redact_patterns")]
    pub redact_patterns: Vec<String>,

    // Provider-specific knobs (spec_reasoning §12).
    #[serde(default = "default_true")]
    pub partial_messages: bool,
    #[serde(default = "default_reasoning_source")]
    pub source: String,
    #[serde(default = "default_true")]
    pub lenient_events: bool,
    #[serde(default = "default_true")]
    pub use_native_api: bool,
    #[serde(default)]
    pub replay_thinking: bool,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            format: default_reasoning_format(),
            enabled: true,
            flush_ms: default_40(),
            flush_bytes: default_256(),
            null_content: true,
            early_role_chunk: true,
            max_chars_per_turn: default_max_reasoning_chars(),
            max_blocks_per_turn: default_max_reasoning_blocks(),
            fallback: default_activity(),
            activity_via_reasoning: true,
            activity_args: false,
            activity_min_interval_ms: default_activity_interval(),
            activity_lang: default_activity_lang(),
            activity_prefix: default_activity_prefix(),
            omitted_marker: default_activity(),
            redact_patterns: default_redact_patterns(),
            partial_messages: true,
            source: default_reasoning_source(),
            lenient_events: true,
            use_native_api: true,
            replay_thinking: false,
        }
    }
}

fn default_reasoning_format() -> String { "reasoning_content".to_string() }
fn default_40() -> u64 { 40 }
fn default_256() -> usize { 256 }
fn default_max_reasoning_chars() -> usize { 200_000 }
fn default_max_reasoning_blocks() -> u32 { 64 }
fn default_activity() -> String { "activity".to_string() }
fn default_activity_interval() -> u64 { 1500 }
fn default_activity_lang() -> String { "id".to_string() }
fn default_activity_prefix() -> String { "⏳ ".to_string() }
fn default_reasoning_source() -> String { "run".to_string() }
fn default_redact_patterns() -> Vec<String> {
    vec![
        r"(?i)api[_-]?key\s*[:=]\s*\S+".to_string(),
        r"sk-[A-Za-z0-9_-]{16,}".to_string(),
    ]
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderConfig {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bin: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub models_cmd: Option<Vec<String>>,
    #[serde(default)]
    pub chat_cmd: Option<Vec<String>>,
    #[serde(default)]
    pub strip_ansi: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub num_ctx: Option<usize>,
    #[serde(default = "default_true")]
    pub bridge: bool,
    #[serde(default)]
    pub transport: Option<String>,
    /// Models that don't support tool calling (for model_no_tool_support check)
    #[serde(default)]
    pub model_no_tool_support: Vec<String>,
    /// Reasoning configuration for this provider
    #[serde(default)]
    pub reasoning: ReasoningConfig,
    /// Reasoning effort: "low" | "medium" | "high"
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Budget tokens for thinking/reasoning
    #[serde(default)]
    pub thinking_budget_tokens: Option<usize>,
    /// Ollama mode: "native" | "cli"
    #[serde(default)]
    pub ollama_mode: Option<String>,
    /// Ollama keep-alive duration for CLI mode (e.g. "10m").
    #[serde(default)]
    pub keep_alive: Option<String>,
    /// Ollama stream timeout in seconds.
    #[serde(default)]
    pub ollama_stream_timeout: Option<u64>,
    /// When true, expose tools to a CLI/model that lacks native tool calling by
    /// prompting for JSON tool calls and parsing them from the output.
    #[serde(default)]
    pub prompt_tools_fallback: bool,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8787
}

fn default_timeout() -> u64 {
    600
}

fn default_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn default_concurrency() -> usize {
    2
}

fn default_cache_ttl() -> u64 {
    300
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            defaults: DefaultsConfig::default(),
            routing: RoutingConfig::default(),
            bridge: BridgeConfig::default(),
            reasoning: ReasoningConfig::default(),
            provider: Vec::new(),
            workspace: std::collections::HashMap::new(),
        }
    }
}

/// Load config from TOML file
pub async fn load_config() -> anyhow::Result<Config> {
    // Try to find config file
    let config_path = find_config_path()?;
    
    if config_path.exists() {
        let content = fs::read_to_string(&config_path).await?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    } else {
        Ok(Config::default())
    }
}

/// Find config file in order: CLI flag, ENV var, XDG home, APPDATA
fn find_config_path() -> anyhow::Result<PathBuf> {
    // 1. Check current directory first
    let cwd_path = std::env::current_dir()?.join("agentgate.toml");
    if cwd_path.exists() {
        return Ok(cwd_path);
    }

    // 2. Try XDG config dir
    if let Some(config_dir) = dirs::config_dir() {
        let path = config_dir.join("agentgate.toml");
        if path.exists() {
            return Ok(path);
        }
    }

    // 3. Fallback to current directory (for creation)
    Ok(std::env::current_dir()?.join("agentgate.toml"))
}

/// Create default config file
pub async fn write_default_config(force: bool) -> anyhow::Result<()> {
    let config_content = r#"# AgentGate Configuration
# Generated by agentgate init

[server]
host = "127.0.0.1"
port = 8787
api_keys = []
request_timeout_secs = 600
cors = false

[defaults]
workdir = "."
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300
# Fail fast: max seconds to wait for the first provider event on a streaming
# request before returning an error chunk (code = first_token_timeout).
first_token_timeout_secs = 120

[routing]
no_tools_policy = "chat_only"
on_client_tools = "bridge"
default_workspace = "default"

[bridge]
enabled = true
host = "127.0.0.1"
port = 0
park_ttl_secs = 600
call_ttl_secs = 600
max_parked_runs = 8
max_runs_per_key = 4
max_run_lifetime_secs = 3600
max_steps = 64
parked_holds_permit = true
server_name = "agentgate"

[bridge.batch]
window_ms = 150
max_calls = 16

[bridge.resume]
replay = true
partial_grace_ms = 0
allow_retry = false

[bridge.limits]
max_tools = 128
max_schema_bytes = 65536
max_result_bytes = 262144
max_prompt_chars = 400000

[bridge.guard]
enforce = true
preflight_probe = true

[bridge.isolation]
# mode = "selective" | "strict" | "off"
# selective: disable conflicting built-ins (read, write, bash/execute, glob, list, patch), allow grep, webfetch, todo*, task
# strict: disable all built-in tools
# off: no tool disabling
mode = "selective"
# Additional built-in tools to allow beyond the mode defaults
allowed_builtins = []
# Override: explicitly disable these built-in tools
disabled_builtins = []

[workspace.default]
path = "."
permission_mode = "read-only"

# Provider configurations
# [[provider]]
# id = "custom"
# bin = "mycli"
# models = ["model1", "model2"]
"#;
    
    let path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentgate.toml");
    
    if path.exists() && !force {
        anyhow::bail!("Config file already exists at {}. Use --force to overwrite.", path.display());
    }
    
    tokio::fs::write(&path, config_content).await?;
    tracing::info!("Config written to {}", path.display());
    Ok(())
}
