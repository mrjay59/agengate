//! Verified HTTP provider endpoint mappings.
//!
//! This module is **data**, not routing logic (spec §6.3). It centralises the
//! confirmed OpenAI-compatible upstream mappings so the HTTP runtime, router,
//! model-catalog sync and admin API all read from one source of truth.
//!
//! Only entries with `mapping_status = Verified` may be enabled by default.
//! NVIDIA NIM is verified against <https://docs.api.nvidia.com/nim/reference/llm-apis>.

use crate::config::ProviderConfig;

/// Provider id used in AgentGate config and model IDs (`agg/nvidia/<model>`).
pub const NVIDIA_PROVIDER_ID: &str = "nvidia";
/// NVIDIA NIM cloud inference base URL (OpenAI-compatible).
pub const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
/// Confirmed chat completions path (relative to `NVIDIA_BASE_URL`).
pub const NVIDIA_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
/// Model catalog path (relative to `NVIDIA_BASE_URL`).
pub const NVIDIA_MODELS_PATH: &str = "/models";
/// Auth scheme declared by the NIM OpenAPI definition (`securitySchemes.Token`).
pub const NVIDIA_AUTH_TYPE: &str = "bearer";
/// Environment variable holding the `nvapi-*` key.
pub const NVIDIA_API_KEY_ENV: &str = "NVIDIA_API_KEY";
/// NVIDIA API keys are issued with this prefix.
pub const NVIDIA_API_KEY_PREFIX: &str = "nvapi-";
/// Adapter used for all OpenAI-compatible chat upstreams.
pub const OPENAI_COMPATIBLE_ADAPTER: &str = "openai_compatible_chat";
/// Provider type token for HTTP upstreams.
pub const HTTP_PROVIDER_TYPE: &str = "http_openai_compatible";
/// Example model from the published NIM catalog.
pub const NVIDIA_DEFAULT_MODEL: &str = "meta/llama-3.3-70b-instruct";
/// Source used to verify the mapping.
pub const NVIDIA_SOURCE: &str = "https://docs.api.nvidia.com/nim/reference/llm-apis";

/// Confirmed Kilo AI Gateway base URL (user-confirmed aliases: `kilo`, `kilocode`).
pub const KILO_BASE_URL: &str = "https://ai-gateway.kilo.ai/api/v1";
pub const KILO_API_KEY_ENV: &str = "KILO_API_KEY";
pub const KILO_SOURCE: &str = "https://kilo.ai/docs/gateway/api-reference";

/// Confirmed Agnes AI base URL (user-provided, verified in official docs).
pub const AGNES_BASE_URL: &str = "https://apihub.agnes-ai.com/v1";
pub const AGNES_API_KEY_ENV: &str = "AGNES_API_KEY";
pub const AGNES_SOURCE: &str = "https://www.agnes-ai.com/en/docs/overview";
/// `agnes-2.5-flash` (the example in the task) retires 2026-11-30; prefer 3.0.
pub const AGNES_DEFAULT_MODEL: &str = "agnes-3.0-flash";
pub const AGNES_SEED_MODELS: &[&str] = &["agnes-3.0-flash", "agnes-2.5-flash", "agnes-2.5-pro"];

pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const OPENROUTER_API_KEY_ENV: &str = "OPENROUTER_API_KEY";
pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const GROQ_API_KEY_ENV: &str = "GROQ_API_KEY";
pub const GEMINI_HTTP_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
pub const GEMINI_HTTP_API_KEY_ENV: &str = "GEMINI_API_KEY";

/// OpenCode HTTP target is deliberately NOT registered as a verified mapping.
/// User decision: keep the existing `opencode` CLI/bridge provider and add a
/// separate HTTP provider id (`opencode-http`) only when the operator chooses
/// an explicit base URL. Candidates (spec §6.2, still open):
///   - OpenCode Go:       https://opencode.ai/inference/go/openai/v1
///   - Pay-as-you-go:     https://opencode.ai/inference/openai/v1
///   - Local `opencode serve` OpenAI-compatible endpoint (configurable)
/// No silent CLI -> HTTP switch. See docs/AGENTGATE_HTTP_CHAT_COMPLETIONS_SPEC.md §18.

/// Seed model IDs taken from the published NIM LLM catalog. These are a
/// fallback only; `GET /models` sync is preferred when the provider is enabled.
pub const NVIDIA_SEED_MODELS: &[&str] = &[
    "meta/llama-3.3-70b-instruct",
    "meta/llama-3.1-8b-instruct",
    "meta/llama-3.1-70b-instruct",
    "nvidia/llama-3.3-nemotron-super-49b-v1",
    "qwen/qwen3-next-80b-a3b-instruct",
    "deepseek-ai/deepseek-v4-flash",
];

/// HTTP statuses treated as quota/rate-limit by the failover policy.
pub const QUOTA_EXHAUSTED_STATUSES: &[u16] = &[429];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingStatus {
    Verified,
    Unverified,
}

impl MappingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MappingStatus::Verified => "verified",
            MappingStatus::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderMapping {
    /// Canonical provider id.
    pub id: &'static str,
    /// Accepted aliases (e.g. `kilocode` -> `kilo`).
    pub aliases: &'static [&'static str],
    pub base_url: &'static str,
    pub chat_completions_path: &'static str,
    pub models_path: &'static str,
    pub auth_type: &'static str,
    pub api_key_env: &'static str,
    /// Expected key prefix; `None` when no prefix requirement is documented.
    pub api_key_prefix: Option<&'static str>,
    pub adapter: &'static str,
    pub provider_type: &'static str,
    pub mode: &'static str,
    pub default_model: Option<&'static str>,
    /// Fallback catalog used when live `/models` sync is unavailable.
    pub seed_models: &'static [&'static str],
    pub forward_tools: bool,
    pub forward_tool_choice: bool,
    pub forward_response_format: bool,
    pub forward_stream: bool,
    pub quota_exhausted_statuses: &'static [u16],
    pub status: MappingStatus,
    pub source: &'static str,
}

impl ProviderMapping {
    /// True when this mapping may be enabled without further confirmation.
    pub fn is_verified(&self) -> bool {
        self.status == MappingStatus::Verified
    }

    /// Build the config block for this mapping.
    ///
    /// The returned provider is **disabled** by design: enabling an HTTP
    /// upstream requires an explicit operator decision (and a configured key).
    pub fn to_provider_config(&self) -> ProviderConfig {
        ProviderConfig {
            id: self.id.to_string(),
            enabled: false,
            bin: None,
            models: self.seed_models.iter().map(|m| m.to_string()).collect(),
            models_cmd: None,
            chat_cmd: None,
            strip_ansi: false,
            base_url: Some(self.base_url.to_string()),
            num_ctx: None,
            bridge: false,
            transport: None,
            model_no_tool_support: Vec::new(),
            reasoning: crate::config::ReasoningConfig::default(),
            reasoning_effort: None,
            thinking_budget_tokens: None,
            ollama_mode: None,
            keep_alive: None,
            ollama_stream_timeout: None,
            prompt_tools_fallback: false,
            mode: self.mode.to_string(),
            provider_type: self.provider_type.to_string(),
            adapter: self.adapter.to_string(),
            chat_completions_path: self.chat_completions_path.to_string(),
            models_path: self.models_path.to_string(),
            auth_type: self.auth_type.to_string(),
            api_key_env: Some(self.api_key_env.to_string()),
            api_key: None,
            api_key_prefix: self.api_key_prefix.map(str::to_string),
            mapping_status: self.status.as_str().to_string(),
            timeout_secs: 120,
            max_retries: 2,
            retry_backoff_ms: vec![250, 1000],
            forward_tools: self.forward_tools,
            forward_tool_choice: self.forward_tool_choice,
            forward_response_format: self.forward_response_format,
            forward_stream: self.forward_stream,
            stream_idle_timeout_secs: 120,
            verify_tls: true,
            default_model: self.default_model.map(str::to_string),
            quota_exhausted_statuses: self.quota_exhausted_statuses.to_vec(),
        }
    }
}

/// NVIDIA NIM cloud inference (OpenAI-compatible).
///
/// Notes verified from the official API reference:
/// - `POST /v1/chat/completions` with `Authorization: Bearer <nvapi-*>`;
/// - `stream: true` emits SSE `data: {...}` chunks terminated by `data: [DONE]`;
/// - `tools` / `tool_calls` / `finish_reason: "tool_calls"` are supported;
/// - `usage.prompt_tokens|completion_tokens|total_tokens` is returned;
/// - NIM `ChatRequest` sets `additionalProperties = false` and does not declare
///   `tool_choice`, `response_format` or `seed` for several models, so those
///   fields are **not** forwarded by default (they can trigger HTTP 422);
/// - errors are returned as 422 (validation) / 500 (invocation) with
///   `{type,title,status,detail,instance,requestId}`; rate-limit/quota
///   exhaustion surfaces as 429 and is classified as `rate_limit`.
pub fn nvidia() -> ProviderMapping {
    ProviderMapping {
        id: NVIDIA_PROVIDER_ID,
        aliases: &[],
        base_url: NVIDIA_BASE_URL,
        chat_completions_path: NVIDIA_CHAT_COMPLETIONS_PATH,
        models_path: NVIDIA_MODELS_PATH,
        auth_type: NVIDIA_AUTH_TYPE,
        api_key_env: NVIDIA_API_KEY_ENV,
        api_key_prefix: Some(NVIDIA_API_KEY_PREFIX),
        adapter: OPENAI_COMPATIBLE_ADAPTER,
        provider_type: HTTP_PROVIDER_TYPE,
        mode: "http",
        default_model: Some(NVIDIA_DEFAULT_MODEL),
        seed_models: NVIDIA_SEED_MODELS,
        forward_tools: true,
        forward_tool_choice: false,
        forward_response_format: false,
        forward_stream: true,
        quota_exhausted_statuses: QUOTA_EXHAUSTED_STATUSES,
        status: MappingStatus::Verified,
        source: NVIDIA_SOURCE,
    }
}

fn http_mapping(
    id: &'static str,
    aliases: &'static [&'static str],
    base_url: &'static str,
    api_key_env: &'static str,
    default_model: Option<&'static str>,
    seed_models: &'static [&'static str],
    source: &'static str,
) -> ProviderMapping {
    ProviderMapping {
        id,
        aliases,
        base_url,
        chat_completions_path: "/chat/completions",
        models_path: "/models",
        auth_type: "bearer",
        api_key_env,
        api_key_prefix: None,
        adapter: OPENAI_COMPATIBLE_ADAPTER,
        provider_type: HTTP_PROVIDER_TYPE,
        mode: "http",
        default_model,
        seed_models,
        forward_tools: true,
        forward_tool_choice: true,
        forward_response_format: true,
        forward_stream: true,
        quota_exhausted_statuses: QUOTA_EXHAUSTED_STATUSES,
        status: MappingStatus::Verified,
        source,
    }
}

/// OpenRouter (confirmed OpenAI-compatible chat completions).
pub fn openrouter() -> ProviderMapping {
    http_mapping(
        "openrouter", &[], OPENROUTER_BASE_URL, OPENROUTER_API_KEY_ENV,
        Some("openrouter/auto"), &[], "https://openrouter.ai/docs/api-reference/chat-completion",
    )
}

/// Groq (confirmed OpenAI-compatible chat completions).
pub fn groq() -> ProviderMapping {
    http_mapping(
        "groq", &[], GROQ_BASE_URL, GROQ_API_KEY_ENV,
        Some("llama-3.1-8b-instant"), &[], "https://console.groq.com/docs/openai",
    )
}

/// Gemini via Google AI Studio OpenAI-compatibility layer.
/// Kept separate from the existing CLI `gemini` provider id.
pub fn gemini_http() -> ProviderMapping {
    http_mapping(
        "gemini-http", &[], GEMINI_HTTP_BASE_URL, GEMINI_HTTP_API_KEY_ENV,
        Some("gemini-2.5-flash"), &[], "https://ai.google.dev/gemini-api/docs/openai",
    )
}

/// Kilo AI Gateway. Both `kilo` and `kilocode` resolve to this canonical mapping
/// by user decision. Official docs confirm `GET /models`, tool calls, SSE and
/// HTTP 429 for rate limits.
pub fn kilo() -> ProviderMapping {
    http_mapping(
        "kilo", &["kilocode"], KILO_BASE_URL, KILO_API_KEY_ENV,
        None, &[], KILO_SOURCE,
    )
}

/// Agnes AI (OpenAI-compatible). Base URL and Bearer auth verified in official
/// docs; `POST /v1/chat/completions`, SSE streaming, tools/tool_choice supported.
pub fn agnes_ai() -> ProviderMapping {
    http_mapping(
        "agnes-ai", &[], AGNES_BASE_URL, AGNES_API_KEY_ENV,
        Some(AGNES_DEFAULT_MODEL), AGNES_SEED_MODELS, AGNES_SOURCE,
    )
}

/// All mappings that have a confirmed source. Unverified providers are
/// intentionally absent: they must be added only after confirmation.
pub fn verified_mappings() -> Vec<ProviderMapping> {
    vec![nvidia(), openrouter(), groq(), gemini_http(), kilo(), agnes_ai()]
}

/// Resolve a provider id (including aliases) to a verified mapping.
pub fn resolve(provider_id: &str) -> Option<ProviderMapping> {
    let id = provider_id.trim();
    verified_mappings()
        .into_iter()
        .find(|m| m.id.eq_ignore_ascii_case(id) || m.aliases.iter().any(|a| a.eq_ignore_ascii_case(&id)))
}

/// Validate a NVIDIA API key read from the environment.
///
/// Returns `None` for a missing/blank key or a key without the documented
/// `nvapi-` prefix, so misconfiguration fails closed instead of sending a
/// request that would be rejected upstream.
pub fn nvidia_api_key_from_env() -> Option<String> {
    let key = std::env::var(NVIDIA_API_KEY_ENV).ok()?;
    let key = key.trim();
    if is_nvidia_api_key(key) {
        Some(key.to_string())
    } else {
        None
    }
}

pub fn is_nvidia_api_key(key: &str) -> bool {
    let key = key.trim();
    key.len() > NVIDIA_API_KEY_PREFIX.len()
        && key.starts_with(NVIDIA_API_KEY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_mapping_matches_verified_endpoint() {
        let m = nvidia();
        assert_eq!(m.id, "nvidia");
        assert!(m.is_verified());
        assert_eq!(m.base_url, "https://integrate.api.nvidia.com/v1");
        assert_eq!(m.chat_completions_path, "/chat/completions");
        assert_eq!(m.models_path, "/models");
        assert_eq!(m.auth_type, "bearer");
        assert_eq!(m.api_key_env, "NVIDIA_API_KEY");
        assert_eq!(m.api_key_prefix, Some("nvapi-"));
        assert_eq!(m.adapter, "openai_compatible_chat");
        assert_eq!(m.provider_type, "http_openai_compatible");
        assert_eq!(m.mode, "http");
        assert_eq!(m.default_model, Some("meta/llama-3.3-70b-instruct"));
        assert!(m.forward_tools);
        assert!(m.forward_stream);
        assert!(!m.forward_tool_choice, "NIM rejects undeclared tool_choice (422)");
        assert!(!m.forward_response_format, "NIM rejects undeclared response_format (422)");
        assert!(m.quota_exhausted_statuses.contains(&429));
        assert_eq!(m.source, NVIDIA_SOURCE);
    }

    #[test]
    fn resolve_finds_nvidia_and_rejects_unknown() {
        let resolved = resolve("nvidia").expect("nvidia mapping must resolve");
        assert_eq!(resolved.base_url, NVIDIA_BASE_URL);
        assert!(resolve("not-a-provider").is_none());
    }

    #[test]
    fn kilo_and_kilocode_are_aliases_of_the_same_mapping() {
        let canonical = resolve("kilo").expect("kilo mapping must resolve");
        let alias = resolve("kilocode").expect("kilocode alias must resolve");
        assert_eq!(canonical.id, "kilo");
        assert_eq!(alias.id, "kilo");
        assert_eq!(canonical.base_url, KILO_BASE_URL);
        assert_eq!(canonical.api_key_env, "KILO_API_KEY");
        assert!(canonical.aliases.contains(&"kilocode"));
    }

    #[test]
    fn agnes_ai_mapping_is_confirmed() {
        let m = resolve("agnes-ai").expect("agnes-ai mapping must resolve");
        assert!(m.is_verified());
        assert_eq!(m.base_url, AGNES_BASE_URL);
        assert_eq!(m.api_key_env, "AGNES_API_KEY");
        assert_eq!(m.default_model, Some("agnes-3.0-flash"));
        assert!(m.seed_models.contains(&"agnes-2.5-flash"));
    }

    #[test]
    fn opencode_http_is_not_auto_registered() {
        // User decision: CLI and HTTP stay separate; no guessed default URL.
        assert!(resolve("opencode-http").is_none());
        assert!(resolve("opencode-go").is_none());
        assert_eq!(resolve("opencode").is_none(), true, "CLI provider is not an HTTP mapping");
    }

    #[test]
    fn nvidia_config_is_disabled_until_operator_enables_it() {
        let cfg = nvidia().to_provider_config();
        assert!(!cfg.enabled);
        assert!(!cfg.bridge, "HTTP providers must not spawn a CLI/bridge process");
        assert_eq!(cfg.id, "nvidia");
        assert_eq!(cfg.provider_type, "http_openai_compatible");
        assert_eq!(cfg.mode, "http");
        assert_eq!(cfg.base_url.as_deref(), Some(NVIDIA_BASE_URL));
        assert_eq!(cfg.chat_completions_path, "/chat/completions");
        assert_eq!(cfg.models_path, "/models");
        assert_eq!(cfg.auth_type, "bearer");
        assert_eq!(cfg.api_key_env.as_deref(), Some("NVIDIA_API_KEY"));
        assert_eq!(cfg.api_key_prefix.as_deref(), Some("nvapi-"));
        assert_eq!(cfg.mapping_status, "verified");
        assert!(cfg.models.iter().any(|m| m == NVIDIA_DEFAULT_MODEL));
        assert!(!cfg.quota_exhausted_statuses.is_empty());
    }

    #[test]
    fn nvidia_key_validation() {
        assert!(is_nvidia_api_key("nvapi-abc123"));
        assert!(!is_nvidia_api_key("nvapi-"));
        assert!(!is_nvidia_api_key("sk-abc123"));
        assert!(!is_nvidia_api_key("  "));
    }

    #[test]
    fn toml_config_parses_nvidia_mapping() {
        let toml = r#"
[[provider]]
id = "nvidia"
type = "http_openai_compatible"
mode = "http"
adapter = "openai_compatible_chat"
base_url = "https://integrate.api.nvidia.com/v1"
chat_completions_path = "/chat/completions"
models_path = "/models"
auth_type = "bearer"
api_key_env = "NVIDIA_API_KEY"
api_key_prefix = "nvapi-"
mapping_status = "verified"
enabled = false
bridge = false
default_model = "meta/llama-3.3-70b-instruct"
models = ["meta/llama-3.3-70b-instruct"]
forward_tools = true
forward_tool_choice = false
forward_response_format = false
forward_stream = true
quota_exhausted_statuses = [429]
"#;
        let cfg: crate::config::Config = toml::from_str(toml).expect("valid nvidia provider config");
        let p = cfg.provider.iter().find(|p| p.id == "nvidia").expect("nvidia provider");
        assert_eq!(p.provider_type, "http_openai_compatible");
        assert_eq!(p.mode, "http");
        assert_eq!(p.base_url.as_deref(), Some(NVIDIA_BASE_URL));
        assert_eq!(p.api_key_env.as_deref(), Some("NVIDIA_API_KEY"));
        assert_eq!(p.quota_exhausted_statuses, vec![429]);
        assert!(!p.forward_tool_choice);
    }
}
