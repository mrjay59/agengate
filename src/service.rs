use crate::bridge::BridgeServer;
use crate::config::{BridgeConfig, ReasoningConfig, RoutingConfig, DefaultsConfig, LimitsConfig};
use crate::error::AppError;
use crate::config::ProviderConfig;
use crate::provider::{CliProvider};
use crate::provider::combo::{ComboModel, ComboRegistry, FailureContext as ComboFailure, should_fallback};
use crate::provider::http::HttpOpenAiCompatibleProvider;
use crate::provider::ollama_native::OllamaNativeProvider;
use crate::reasoning::RunEvent;
use crate::run::classify;
use crate::run::RunRegistry;
use crate::types::{AddProviderRequest, ChatCompletionRequest, ChatCompletionResponse, Model, ModelsResponse, ToolCall};
use futures::future::join_all;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, mpsc, Mutex, Semaphore};
use tracing::info;

static GLOBAL_DONE_TIMEOUT_BLOCK: AtomicU64 = AtomicU64::new(0);
static GLOBAL_DONE_TIMEOUT_COUNT: AtomicU64 = AtomicU64::new(0);
static GLOBAL_ABORTED_DONE_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static GLOBAL_BLOCK_SHOWN: AtomicU64 = AtomicU64::new(0);

fn ms_since_epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn parse_turn_index(messages: &[crate::types::ChatMessage]) -> u32 {
    messages.iter().filter(|m| m.role == crate::types::MessageRole::Tool).count() as u32 + 1
}

/// Serial-mode gate (spec §10.3, `parallel_tool_calls: false`): keep at most
/// one tool call per assistant turn. The client then executes them one at a
/// time; the model re-issues the rest on subsequent turns with history
/// intact.
///
/// Applies to any tool-call list (bridge `ProviderResponse::Success.tool_calls`
/// and native responses alike). In serial mode a batch of N calls is truncated
/// to the first call so the run only ever parks one open tool call at a time.
pub fn truncate_tool_calls(calls: &mut Option<Vec<ToolCall>>, parallel_tool_calls: Option<bool>) {
    if parallel_tool_calls != Some(false) {
        return;
    }
    if let Some(calls) = calls.as_mut() {
        if calls.len() > 1 {
            calls.truncate(1);
        }
    }
}

/// Serial-mode gate on a full native response (spec §10.3).
pub fn truncate_parallel_tool_calls(
    resp: &mut crate::types::ChatCompletionResponse,
    parallel_tool_calls: Option<bool>,
) {
    for choice in resp.choices.iter_mut() {
        truncate_tool_calls(&mut choice.message.tool_calls, parallel_tool_calls);
    }
}

/// Map a provider error to the API error envelope, keeping the spec §7.6
/// error semantics (e.g. bridge isolation failures surface as 503
/// `tool_isolation_unavailable`).
fn provider_error_to_app_error(e: anyhow::Error) -> AppError {
    let msg = e.to_string();
    if msg.contains("tool_isolation_unavailable") {
        AppError::ToolIsolationUnavailable
    } else {
        AppError::Internal(msg)
    }
}

/// FIX-QUOTA-METRIC: detect provider.free-limit / HTTP 429 errors and record
/// them so free-tier exhaustion is observable at /_metrics.
fn is_quota_error(e: &str) -> bool {
    let lower = e.to_lowercase();
    lower.contains("provider.free-limit")
        || lower.contains("free usage limit")
        || lower.contains("http 429")
        || lower.contains("status 429")
        || lower.contains("\"status\":429")
}

/// Transient opencode/bridge errors worth one retry when no meaningful output was emitted.
///
/// FIX-TRANSPORT-CLASSIFY: delegate to the shared `transport` classifier so
/// bridge retries, HTTP upstream retries, and metrics use one taxonomy. All
/// previously matched strings remain retryable; the classifier additionally
/// covers DNS/timeouts/5xx/ECONNRESET/ECONNREFUSED variants.
fn is_transient_bridge_error(e: &str) -> bool {
    crate::transport::classify_message(e).is_retryable()
        || e.to_lowercase().contains("failed to spawn opencode process")
}

/// Whether a stream error should be ignored because the SSE consumer is gone.
///
/// A broken pipe / readiness failure that happens *after* the client hung up
/// must not trigger another CLI spawn (retry or failover): nobody reads the
/// result and the extra child process pushes the cgroup toward TasksMax.
fn skip_retry_when_client_gone(client_gone: bool, error: &str) -> bool {
    if !client_gone { return false; }
    // Any error is skip-worthy once the only consumer has disconnected.
    !error.is_empty()
}

fn is_opencode_quota_failover_candidate(provider_id: &str, model: &str) -> bool {
    let is_opencode = provider_id == "opencode"
        || model.starts_with("opencode/")
        || model.starts_with("agg/opencode/");
    let already_failover = model.contains("9router/free-forever")
        || model.contains("9router/free_forever");
    is_opencode && !already_failover
}

fn record_quota_error_if_needed(e: &str, provider: &str, model: &str) {
    if is_quota_error(e) {
        crate::metrics::init_metrics().record_quota_error(provider, model);
    }
}

/// RAII guard around a provider semaphore permit (spec §10.3).
/// Updates the `agentgate_provider_concurrency` gauge when it is dropped.
pub(crate) struct ProviderPermit {
    _permit: tokio::sync::OwnedSemaphorePermit,
    provider_id: String,
}

// Manual impl: `OwnedSemaphorePermit` has no useful `Debug` surface, and
// `Run` derives `Debug` (bridge.rs), so the permit field must be printable.
impl std::fmt::Debug for ProviderPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderPermit")
            .field("provider_id", &self.provider_id)
            .finish_non_exhaustive()
    }
}

impl Drop for ProviderPermit {
    fn drop(&mut self) {
        crate::metrics::init_metrics().record_provider_concurrency(&self.provider_id, -1);
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuotaDisabledModel {
    pub model: String,
    pub provider_id: String,
    /// FIX-QUOTA-WATCHER: timestamp of the FIRST free.limit notification.
    /// Never overwritten by later notifications while the model stays disabled.
    pub disabled_at_unix: u64,
    pub retry_after_unix: u64,
    pub reason: String,
    /// FIX-QUOTA-WATCHER: timestamp of the most recent notification.
    #[serde(default)]
    pub last_seen_quota_at: u64,
    /// FIX-QUOTA-WATCHER: number of watcher re-checks performed.
    #[serde(default)]
    pub recheck_count: u32,
    /// FIX-QUOTA-WATCHER: origin of the notification (http_429 / provider.free-limit / admin).
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QuotaState {
    #[serde(default)]
    pub disabled: HashMap<String, QuotaDisabledModel>,
}

impl QuotaState {
    fn is_disabled(&self, model_key: &str, now: u64) -> bool {
        self.disabled.get(model_key).map(|e| e.retry_after_unix > now).unwrap_or(false)
    }
    fn disable(&mut self, model_key: String, entry: QuotaDisabledModel) {
        self.disabled.insert(model_key, entry);
    }
    fn remove_expired(&mut self, now: u64) -> Vec<String> {
        let expired: Vec<String> = self.disabled.iter().filter(|(_, v)| v.retry_after_unix <= now).map(|(k, _)| k.clone()).collect();
        for k in &expired { self.disabled.remove(k); }
        expired
    }
}

/// How the router should execute a resolved model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Existing CLI/bridge path (spawns a process).
    Cli,
    /// Direct HTTP `/chat/completions` (never spawns a process).
    Http,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionMode::Cli => "cli",
            ExecutionMode::Http => "http",
        }
    }
}

/// One resolved combo candidate, owned so the forwarding task never borrows
/// `ServiceRegistry`.
enum ComboStreamTarget {
    Http {
        provider: Arc<HttpOpenAiCompatibleProvider>,
        upstream: String,
        sticky: Option<String>,
        request: ChatCompletionRequest,
    },
    Cli {
        provider: Arc<dyn CliProvider + Send + Sync>,
        model: String,
        request: ChatCompletionRequest,
    },
    Missing(String),
}

/// An enabled HTTP upstream plus the routing metadata needed by the catalog.
struct HttpProviderEntry {
    provider: Arc<HttpOpenAiCompatibleProvider>,
    mode: String,
    models: Vec<String>,
}

pub struct ServiceRegistry {
    providers: RwLock<HashMap<String, Arc<dyn CliProvider + Send + Sync>>>,
    native_providers: RwLock<HashMap<String, Arc<dyn CliProvider + Send + Sync>>>,
    /// Direct OpenAI-compatible upstreams. Kept separate from `CliProvider`s so
    /// HTTP mode never spawns a CLI process (spec §5.1).
    http_providers: RwLock<HashMap<String, HttpProviderEntry>>,
    add_providers: RwLock<Vec<AddProviderRequest>>,
    pub bridge: Arc<BridgeServer>,
    pub run_registry: RunRegistry,
    pub routing: RoutingConfig,
    pub bridge_config: BridgeConfig,
    /// Global reasoning configuration (`[reasoning]` in agentgate.toml).
    pub reasoning: ReasoningConfig,
    /// When true, Ollama requests are served by the CLI bridge instead of the
    /// HTTP native proxy (`ollama_mode = "cli"`).
    pub ollama_cli_mode: bool,
    /// Semaphore to limit concurrent runs per provider (spec §10.3).
    pub provider_semaphores: RwLock<HashMap<String, Arc<Semaphore>>>,
    /// Default max concurrency per provider from config.
    pub max_concurrency_per_provider: usize,
    /// Tool injection limits (QUOTA-01)
    pub limits: LimitsConfig,
    /// Deadline for the whole non-streaming request (`[server] request_timeout_secs`).
    /// Applied in the HTTP handler only for non-stream requests so SSE streams
    /// and valid tool loops are never cut (B7).
    pub request_timeout: Duration,
    /// Deadline (seconds) applied to each *turn's* provider call. With
    /// `AGENTGATE_BRIDGE_TIMEOUT_SECS=120`, a final turn that ends in text or a
    /// bridge `done` event was previously never even noticed past 120 s, so a
    /// hung CLI produced "bridge turn timed out" on a perfectly healthy final
    /// turn. Timeout turns still trigger the replay-retry ladder.
    pub turn_timeout: Duration,
    /// Exponential backoff (ms) between timeout-retry attempts.
    pub timeout_retry_backoff_ms: Vec<u64>,
    /// Deadline for the first provider event on a streaming request
    /// (`[defaults] first_token_timeout_secs`, default 120 s).
    pub first_token_timeout: Duration,
    /// Model cache: provider_id -> (models, cached_at)
    model_cache: RwLock<HashMap<String, (Vec<String>, Instant)>>,
    /// Negative cache: model_id -> cached_at (for models not found, to avoid repeated scans)
    /// FIX-MODEL-CACHE: prevents unknown models from triggering full CLI scan on every request.
    model_negative_cache: RwLock<HashMap<String, Instant>>,
    /// Cache TTL for model scans (default 300s).
    model_cache_ttl: Duration,
    /// Quota-disabled model state (quota exhaustion -> disable, watcher re-enables).
    quota_state: Arc<RwLock<QuotaState>>,
    quota_state_path: std::path::PathBuf,
    /// FIX-QUOTA-WATCHER: 12h watcher interval + cooldown (configurable).
    quota_watcher_interval: Duration,
    quota_cooldown: Duration,
}

impl ServiceRegistry {
    /// Create a new ServiceRegistry with the given config sections.
    pub fn with_config(
        routing: RoutingConfig,
        bridge_config: BridgeConfig,
        reasoning: ReasoningConfig,
        defaults: DefaultsConfig,
        ollama_cli_mode: bool,
        limits: LimitsConfig,
    ) -> Self {
        let bridge = Arc::new(BridgeServer::new(bridge_config.clone()));
        let run_registry = RunRegistry::new(bridge.clone(), bridge_config.clone(), crate::permission::ToolActivityPolicy::default());
        let quota_state_path = std::env::var("AGENTGATE_QUOTA_STATE_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let base = std::env::var("XDG_CONFIG_HOME")
                    .map(std::path::PathBuf::from)
                    .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
                    .or_else(|_| std::env::var("USERPROFILE").map(|h| std::path::PathBuf::from(h).join(".config")))
                    .unwrap_or_else(|_| std::path::PathBuf::from("."));
                base.join("agentgate").join("quota_state.json")
            });
        let quota_state = Self::load_quota_state(&quota_state_path);
        Self {
            providers: RwLock::new(HashMap::new()),
            native_providers: RwLock::new(HashMap::new()),
            http_providers: RwLock::new(HashMap::new()),
            add_providers: RwLock::new(Vec::new()),
            bridge,
            run_registry,
            routing,
            bridge_config,
            reasoning,
            ollama_cli_mode,
            provider_semaphores: RwLock::new(HashMap::new()),
            max_concurrency_per_provider: defaults.max_concurrency_per_provider,
            limits,
            first_token_timeout: Duration::from_secs(if defaults.first_token_timeout_secs == 0 {
                120
            } else {
                defaults.first_token_timeout_secs
            }),
            request_timeout: Duration::from_secs(600),
            turn_timeout: Duration::from_secs(120),
            timeout_retry_backoff_ms: vec![50, 100, 200, 400, 800],
            model_cache: RwLock::new(HashMap::new()),
            model_negative_cache: RwLock::new(HashMap::new()),
            model_cache_ttl: Duration::from_secs(if defaults.model_cache_ttl_secs == 0 {
                900
            } else {
                defaults.model_cache_ttl_secs
            }),
            quota_state: Arc::new(RwLock::new(quota_state)),
            quota_state_path,
            quota_watcher_interval: Duration::from_secs(43_200),
            quota_cooldown: Duration::from_secs(43_200),
        }
    }

    /// Create a new ServiceRegistry with default config (for tests/backwards compat).
    pub fn new() -> Self {
        Self::with_config(
            RoutingConfig::default(),
            BridgeConfig::default(),
            ReasoningConfig::default(),
            DefaultsConfig::default(),
            false,
            LimitsConfig::default(),
        )
    }

    fn load_quota_state(path: &std::path::Path) -> QuotaState {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<QuotaState>(&s).ok())
            .unwrap_or_default()
    }

    fn save_quota_state(&self, state: &QuotaState) {
        if let Some(parent) = self.quota_state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(state) {
            let _ = std::fs::write(&self.quota_state_path, s);
        }
    }

    fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn next_utc_day_unix(now: u64) -> u64 {
        ((now / 86_400) + 1) * 86_400
    }

    fn quota_model_key(provider_id: &str, model: &str) -> String {
        let m = model.trim().to_lowercase();
        if m.starts_with("agg/") { m[4..].to_string() } else { format!("{}/{}", provider_id, m.trim_start_matches(&format!("{}/", provider_id))) }
    }

    pub async fn mark_model_quota_disabled(&self, provider_id: &str, model: &str, reason: &str) {
        let now = Self::now_unix();
        let key = Self::quota_model_key(provider_id, model);
        // FIX-QUOTA-WATCHER: cooldown runs from the LAST notification so repeated
        // free.limit hits keep the model disabled, while `disabled_at_unix`
        // preserves the very first notification for auditing.
        let retry_after = now.saturating_add(self.quota_cooldown.as_secs());
        let source = if reason.to_lowercase().contains("429") {
            "http_429"
        } else if reason.to_lowercase().contains("free") {
            "provider.free-limit"
        } else {
            "quota_exhausted"
        };
        let mut st = self.quota_state.write().await;
        let previous = st.disabled.get(&key);
        let first_seen = previous.map(|e| e.disabled_at_unix).unwrap_or(now);
        let recheck_count = previous.map(|e| e.recheck_count).unwrap_or(0);
        let entry = QuotaDisabledModel {
            model: model.to_string(),
            provider_id: provider_id.to_string(),
            disabled_at_unix: first_seen,
            retry_after_unix: retry_after,
            reason: reason.to_string(),
            last_seen_quota_at: now,
            recheck_count,
            source: source.to_string(),
        };
        let is_first = previous.is_none();
        st.disable(key.clone(), entry);
        self.save_quota_state(&st);
        if is_first {
            tracing::warn!(event="quota.model.disabled_first_seen", model=%model, provider_id=%provider_id, first_seen_unix=first_seen, retry_after_unix=retry_after, reason=%reason, source=%source, "Model disabled on first quota notification");
        }
        tracing::warn!(event="quota.model.disabled", model=%model, provider_id=%provider_id, retry_after_unix=retry_after, reason=%reason, "Model disabled due to quota exhaustion");
    }

    pub async fn is_model_quota_disabled(&self, provider_id: &str, model: &str) -> bool {
        let now = Self::now_unix();
        let key = Self::quota_model_key(provider_id, model);
        let st = self.quota_state.read().await;
        st.is_disabled(&key, now)
    }

    /// Free-model candidates for the load balancer.
    ///
    /// Combines models discovered from the provider CLI with the gateway model
    /// catalog (so cross-provider free pools work), excluding the hard
    /// `9router/free-forever` fallback.
    pub async fn free_model_candidates(&self, provider_id: &str) -> Vec<crate::gateway::LbCandidate> {
        use crate::gateway::LbCandidate;
        let models = {
            let providers = self.providers.read().await;
            providers.get(provider_id).map(|p| p.models()).unwrap_or_default()
        };
        let is_free = |model: &str| {
            let ml = model.to_lowercase();
            let fallback = ml.contains("9router/free-forever") || ml.contains("9router/free_forever");
            !fallback && (ml.ends_with("-free") || ml == "free" || ml.contains("free-forever"))
        };
        let mut candidates: Vec<LbCandidate> = models.into_iter()
            .filter(|m| is_free(m))
            .map(|m| LbCandidate::new(m, provider_id))
            .collect();
        let doc = crate::gateway::state().document().await;
        for (id, record) in &doc.models {
            if !record.enabled || candidates.iter().any(|c| c.model == *id) { continue; }
            if !record.free && !is_free(id) { continue; }
            let candidate_provider = record.provider_id.clone().unwrap_or_else(|| provider_id.to_string());
            let mut candidate = LbCandidate::new(id.clone(), candidate_provider);
            candidate.weight = record.weight.max(1);
            candidates.push(candidate);
        }
        candidates.sort_by(|a, b| a.model.cmp(&b.model));
        candidates
    }

    pub async fn pick_available_free_model(&self, provider_id: &str) -> Option<String> {
        self.pick_available_free_model_for(provider_id, None, "quota_disabled", None).await
    }

    /// Filter quota-disabled candidates, then pick via the free-pool LB.
    pub async fn pick_available_free_model_for(
        &self,
        provider_id: &str,
        session_id: Option<&str>,
        reason: &str,
        already_disabled: Option<&str>,
    ) -> Option<String> {
        let candidates = self.free_model_candidates(provider_id).await;
        let now = Self::now_unix();
        let st = self.quota_state.read().await;
        let mut eligible = Vec::new();
        for candidate in candidates {
            if Some(candidate.model.as_str()) == already_disabled { continue; }
            if st.is_disabled(&candidate.key(), now) { continue; }
            eligible.push(candidate);
        }
        drop(st);
        crate::gateway::lb::select(
            crate::gateway::DEFAULT_FREE_POOL,
            &eligible,
            &format!("{}/", provider_id),
            reason,
            session_id,
            None,
        )
        .await
        .map(|selection| selection.model)
    }

    pub async fn reroute_model_for_quota(&self, provider_id: &str, requested_model: &str) -> String {
        self.reroute_model_for_quota_with_session(provider_id, requested_model, None).await
    }

    pub async fn reroute_model_for_quota_with_session(
        &self,
        provider_id: &str,
        requested_model: &str,
        session_id: Option<&str>,
    ) -> String {
        if provider_id != "opencode" {
            return requested_model.to_string();
        }
        let now = Self::now_unix();
        let key = Self::quota_model_key(provider_id, requested_model);
        let disabled = { self.quota_state.read().await.is_disabled(&key, now) };
        let req_lower = requested_model.to_lowercase();
        let is_free = req_lower.ends_with("-free") || req_lower == "free";
        if !disabled && !is_free {
            return requested_model.to_string();
        }
        if disabled || is_free {
            let reason = if disabled { "quota_disabled" } else { "free_policy" };
            if let Some(m) = self
                .pick_available_free_model_for(provider_id, session_id, reason, Some(requested_model))
                .await
            {
                if Self::quota_model_key(provider_id, &m) != key {
                    return m;
                }
            }
            return crate::gateway::lb::fallback_selection(
                crate::gateway::DEFAULT_FREE_POOL,
                requested_model,
                "pool_exhausted",
            )
            .await
            .model;
        }
        requested_model.to_string()
    }

    pub fn start_quota_watcher(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let interval_secs = this.quota_watcher_interval.as_secs();
        tokio::spawn(async move {
            // FIX-QUOTA-WATCHER: default 12h (was 5 min). A disabled model is
            // only reconsidered on this schedule; no quota is burned probing
            // unless `probe_on_recheck` is explicitly enabled elsewhere.
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
            tracing::info!(event="quota.watcher.started", interval_secs=interval_secs, "Quota watcher started");
            loop {
                interval.tick().await;
                let now = Self::now_unix();
                let mut st = this.quota_state.write().await;
                let mut due: Vec<(String, u64, u32)> = Vec::new();
                for (k, e) in st.disabled.iter_mut() {
                    if e.retry_after_unix <= now {
                        e.recheck_count = e.recheck_count.saturating_add(1);
                        due.push((k.clone(), e.disabled_at_unix, e.recheck_count));
                    }
                }
                for (k, first_seen, rechecks) in &due {
                    tracing::info!(event="quota.model.recheck", model=%k, first_seen_unix=first_seen, recheck_count=rechecks, "Re-checking quota-disabled model");
                }
                let expired = st.remove_expired(now);
                if !expired.is_empty() {
                    this.save_quota_state(&st);
                    for m in expired {
                        tracing::info!(event="quota.model.reenabled", model=%m, "Quota-disabled model re-enabled after cooldown");
                    }
                }
            }
        });
    }

    /// Replace the global reasoning configuration.
    pub fn with_reasoning(mut self, reasoning: ReasoningConfig) -> Self {
        self.reasoning = reasoning;
        self
    }

    /// FIX-QUOTA-WATCHER: wire `[quota]` watcher interval and cooldown.
    pub fn with_quota_config(mut self, cfg: &crate::config::QuotaConfig) -> Self {
        if cfg.watcher_interval_secs > 0 {
            self.quota_watcher_interval = Duration::from_secs(cfg.watcher_interval_secs);
        }
        if cfg.cooldown_secs > 0 {
            self.quota_cooldown = Duration::from_secs(cfg.cooldown_secs);
        }
        self
    }

    /// Wire the server-level request timeout (applied to non-stream requests only).
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        if timeout.as_secs() > 0 {
            self.request_timeout = timeout;
        }
        self
    }

    pub fn reasoning_config(&self) -> &ReasoningConfig {
        &self.reasoning
    }

    /// Wire the per-turn timeout applied to each provider call (bridge path).
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        if timeout.as_secs() > 0 {
            self.turn_timeout = timeout;
        }
        self
    }

    /// Wire the exponential backoff ladder (ms) used between bridge-turn retries.
    pub fn with_timeout_retry_backoff_ms(mut self, backoff_ms: Vec<u64>) -> Self {
        if !backoff_ms.is_empty() {
            self.timeout_retry_backoff_ms = backoff_ms;
        }
        self
    }
    
    /// Select CLI bridge for Ollama instead of the HTTP native proxy.
    pub fn with_ollama_cli_mode(mut self, enabled: bool) -> Self {
        self.ollama_cli_mode = enabled;
        self
    }

    /// Set the routing configuration from config file [routing] section.
    pub fn with_routing(mut self, routing: RoutingConfig) -> Self {
        self.routing = routing;
        self
    }

    /// Set the bridge configuration from config file [bridge] section.
    /// Note: this recreates the bridge and run_registry, so should be called before registering providers.
    pub fn with_bridge_config(mut self, bridge_config: BridgeConfig) -> Self {
        self.bridge_config = bridge_config.clone();
        self.bridge = Arc::new(BridgeServer::new(bridge_config.clone()));
        self.run_registry = RunRegistry::new(self.bridge.clone(), bridge_config, crate::permission::ToolActivityPolicy::default());
        self
    }

    /// Resolve model alias (e.g., "gpt-4o" → "agg/opencode/beam-free")
    pub fn resolve_model_alias(&self, model: &str) -> String {
        // Check config first (config keys use simplified names without hyphens/dots)
        let config_key = model.replace('-', "_").replace('.', "_");
        if let Some(mapped) = self.routing.model_aliases.get(&config_key) {
            return mapped.clone();
        }
        // Built-in defaults for GitHub Copilot Chat
        match model {
            "gpt-4o" | "gpt-4o-mini" => "agg/opencode/beam-free".to_string(),
            "gpt-4-turbo" => "agg/opencode/flint-40b-free".to_string(),
            "gpt-3.5-turbo" => "agg/opencode/iceman-free".to_string(),
            "o1-mini" | "o1-preview" => "agg/opencode/deepseek-v4.1-flash-free".to_string(),
            _ => model.to_string(),
        }
    }

    /// Resolve the provider for a model without spawning model-discovery commands.
    pub async fn provider_for_model(&self, model: &str) -> Option<String> {
        // Resolve model alias first (e.g., "gpt-4o" -> "agg/opencode/beam-free")
        let model = self.resolve_model_alias(model);
        // Strip "agg/" gateway namespace prefix if present
        let model = model.strip_prefix("agg/").unwrap_or(&model);
        if model.starts_with("ollama/") {
            return Some("ollama".to_string());
        }
        // HTTP upstreams are checked first: their catalog ids are explicit
        // (`nvidia/meta/...`) and must not be misattributed to a CLI namespace.
        {
            let http = self.http_providers.read().await;
            if let Some((id, _)) = http
                .iter()
                .find(|(_, entry)| entry.models.iter().any(|m| m == model))
            {
                return Some(id.clone());
            }
            if let Some((prefix, _)) = model.split_once('/') {
                if http.contains_key(prefix) {
                    return Some(prefix.to_string());
                }
            }
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

    /// Resolve the provider for a model, including `agg/`-namespaced ids that
    /// were discovered from a provider CLI (e.g. `agg/9router/ag-poli` belongs
    /// to the `opencode` provider even though its namespace is `9router`).
    pub async fn resolve_provider(&self, model: &str) -> Option<String> {
        if let Some(id) = self.provider_for_model(model).await {
            return Some(id);
        }
        // Fall back to the cached model list, which carries the discovering
        // provider for every published id.
        if let Ok(models) = self.list_models(None).await {
            if let Some(m) = models.data.iter().find(|m| m.id == model) {
                return Some(m.provider.clone());
            }
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

    /// Register a native (non-bridge) provider for chat-only requests.
    pub async fn register_native(&mut self, mut provider: Box<dyn CliProvider + Send + Sync>) {
        let id = provider.id();
        let available = provider.check_available().await;
        provider.set_available(available);
        let mut providers = self.native_providers.write().await;
        providers.insert(id, Arc::from(provider));
    }

    /// Register an HTTP upstream provider. Missing keys or malformed URLs are
    /// logged and skipped so one broken provider cannot block server startup.
    /// Returns `true` when the provider was registered.
    pub async fn register_http_provider(&self, config: &ProviderConfig) -> bool {
        if config.provider_type != crate::provider::mappings::HTTP_PROVIDER_TYPE {
            return false;
        }
        if !config.enabled {
            tracing::info!(provider_id = %config.id, "HTTP provider disabled, skipping");
            return false;
        }
        match HttpOpenAiCompatibleProvider::new(config.clone()) {
            Ok(provider) => {
                let entry = HttpProviderEntry {
                    provider: Arc::new(provider),
                    mode: config.mode.clone(),
                    models: config.models.clone(),
                };
                self.http_providers.write().await.insert(config.id.clone(), entry);
                tracing::info!(provider_id = %config.id, mode = %config.mode, "HTTP provider registered");
                true
            }
            Err(error) => {
                tracing::warn!(provider_id = %config.id, error = %error, "HTTP provider registration failed; skipping");
                false
            }
        }
    }

    /// Access a registered HTTP provider (for the data-plane handler).
    pub async fn http_provider(&self, provider_id: &str) -> Option<Arc<HttpOpenAiCompatibleProvider>> {
        self.http_providers
            .read()
            .await
            .get(provider_id)
            .map(|entry| entry.provider.clone())
    }

    /// Resolve a combo from the admin gateway catalog (`/admin/combos`) first,
    /// then from the in-process registry used by tests/programmatic callers.
    pub async fn combo_for_model(&self, id: &str) -> Option<ComboModel> {
        if let Some(record) = crate::gateway::state().get_model(id).await {
            if let Some(combo) = crate::provider::combo::from_model_record(&record) {
                return Some(combo);
            }
        }
        crate::provider::combo::registry().get(id).await
    }

    /// Refresh a provider's model catalog from its upstream `/models` endpoint.
    /// The configured static list is kept when the probe fails.
    pub async fn sync_http_models(&self, provider_id: &str) -> anyhow::Result<usize> {
        let provider = self.http_provider(provider_id).await.ok_or_else(|| {
            anyhow::anyhow!("HTTP provider '{}' is not registered", provider_id)
        })?;
        let models = tokio::time::timeout(Duration::from_secs(10), provider.fetch_models())
            .await
            .map_err(|_| anyhow::anyhow!("model sync timed out for '{}'", provider_id))?
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if let Some(entry) = self.http_providers.write().await.get_mut(provider_id) {
            entry.models = models.clone();
        }
        Ok(models.len())
    }

    /// Map a gateway model id to the upstream model id expected by an HTTP
    /// provider, e.g. `agg/nvidia/meta/llama-3.3-70b-instruct` ->
    /// `meta/llama-3.3-70b-instruct`.
    pub async fn upstream_model_for(&self, model: &str) -> Option<String> {
        let provider_id = self.provider_for_model(model).await?;
        if !self.http_providers.read().await.contains_key(&provider_id) {
            return None;
        }
        let bare = model.strip_prefix("agg/").unwrap_or(model);
        let prefix = format!("{}/", provider_id);
        let upstream = bare.strip_prefix(&prefix).unwrap_or(bare);
        if upstream.is_empty() { None } else { Some(upstream.to_string()) }
    }

    /// Decide `cli` vs `http` execution for a resolved model.
    ///
    /// Rules (spec §5.1): explicit `mode = http` always uses HTTP; `cli` only
    /// uses CLI when a CLI provider exists; `auto` prefers HTTP for chat-only
    /// requests and falls back to CLI when tools are requested and the upstream
    /// cannot forward them. OpenCode CLI and HTTP are never auto-switched: each
    /// provider id keeps its own configured mode.
    pub async fn select_execution_mode(&self, model: &str, has_tools: bool) -> ExecutionMode {
        let provider_id = match self.provider_for_model(model).await {
            Some(id) => id,
            None => return ExecutionMode::Cli,
        };
        let (mode, forward_tools, has_http) = {
            let http = self.http_providers.read().await;
            match http.get(&provider_id) {
                Some(entry) => (entry.mode.clone(), entry.provider.forward_tools(), true),
                None => (String::new(), false, false),
            }
        };
        if !has_http {
            return ExecutionMode::Cli;
        }
        match mode.as_str() {
            "http" => ExecutionMode::Http,
            "cli" => {
                if self.providers.read().await.contains_key(&provider_id) {
                    ExecutionMode::Cli
                } else {
                    ExecutionMode::Http
                }
            }
            _ => {
                // auto mode: prefer HTTP for chat-only; use CLI for tools when
                // the HTTP upstream cannot forward them and a CLI provider exists.
                if has_tools && !forward_tools && self.providers.read().await.contains_key(&provider_id) {
                    ExecutionMode::Cli
                } else {
                    ExecutionMode::Http
                }
            }
        }
    }

    /// Whether a provider is registered **and** reported as available.
    ///
    /// Used to reject requests for dead providers (missing binary, broken
    /// install) before opening an SSE stream that would only send keep-alives.
    /// Both the bridge adapter and the native CLI provider are consulted
    /// because a bridge adapter can report unavailable while the native CLI
    /// works (and vice versa).
    pub async fn is_provider_available(&self, provider_id: &str) -> bool {
        {
            let http = self.http_providers.read().await;
            if http.contains_key(provider_id) {
                return true;
            }
        }
        {
            let native = self.native_providers.read().await;
            if let Some(p) = native.get(provider_id) {
                if p.is_available() {
                    return true;
                }
            }
        }
        match self.providers.read().await.get(provider_id) {
            Some(p) => p.is_available(),
            None => false,
        }
    }

    /// Register every configured provider the way the server does: the bridge
    /// adapter plus the native CLI implementation.
    ///
    /// Every CLI entry point (`serve`, `models`, `test`, `doctor`, `providers`)
    /// must go through this. Registering only the bridge adapter made
    /// `agentgate models` print the static `models` list from the config (10
    /// entries) while the server scanned the real catalog (98), and made
    /// `agentgate doctor` report `opencode MISSING` although the binary was
    /// reachable through `resolve_binary`'s nvm fallback.
    pub async fn register_configured(&mut self, providers: &[ProviderConfig]) {
        for provider_cfg in providers {
            // HTTP upstreams must never be registered as CLI providers: no
            // binary is spawned and no bridge session is created for them.
            if provider_cfg.provider_type == crate::provider::mappings::HTTP_PROVIDER_TYPE {
                self.register_http_provider(provider_cfg).await;
                continue;
            }
            let provider =
                crate::provider::build_cli_provider(provider_cfg, Some(self.bridge.clone()), Some(&self.bridge_config), Some(&self.limits));
            self.register(provider).await;
            let native: Box<dyn CliProvider + Send + Sync> =
                Box::new(crate::provider::CliProviderImpl::new(provider_cfg.clone()));
            self.register_native(native).await;
        }
    }

    pub async fn list_models(&self, _provider_filter: Option<&str>) -> Result<ModelsResponse, AppError> {
        let start = Instant::now();
        
        // Get all providers (bridge + native)
        let bridge_providers = self.providers.read().await;
        let native_providers = self.native_providers.read().await;
        
        // Collect all providers with their IDs. Prefer the native provider for
        // each ID: bridge adapters just echo the configured `models` list, while
        // the native CLI provider actually runs `models_cmd` (`opencode models`,
        // `gemini list`, `ollama list`, …).
        let mut all_providers: Vec<(String, Arc<dyn CliProvider + Send + Sync>)> = Vec::new();
        for (id, provider) in native_providers.iter() {
            all_providers.push((id.clone(), provider.clone()));
        }
        for (id, provider) in bridge_providers.iter() {
            if !all_providers.iter().any(|(pid, _)| pid == id) {
                all_providers.push((id.clone(), provider.clone()));
            }
        }
        drop(bridge_providers);
        drop(native_providers);
        
        // Check cache for all providers
        let mut cached_models = HashMap::new();
        let mut providers_to_scan = Vec::new();
        // Stale-but-previously-known models, kept as fallback when a scan fails.
        // Prevents transient CLI failures from evicting dynamically-discovered
        // models (e.g. 9router/* discovered via `opencode models`) that are not
        // in the static config list.
        let mut stale_models: HashMap<String, Vec<String>> = HashMap::new();
        let cache_ttl = self.model_cache_ttl;
        let now = Instant::now();
        
        {
            let cache = self.model_cache.read().await;
            for (id, provider) in &all_providers {
                if let Some((models, cached_at)) = cache.get(id) {
                    if now.duration_since(*cached_at) < cache_ttl {
                        cached_models.insert(id.clone(), models.clone());
                        // FIX-MODEL-CACHE-02: record cache hit so /_metrics
                        // shows the hit/miss ratio for observability.
                        crate::metrics::init_metrics().record_model_cache_hit(id);
                        continue;
                    }
                    // Expired but known: keep as fallback if scan fails.
                    stale_models.insert(id.clone(), models.clone());
                    // Cache miss (expired) — record it so we can observe
                    // how often TTL expiry forces re-scans.
                    crate::metrics::init_metrics().record_model_cache_miss(id);
                } else {
                    // Never seen this provider before — a miss.
                    crate::metrics::init_metrics().record_model_cache_miss(id);
                }
                providers_to_scan.push((id.clone(), provider.clone()));
            }
        }
        
        // Scan missing/expired providers in parallel. Each scan is bounded so a
        // hung CLI (`opencode models`, `gemini list`, …) cannot stall the whole
        // request past the client's gateway timeout.
        // FIX-MODEL-CACHE-03: skip scanning providers whose previous scan already
        // failed recently (negative cache). This prevents every request for an
        // unknown/missing model from paying another 5 s CLI scan.
        let neg_cache = self.model_negative_cache.read().await;
        providers_to_scan.retain(|(id, _)| {
            match neg_cache.get(id) {
                Some(&cached_at) => now.duration_since(cached_at) < cache_ttl,
                None => true,
            }
        });
        drop(neg_cache);
        let scan_timeout = Duration::from_secs(5);
        let scan_futures: Vec<_> = providers_to_scan.into_iter().map(|(id, provider)| {
            let id_clone = id.clone();
            tokio::spawn(async move {
                let scan_start = Instant::now();
                let result = match tokio::time::timeout(scan_timeout, provider.scan_models()).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!(
                        "scan_models timed out after {}s",
                        scan_timeout.as_secs()
                    )),
                };
                let elapsed = scan_start.elapsed();
                tracing::info!(provider = %id_clone, elapsed_ms = elapsed.as_millis(), "scan_models completed");
                (id_clone, result, elapsed)
            })
        }).collect();
        
        let scan_results = futures::future::join_all(scan_futures).await;
        
        // Update cache with new results
        let mut cache = self.model_cache.write().await;
        for result in scan_results {
            match result {
                Ok((id, Ok(models), elapsed)) => {
                    // FIX-MODEL-CACHE-02: clear negative cache on successful scan.
                    self.model_negative_cache.write().await.remove(&id);
                    cache.insert(id.clone(), (models.clone(), Instant::now()));
                    cached_models.insert(id, models);
                }
                Ok((id, Err(e), elapsed)) => {
                    // FIX-MODEL-CACHE-03: record in negative cache so the next
                    // request does not re-scan until TTL expires (prevents
                    // repeated 5 s CLI scans for unavailable providers).
                    self.model_negative_cache.write().await.insert(id.clone(), now);
                    // On scan failure, keep stale cached models as fallback.
                    // This prevents transient CLI failures from evicting
                    // dynamically-discovered models (e.g. 9router/* via
                    // `opencode models`) that are not in the static config.
                    let provider_available = all_providers
                        .iter()
                        .find(|(pid, _)| pid == &id)
                        .map(|(_, p)| p.is_available())
                        .unwrap_or(false);
                    if provider_available {
                        if let Some(old) = stale_models.get(&id) {
                            tracing::warn!(provider = %id, elapsed_ms = elapsed.as_millis(), error = %e, "scan_models failed, keeping stale cached models");
                            cache.insert(id.clone(), (old.clone(), Instant::now()));
                            cached_models.insert(id, old.clone());
                        } else if let Some((_, provider)) = all_providers.iter().find(|(pid, _)| pid == &id) {
                            tracing::warn!(provider = %id, elapsed_ms = elapsed.as_millis(), error = %e, "scan_models failed, no stale cache; using config models");
                            cache.insert(id.clone(), (provider.models(), Instant::now()));
                            cached_models.insert(id, provider.models());
                        }
                    } else {
                        tracing::warn!(provider = %id, elapsed_ms = elapsed.as_millis(), error = %e, "scan_models failed and provider is unavailable; not advertising its configured models");
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "scan_models task join error");
                }
            }
        }
        
        // Build models response from cached results.
        //
        // Namespacing rules (`agg/` prevents collisions with 9router's own CLI):
        // - `provider/model` (e.g. `opencode/beam-free`, `ollama/llama3`): keep the
        //   upstream namespace as-is → `agg/opencode/beam-free`. If that namespace
        //   is a registered gateway provider, attribute the model to it so the
        //   same model discovered from two providers is not misattributed.
        // - bare `model` → `agg/<discovering provider>/<model>`.
        let mut models = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for (id, model_list) in &cached_models {
            // Never advertise models of a provider that cannot serve them.
            // `scan_models` falls back to the configured list when it finds
            // nothing, so an absent binary (gemini, a mis-installed opencode)
            // used to keep its whole static catalog visible. Ollama over HTTP
            // is exempt: the CLI may be absent while the server is fine.
            let is_ollama_http = !self.ollama_cli_mode && id == "ollama";
            if !is_ollama_http {
                let available = all_providers
                    .iter()
                    .find(|(pid, _)| pid == id)
                    .map(|(_, p)| p.is_available())
                    .unwrap_or(false);
                if !available {
                    tracing::debug!(provider = %id, "list_models: provider unavailable, hiding its models");
                    continue;
                }
            }
            for model_id in model_list {
                let (prefixed_id, owner) = match model_id.split_once('/') {
                    Some((prefix, _)) if all_providers.iter().any(|(pid, _)| pid == prefix) => {
                        (format!("agg/{}", model_id), prefix.to_string())
                    }
                    Some(_) => (format!("agg/{}", model_id), id.clone()),
                    None => (format!("agg/{}/{}", id, model_id), id.clone()),
                };
                if seen_ids.insert(prefixed_id.clone()) {
                    models.push(Model {
                        id: prefixed_id,
                        object: "model".to_string(),
                        created: 0,
                        owned_by: owner.clone(),
                        provider: owner,
                        name: model_id.clone(),
                    });
                }
            }
        }
        
        // Ollama native provider: include models from /api/tags.
        // Skip when ollama_cli_mode is on (the `ollama list` scan above already
        // covers it) and always bound the HTTP probe so a hung Ollama daemon
        // cannot turn /v1/models into a gateway timeout.
        let has_ollama = all_providers.iter().any(|(id, _)| id == "ollama");
        if has_ollama && !self.ollama_cli_mode {
            let cfg = ProviderConfig {
                id: "ollama".to_string(),
                base_url: Some("http://localhost:11434".to_string()),
                ..Default::default()
            };
            let ollama = OllamaNativeProvider::new(cfg);
            match tokio::time::timeout(Duration::from_secs(2), ollama.get_models()).await {
                Ok(Ok(ollama_models)) => {
                    for m in ollama_models {
                        let prefixed = format!("agg/{}", m.name);
                        if seen_ids.insert(prefixed.clone()) {
                            models.push(Model {
                                id: prefixed,
                                object: "model".to_string(),
                                created: 0,
                                owned_by: "ollama".to_string(),
                                provider: "ollama".to_string(),
                                name: m.name.clone(),
                            });
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "ollama /api/tags unavailable, skipping");
                }
                Err(_) => {
                    tracing::warn!("ollama /api/tags timed out after 2s, skipping");
                }
            }
        }

        // Direct HTTP upstreams: advertise their configured/synced catalog as
        // `agg/<provider>/<upstream-model>`. No CLI scan and no network probe
        // here — `/v1/models` must stay fast; use sync_http_models() to refresh.
        {
            let http = self.http_providers.read().await;
            for (provider_id, entry) in http.iter() {
                for model_id in &entry.models {
                    let prefixed = format!("agg/{}/{}", provider_id, model_id);
                    if seen_ids.insert(prefixed.clone()) {
                        models.push(Model {
                            id: prefixed,
                            object: "model".to_string(),
                            created: 0,
                            owned_by: provider_id.clone(),
                            provider: provider_id.clone(),
                            name: model_id.clone(),
                        });
                    }
                }
            }
        }

        // Stable ordering so clients that diff the model list do not see
        // HashMap iteration order churn between requests.
        models.sort_by(|a, b| a.id.cmp(&b.id));

        let cached_count = cached_models.len();
        tracing::info!(total_providers = all_providers.len(), cached = cached_count, total_models = models.len(), elapsed_ms = start.elapsed().as_millis(), "list_models completed");
        
        Ok(ModelsResponse {
            object: "list".to_string(),
            data: models,
        })
    }

    /// Warm the model-scan cache in the background at startup.
    ///
    /// The first `/v1/models` call used to pay the full CLI scan cost
    /// (`opencode models`, `gemini list`, `ollama list`, …), which could exceed
    /// a client's gateway timeout and surface as a 504 on
    /// `POST /api/providers/fetch-models`. Calling this once after providers are
    /// registered makes the first real request a cache hit.
    pub async fn warm_model_cache(&self) {
        let start = Instant::now();
        match self.list_models(None).await {
            Ok(resp) => {
                tracing::info!(
                    models = resp.data.len(),
                    elapsed_ms = start.elapsed().as_millis(),
                    "model cache warmed"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "model cache warm-up failed");
            }
        }
    }

    /// Pre-populate the model cache from static config models (agentgate.toml
    /// [[provider]] models = [...]). This eliminates the initial CLI scan for
    /// known models, reducing first-request latency from ~seconds to ~zero.
    /// Unknown models remain subject to per-TTL scanning (handled by list_models).
    ///
    /// FIX-MODEL-CACHE-01: reduces hot-path scan time from 5 s to zero for
    /// pre-registered models. Unknown models still fall back to scanning but
    /// results are cached positive/negative for model_cache_ttl_secs.
    pub async fn pre_populate_cache(&self, providers: &[crate::config::ProviderConfig]) {
        let mut cache = self.model_cache.write().await;
        for pc in providers {
            if !pc.enabled {
                continue;
            }
            let provider_id = &pc.id;
            // Seed cache with configured models so the first request is a hit.
            let now = Instant::now();
            cache.insert(provider_id.clone(), (pc.models.clone(), now));
            tracing::debug!(
                provider = %provider_id,
                count = pc.models.len(),
                "pre-populated model cache from config"
            );
        }
        drop(cache);
        tracing::info!(count = providers.len(), "model cache pre-populated from config");
    }

    pub async fn list_providers(&self) -> Result<Vec<crate::types::Provider>, AppError> {
        let providers = self.providers.read().await;
        let result: Vec<crate::types::Provider> = futures::future::join_all(
            providers.iter().map(|(id, provider)| {
                let id = id.clone();
                let provider = provider.clone();
                async move {
                    let raw_models = provider.scan_models().await.unwrap_or_else(|_| provider.models());
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
                }
            })
        ).await;
        Ok(result)
    }

    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, AppError> {
        self.chat_completion_with_depth(request, 0).await
    }

    async fn chat_completion_with_depth(
        &self,
        mut request: ChatCompletionRequest,
        depth: u8,
    ) -> Result<ChatCompletionResponse, AppError> {
        const MAX_COMBO_DEPTH: u8 = 8;

        // Combo (virtual) model: try candidates in order. Recursion is bounded
        // and the registry prevents cycles at creation time.
        if depth < MAX_COMBO_DEPTH {
            if let Some(combo) = self
                .combo_for_model(&request.model)
                .await
                .filter(|combo| combo.enabled)
            {
                for (attempt, candidate) in ComboRegistry::candidates(&combo).iter().enumerate() {
                    let mut candidate_request = request.clone();
                    candidate_request.model = candidate.model.clone();
                    if let Some(provider) = candidate.provider.as_deref() {
                        candidate_request.provider = Some(provider.to_string());
                    }
                    ComboRegistry::record_attempt(&combo.id, &candidate.model, attempt);
                    match Box::pin(self.chat_completion_with_depth(candidate_request, depth + 1)).await {
                        Ok(response) => return Ok(response),
                        Err(error) => {
                            let failure = ComboFailure::from_message(error.to_string());
                            if should_fallback(&combo.fallback_on, &failure) {
                                ComboRegistry::record_fallback(&combo.id, error.code());
                                continue;
                            }
                            return Err(error);
                        }
                    }
                }
                return Err(AppError::Internal(format!(
                    "combo '{}' exhausted all candidates",
                    combo.id
                )));
            }
        }

        // Keep the published (possibly `agg/`-namespaced) id for provider
        // resolution, then strip the gateway namespace for the CLI invocation.
        let requested_model = request.model.clone();
        if let Some(stripped) = request.model.strip_prefix("agg/") {
            request.model = stripped.to_string();
        }
        if request.n.unwrap_or(1) > 1 {
            return Err(AppError::ToolError("n>1 is not supported".to_string()));
        }
        if let Some(tools) = &request.tools {
            // Fail-closed limits (spec §14, [bridge.limits]).
            let limits = &self.bridge_config.limits;
            // FIX-QUOTA-04: Skip max_tools check for free models to avoid rejecting requests
            // before they reach the provider. Free models have their own quota limits.
            // FIX-QUOTA-FREE: align with provider.rs / opencode.rs suffix match
            // so paid models that merely contain "free" are not misclassified.
            let model_lower = request.model.to_lowercase();
            let is_free_model = model_lower.ends_with("-free") || model_lower == "free";
            if !is_free_model && tools.len() > limits.max_tools {
                return Err(AppError::ToolError(format!(
                    "too_many_tools ({} > {})",
                    tools.len(),
                    limits.max_tools
                )));
            }
            for t in tools {
                let schema_bytes = t.function
                    .parameters
                    .as_ref()
                    .and_then(|p| serde_json::to_vec(p).ok())
                    .map(|v| v.len())
                    .unwrap_or(0);
                if schema_bytes > limits.max_schema_bytes {
                    return Err(AppError::ToolError(format!(
                        "tool schema too large ({} > {} bytes)",
                        schema_bytes, limits.max_schema_bytes
                    )));
                }
                if t.r#type != "function" {
                    return Err(AppError::ToolError("unsupported tool type".to_string()));
                }
            }
        }
        info!("ChatCompletion START: model='{}', provider_opt={:?}, messages={}", request.model, request.provider, request.messages.len());

        // Keep tool role as-is for OpenAI tool-calling compatibility (spec_tooling §7.1)
        let _ = &mut request.messages;
        let provider_id = if let Some(id) = request.provider.as_ref() {
            info!("Using explicit provider: {}", id);
            id.clone()
        } else if let Some(id) = self.resolve_provider(&requested_model).await {
            info!("Resolved provider via resolve_provider: {}", id);
            id
        } else {
            info!("Provider not found for model='{}'", requested_model);
            return Err(AppError::ModelNotFound(requested_model));
        };

        let is_native = !self.ollama_cli_mode
            && (provider_id == "ollama" || request.model.starts_with("ollama/"));
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
            let mut resp = ollama.forward_chat(&model, &request).await.map_err(|e| AppError::Internal(e.to_string()))?;
            truncate_parallel_tool_calls(&mut resp, request.parallel_tool_calls);
            return Ok(resp);
        }

        // Direct HTTP OpenAI-compatible upstream (no CLI process, no bridge).
        // Resolution happens before this point via `resolve_provider`, which
        // now understands HTTP model ids. Choose the explicit execution mode
        // rather than silently switching a CLI provider to HTTP.
        if self
            .select_execution_mode(&requested_model, request.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false))
            .await
            == ExecutionMode::Http
        {
            if let Some(provider) = self.http_provider(&provider_id).await {
                let upstream_model = self
                    .upstream_model_for(&requested_model)
                    .await
                    .unwrap_or_else(|| request.model.clone());
                let mut resp = provider
                    .chat(&upstream_model, &request, request.session_id.as_deref())
                    .await
                    .map_err(|e| AppError::Internal(e.to_string()))?;
                truncate_parallel_tool_calls(&mut resp, request.parallel_tool_calls);
                return Ok(resp);
            }
        }

        // Fail-closed (spec §7.6 error table + spec §13.1): during the bridge
        // flow, client-sent `tools[]` are only honoured when the policy allows
        // and the resolved provider is bridge-capable.
        //   - `on_client_tools = "reject"` -> HTTP 501 `tool_bridge_unsupported`
        //   - provider tidak bridge-capable     -> HTTP 501 `tool_bridge_unsupported`
        // Native (ollama) path already returned above; here we are in bridge mode.
        if request.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false) && !is_native {
            let provider_bridge_ok = self
                .providers
                .read()
                .await
                .get(&provider_id)
                .map(|p| p.is_bridge())
                .unwrap_or(false);
            if self.routing.on_client_tools == crate::permission::ClientToolsPolicy::Reject || !provider_bridge_ok {
                return Err(AppError::ToolBridgeUnsupported);
            }
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
                info!(event = "run.resumed", provider = %provider_id);
                self.resume_run(&provider_id, call_ids, request).await
            }
            crate::run::Classified::Replay { history } => {
                tracing::warn!(event = "replay_fallback", provider = %provider_id, reason = "unknown_tool_call_id");
                request.agentgate_multi_turn = false;
                self.replay_run(&provider_id, history, request).await
            }
            crate::run::Classified::NewBridge { tools } => {
                let turn_index = parse_turn_index(&request.messages);
                let t0 = Instant::now();
                let gateway_deadline = Some(tokio::time::Instant::now() + self.request_timeout);
                self.new_bridge_run(&provider_id, tools, request, Some(turn_index), t0, gateway_deadline).await
            }
            crate::run::Classified::NewChatOnly => {
                request.agentgate_multi_turn = false;
                self.chat_only_run(&provider_id, request).await
            }
        }
    }

    async fn call_provider_native(&self, provider_id: &str, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).ok_or(AppError::ProviderNotFound(provider_id.to_string()))?.clone();
        drop(providers);
        self.call_provider_native_with(provider, request).await
    }

    async fn call_provider_native_with(&self, provider: Arc<dyn CliProvider + Send + Sync>, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        let mut model = request.model.clone();
        let provider_id = provider.id().to_string();
        let timeout_secs = provider_call_timeout_secs();
        let gateway_deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        tracing::info!(model, provider_id = %provider_id, timeout_secs, "call_provider_native_with: calling send_chat_completion");
        let _ = &gateway_deadline;
        // FIX-QUOTA-RETRY: retry only quota/free-limit failures with bounded
        // exponential backoff (1s, 2s, 4s). Other errors still fail fast.
        const MAX_QUOTA_RETRIES: u32 = 3;
        let mut attempt: u32 = 0;
        let mut request = request;
        let mut failed_over_to_9router = false;
        let response = loop {
            let call = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                provider.send_chat_completion(&model, request.clone()),
            )
            .await;
            match call {
                Ok(Ok(crate::provider::ProviderResponse::Error(ref e)))
                    if is_quota_error(e) && !failed_over_to_9router && is_opencode_quota_failover_candidate(&provider_id, &model) =>
                {
                    failed_over_to_9router = true;
                    let from_model = model.clone();
                    self.mark_model_quota_disabled(&provider_id, &from_model, "quota_exhausted").await;
                    crate::gateway::lb::cooldown_after_quota(crate::gateway::DEFAULT_FREE_POOL, &from_model, &provider_id).await;
                    // Prefer another healthy free model; hard fallback only when
                    // the pool is exhausted (spec §13.2).
                    model = self
                        .pick_available_free_model_for(&provider_id, request.session_id.as_deref(), "quota_exhausted", Some(&from_model))
                        .await
                        .unwrap_or_else(|| "opencode/9router/free-forever".to_string());
                    request.model = model.clone();
                    request.agentgate_force_fresh_cli_session = true;
                    if let Some(sid) = request.session_id.clone() {
                        crate::provider::clear_cli_session_for_chain(&sid).await;
                    }
                    tracing::warn!(
                        provider_id = %provider_id,
                        from_model = %from_model,
                        to_model = %model,
                        "quota/free-limit on opencode model; failing over to 9router/free-forever"
                    );
                    crate::metrics::init_metrics().record_quota_error(&provider_id, &from_model);
                    continue;
                }
                Ok(Ok(crate::provider::ProviderResponse::Error(ref e)))
                    if is_quota_error(e) && attempt < MAX_QUOTA_RETRIES =>
                {
                    attempt += 1;
                    let delay = Duration::from_secs(1u64 << (attempt - 1));
                    tracing::warn!(
                        model, provider_id = %provider_id, attempt,
                        delay_ms = delay.as_millis(),
                        "quota error, retrying with backoff"
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Ok(result) => {
                // If a native provider signals the tail was processed and more
                // turns remain, reprocess them before returning to the caller
                // so no logical turn is skipped.
                if request.agentgate_native_tail_reprocess {
                    if let Ok(crate::provider::ProviderResponse::Success {
                        ref content,
                        ref tool_calls,
                        ..
                    }) = result {
                        info!(event = "native.tail_reprocess", provider_id = %provider_id,
                             model = %model, has_tools = tool_calls.as_ref().is_some_and(|t| !t.is_empty()),
                             content_len = content.len());
                        let mut reprocessed_request = request.clone();
                        reprocessed_request.messages.push(crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: Some(content.clone()),
                            reasoning_content: None,
                            tool_calls: tool_calls.clone(),
                            tool_call_id: None,
                            name: None,
                        });
                        if let Some(ids) = &tool_calls {
                            if !ids.is_empty() {
                                for id in ids.iter().map(|c| c.id.clone()) {
                                    reprocessed_request.messages.push(crate::types::ChatMessage {
                                        role: crate::types::MessageRole::Tool,
                                        content: Some(String::new()),
                                        reasoning_content: None,
                                        tool_calls: None,
                                        tool_call_id: Some(id),
                                        name: None,
                                    });
                                }
                            }
                        }
                        reprocessed_request.agentgate_native_tail_reprocess = false;
                        request = reprocessed_request;
                        continue;
                    }
                }
                break result.map_err(provider_error_to_app_error)?
            }
                Err(_) => {
                    crate::metrics::init_metrics().record_cli_exit(false);
                    tracing::error!(model, provider_id = %provider_id, timeout_secs, "call_provider_native_with: provider call timed out");
                    return Err(AppError::ProviderError(format!(
                        "provider '{}' produced no result within {}s (timed out)",
                        provider_id, timeout_secs
                    )));
                }
            }
        };
        tracing::info!(model, provider_id = %provider_id, "call_provider_native_with: got response");
        match response {
            crate::provider::ProviderResponse::Success { content: text, reasoning, tool_calls, session_id, .. } => {
                crate::metrics::init_metrics().record_cli_exit(true);
                crate::metrics::init_metrics().record_provider_call(&provider_id, &model, true);
                let has_tool_calls = tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                let finish_reason = if has_tool_calls { "tool_calls" } else { "stop" };
                Ok(ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    model,
                    choices: vec![crate::types::Choice {
                        index: 0,
                        message: crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: if text.is_empty() { None } else { Some(text) },
                            reasoning_content: reasoning,
                            tool_calls: tool_calls,
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: Some(finish_reason.to_string()),
                    }],
                    usage: Some(crate::types::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    }),
                    session_id,
                })
            }
            crate::provider::ProviderResponse::Error(e) => {
                crate::metrics::init_metrics().record_cli_exit(false);
                crate::metrics::init_metrics().record_provider_call(&provider_id, &model, false);
                record_quota_error_if_needed(&e, &provider_id, &model);
                Err(AppError::ProviderError(e))
            }
        }
    }

    async fn resume_run(&self, provider_id: &str, call_ids: Vec<String>, mut request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Extract tool results from trailing tool messages of the request
        let trailing = crate::run::trailing_tool_messages(&request.messages);
        let tool_results: Vec<(String, String)> = trailing.iter()
            .filter_map(|m| m.tool_call_id.clone().map(|id| (id, m.content.clone().unwrap_or_default())))
            .collect();

        // Find parked run for any call_ids (PR-S1 linkage + PR-S2 liveness).
        // PR-S1 fix: lookup and consume are atomic under one guard, so no
        // concurrent resume/cleanup can consume the same parked run between
        // find and remove.
        let (run_id, session_key) = {
            let mut parked_runs = self.bridge.parked_runs().lock().await;
            // PR-S1 fix: find + consume atomically under one guard, so no
            // concurrent resume/cleanup can consume the same parked run between
            // find and remove.
            let consumed = call_ids.iter().find_map(|id| {
                parked_runs.iter().find(|(_, run)| {
                    run.pending_tool_call_ids.iter().any(|p| p == id)
                        || run.messages.iter().any(|msg| {
                            msg.tool_call_id.as_ref() == Some(id)
                                || msg.tool_calls.as_ref().is_some_and(|tcs| tcs.iter().any(|tc| &tc.id == id))
                        })
                }).map(|(k, run)| (
                    k.clone(),
                    run.mcp_session_id.clone().or_else(|| run.provider_session_key.clone()),
                ))
            });
            match consumed {
                Some((key, skey)) => {
                    parked_runs.remove(&key);
                    (Some(key), skey)
                }
                None => (None, None),
            }
        };
        let session_lost_reason = if run_id.is_some() {
            if let Some(ref key) = session_key {
                if self.bridge.has_provider_session(key).await { None } else { Some("parked_session_lost") }
            } else {
                None // unknown linkage: keep current fallback behavior
            }
        } else {
            Some("parked_run_not_found")
        };

        if let (Some(run_id), None) = (run_id.as_ref(), session_lost_reason) {
            // Parked run found and no known session loss.
            for (id, content) in &tool_results {
                let res = crate::bridge::ToolResult {
                    tool_call_id: id.clone(),
                    name: "resumed_tool".to_string(),
                    content: serde_json::json!(content),
                    is_error: false,
                };
                let resumed = self.run_registry.resume_run(&run_id, res.clone()).await;
                if resumed.is_err() {
                    // PR-S1: race with cleanup/expiry between the parked-run
                    // lookup and resume. The replay fallback still runs below,
                    // so this is observability only.
                    tracing::warn!(event = "resume.run_missing", run_id = %run_id, "parked run not found at resume time");
                }
                // PR-S4: deliver real results to held MCP tools/call (unblocks live CLI continuation).
                let _ = self.bridge.resolve_tool_result_by_id(id, res).await;
            }
            // PR-S4: capability-gated live continuation (opencode). Await continuation if registered;
            // replay fallback mandatory on absence/timeout/error/session loss.
            let mut replay_reason = "continuation_not_available";
            let mut has_live_session = self.bridge.has_provider_session(&run_id).await;
            if let Some(ref k) = session_key {
                if self.bridge.has_provider_session(k).await { has_live_session = true; }
            }
            if has_live_session {
                let cont = match self.bridge.take_continuation(&run_id).await {
                    Some(c) => Some(c),
                    None => match session_key.as_deref() {
                        Some(k) => self.bridge.take_continuation(k).await,
                        None => None,
                    },
                };
                if let Some(cont) = cont {
                    // Budget the continuation wait against the caller's HTTP
                    // deadline as well as the bridge turn timeout, so a
                    // healthy-but-slow final answer isn't cut by the gateway
                    // while a real stall still triggers replay fallback.
                    let gateway_now = tokio::time::Instant::now();
                    let gateway_deadline: Option<tokio::time::Instant> = Some(gateway_now + self.request_timeout);
                    let effective_timeout = if request.agentgate_multi_turn {
                        let from_gateway = gateway_deadline.map(|d| {
                            if d <= gateway_now { std::time::Duration::ZERO } else { d.saturating_duration_since(gateway_now) }
                        }).unwrap_or_else(crate::provider::bridge_turn_timeout);
                        if from_gateway < crate::provider::bridge_turn_timeout() { from_gateway } else { crate::provider::bridge_turn_timeout() }
                    } else {
                        let full = crate::provider::bridge_turn_timeout();
                        gateway_deadline.map(|d| {
                            if d <= gateway_now { full } else { d.saturating_duration_since(gateway_now).max(full) }
                        }).unwrap_or(full)
                    };
                    let turn_index = parse_turn_index(&request.messages);
                    let turn_t0 = Instant::now();
                    info!(event = "resume.continuation_timeout_config", provider_id = %provider_id,
                         run_id = %run_id, turn_index, timeout_ms = effective_timeout.as_millis(),
                         multi_turn = request.agentgate_multi_turn);
                    let retry_guard = Duration::from_secs(1);
                    match tokio::time::timeout(effective_timeout + retry_guard, cont.rx).await {
                        Ok(Ok(out)) => {
                            tracing::info!(event = "resume.live_continuation.ok", provider = %provider_id, run_id = %run_id);
                            let has_tool_calls = out.tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                            let finish_reason = if has_tool_calls { "tool_calls" } else { "stop" };
                            // PR-S5: best-effort cleanup when continuation completes without further tool calls.
                            if !has_tool_calls {
                                self.bridge.remove_continuation(&run_id).await;
                                if let Some(ref k) = session_key { self.bridge.unregister_provider_session(k).await; }
                                self.bridge.unregister_provider_session(&run_id).await;
                                // Remove from run registries best-effort.
                                let _ = self.bridge.runs().write().await.remove(run_id);
                            }
                            return Ok(ChatCompletionResponse {
                                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                                object: "chat.completion".to_string(),
                                created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                                model: request.model.clone(),
                                choices: vec![crate::types::Choice {
                                    index: 0,
                                    message: crate::types::ChatMessage {
                                        role: crate::types::MessageRole::Assistant,
                                        content: if out.content.is_empty() { None } else { Some(out.content) },
                                        reasoning_content: out.reasoning,
                                        tool_calls: out.tool_calls,
                                        tool_call_id: None,
                                        name: None,
                                    },
                                    finish_reason: Some(finish_reason.to_string()),
                                }],
                                usage: Some(crate::types::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 }),
                                session_id: out.session_id,
                            });
                        }
                        Ok(Err(_)) => {
                            replay_reason = "continuation_channel_closed";
                            if let Some(ref k) = session_key { self.bridge.unregister_provider_session(k).await; }
                            self.bridge.unregister_provider_session(&run_id).await;
                        }
                        Err(_) => {
                            replay_reason = "continuation_timeout";
                            if let Some(ref k) = session_key { self.bridge.unregister_provider_session(k).await; }
                            self.bridge.unregister_provider_session(&run_id).await;
                        }
                    }
                } else {
                    tracing::info!(event = "resume.live_continuation.pending", provider = %provider_id, run_id = %run_id, session_key = ?session_key);
                }
            }
            let mut tools = request.tools.take().unwrap_or_default();
            // Preserve parked-run tool schemas for replay/re-invocation when client omits tools.
            if tools.is_empty() {
                if let Some(run) = self.run_registry.get_run(&run_id).await {
                    tools = run.tools.into_iter().map(|t| crate::types::ToolDefinition {
                        r#type: "function".to_string(),
                        function: crate::types::FunctionDefinition {
                            name: t.name,
                            description: if t.description.is_empty() { None } else { Some(t.description) },
                            parameters: Some(t.parameters),
                        },
                    }).collect();
                }
            }
            tracing::warn!(event = "replay_fallback", provider = %provider_id, run_id = ?run_id, session_key = ?session_key, reason = %replay_reason);
            // PR-S5: rotate session on replay fallback (avoid stale CLI/persistent session reuse).
            request.session_id = None;
            request.agentgate_run_id = None;
            let turn_index = parse_turn_index(&request.messages);
            let turn_t0 = std::time::Instant::now();
            let replay_deadline = Some(tokio::time::Instant::now() + self.request_timeout);
            self.new_bridge_run(provider_id, tools, request, Some(turn_index), turn_t0, replay_deadline).await
        } else {
            let reason = session_lost_reason.unwrap_or("parked_session_lost");
            // Best-effort: release any MCP holds for these tool_call_ids to avoid leaks on replay.
            for (id, _) in &tool_results {
                let _ = self.bridge.resolve_by_openai_tool_call_id(id).await;
            }
            tracing::warn!(event = "replay_fallback", provider = %provider_id, run_id = ?run_id, session_key = ?session_key, reason = %reason);
            // PR-S5: rotate session on replay fallback.
            request.session_id = None;
            request.agentgate_run_id = None;
            let turn_index = parse_turn_index(&request.messages);
            let turn_t0 = std::time::Instant::now();
            let replay_deadline = Some(tokio::time::Instant::now() + self.request_timeout);
            let tools = request.tools.take().unwrap_or_default();
            self.new_bridge_run(provider_id, tools, request, Some(turn_index), turn_t0, replay_deadline).await
        }
    }

    async fn replay_run(&self, provider_id: &str, history: Vec<crate::types::Message>, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Flatten history and create new run (spec §10.5 replay fallback).
        // Session rotation (cli_gen++/new CLI session) is provider-dependent; full replay
        // is performed by reissuing the request with flattened history.
        tracing::warn!(event = "replay_fallback", provider = %provider_id, reason = "explicit_replay");
        let mut new_request = request.clone();
        // `history` already is the full conversation (classify() returns
        // request.messages.clone()). Extending would append it a second time and
        // double the prompt on every replay.
        new_request.messages = history;
        // PR-S5: force fresh session/CLI context on replay.
        new_request.session_id = None;
        new_request.agentgate_run_id = None;
        let turn_index = parse_turn_index(&new_request.messages);
        let t0 = Instant::now();
        let gateway_deadline = Some(tokio::time::Instant::now() + self.request_timeout);
        let tools = new_request.tools.take().unwrap_or_default();
        self.new_bridge_run(provider_id, tools, new_request, Some(turn_index), t0, gateway_deadline).await
    }

    async fn new_bridge_run(&self, provider_id: &str, tools: Vec<crate::types::ToolDefinition>, mut request: ChatCompletionRequest, turn_index: Option<u32>, turn_t0: std::time::Instant, gateway_deadline: Option<tokio::time::Instant>) -> Result<ChatCompletionResponse, AppError> {
        // Acquire provider semaphore to limit concurrent runs per provider (spec §10.3)
        let permit = self.acquire_provider_permit(provider_id).await?;

        // Quota-aware model routing: disabled-quota models reroute to other free models, else 9router/free-forever.
        if provider_id == "opencode" {
            let routed = self
                .reroute_model_for_quota_with_session(provider_id, &request.model, request.session_id.as_deref())
                .await;
            if routed != request.model {
                let sid = request.session_id.clone().unwrap_or_default();
                if !sid.is_empty() {
                    crate::provider::clear_cli_session_for_chain(&sid).await;
                }
                request.agentgate_force_fresh_cli_session = true;
                request.model = routed;
            }
        }

        // Create new run with bridge (best-effort; do not block on failure)
        let tool_schemas: Vec<crate::bridge::ToolSchema> = tools.iter().map(|t| crate::bridge::ToolSchema {
            name: t.function.name.clone(),
            description: t.function.description.clone().unwrap_or_default(),
            parameters: t.function.parameters.clone().unwrap_or_default(),
            provider: provider_id.to_string(),
            workspace: "default".to_string(),
        }).collect();
        let run = match self.run_registry.create_run(
            "default".to_string(),
            request.model.clone(),
            request.messages.clone(),
            tool_schemas,
            crate::openai::ToolChoice::from_value(request.tool_choice.clone()).unwrap_or_default(),
        ).await {
            Ok(run) => Some(run),
            Err(e) => {
                // FIX-RUN-VISIBILITY: do not silently swallow run creation errors —
                // they are the reason follow-ups fall back to Replay.
                tracing::warn!(error = %e, provider_id, "new_bridge_run: failed to create Run; tool resume will be unavailable");
                crate::metrics::init_metrics().record_error("run", "create_failed");
                None
            }
        };

        // Compute the effective timeout for this provider call. Bridge CLI agents
        // that emit no tool calls should complete within a bounded turn window;
        // budget the caller's time instead of polling until the HTTP timeout.
        let effective_timeout: Duration = {
            let full = crate::provider::bridge_turn_timeout();
            let from_gateway = gateway_deadline
                .map(|d| d.saturating_duration_since(tokio::time::Instant::now()))
                .unwrap_or(full);
            if request.tools.as_ref().is_some_and(|t| !t.is_empty()) && !request.agentgate_multi_turn {
                // Turn-1 with tools is supposed to park in this session; poll
                // until CLI exit (after turn timeout). Poll interval keeps the
                // first tool-call event surfacing early.
                full
            } else {
                // Replayed/chain/resumed contexts, or tool-free final turns:
                // the CLI must emit text/done promptly after turn timeout.
                if from_gateway < full { from_gateway } else { full }
            }
        };
        info!(event = "bridge.call_timeout", provider_id = %provider_id, model = %request.model,
             turn_index, timeout_ms = effective_timeout.as_millis(), tools = request.tools.as_ref().is_some_and(|t| !t.is_empty()),
             multi_turn = request.agentgate_multi_turn);

        // Spawn bridge process and get the single response (carries tool_calls)
        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).ok_or(AppError::ProviderNotFound(provider_id.to_string()))?;
        tracing::info!(provider_id = %provider_id, provider_type = %std::any::type_name_of_val(&*provider), is_bridge = %provider.is_bridge(), "new_bridge_run: calling provider");
        let mut model = request.model.clone();
        let mut req_for_provider = request.clone();
        req_for_provider.agentgate_run_id = run.as_ref().map(|r| r.id.clone());
        // FIX-QUOTA-RETRY: bounded exponential backoff for quota/free-limit
        // responses on the bridge path too (native path is handled above).
        const MAX_QUOTA_RETRIES: u32 = 3;
        let mut quota_attempt: u32 = 0;
        let mut failed_over_to_9router = false;
        let mut effective_timeout = effective_timeout;
        let mut response: Result<crate::provider::ProviderResponse, anyhow::Error>;
        loop {
            // Wall-clock guard for the provider call: polling a hung bridge
            // child can keep a healthy final turn blocked past the configured
            // bridge timeout. Retry ladder stays available for real timeouts.
            if quota_attempt > 0 {
                let now = tokio::time::Instant::now();
                let call_budget = effective_timeout + self.turn_timeout;
                match tokio::time::timeout(call_budget, provider.send_chat_completion(&model, req_for_provider.clone()))
                    .await
                {
                    Ok(res) => response = res,
                    Err(_) => {
                        info!(event = "bridge.timeout.retry_guard_fired", provider_id = %provider_id, model = %model,
                             turn_index, attempt = quota_attempt, timeout_ms = call_budget.as_millis());
                        if let Some(mut r) = run.clone() {
                            r.fail(format!("provider_error: bridge turn timed out (retry guard, attempt={})", quota_attempt + 1));
                            let _ = self.bridge.runs().write().await.insert(r.id.clone(), r);
                        }
                        if quota_attempt >= MAX_QUOTA_RETRIES {
                            return Err(AppError::ProviderError(format!(
                                "provider_error: bridge turn timed out after {} attempts (retry guard)",
                                quota_attempt + 1
                            )));
                        }
                        let backoff_ms = self.timeout_retry_backoff_ms
                            .get((quota_attempt as usize).saturating_sub(1))
                            .copied()
                            .unwrap_or(400);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                }
            } else {
                match tokio::time::timeout(effective_timeout + self.turn_timeout, provider.send_chat_completion(&model, req_for_provider.clone())).await {
                    Ok(res) => response = res,
                    Err(_) => {
                        info!(event = "bridge.timeout.retry_guard_fired", provider_id = %provider_id, model = %model,
                             turn_index, attempt = 1, timeout_ms = (effective_timeout + self.turn_timeout).as_millis());
                        if let Some(mut r) = run.clone() {
                            r.fail("provider_error: bridge turn timed out (retry guard, attempt=1)".to_string());
                            let _ = self.bridge.runs().write().await.insert(r.id.clone(), r);
                        }
                        quota_attempt = 1;
                        let backoff_ms = self.timeout_retry_backoff_ms
                            .get(0)
                            .copied()
                            .unwrap_or(400);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                }
            }
            if turn_t0.elapsed() > effective_timeout {
                // The provider call succeeded but the polling budget was crossed
                // while it ran; the caller is still owed the result, so report
                // success rather than "timed out" for a healthy final turn.
                tracing::warn!(event = "bridge.timeout.budget_crossed_after_ok", provider_id = %provider_id,
                               model = %model, turn_index, elapsed_ms = turn_t0.elapsed().as_millis(),
                               timeout_ms = effective_timeout.as_millis(), "Provider succeeded after polling budget; reporting success");
                effective_timeout = effective_timeout.saturating_add(self.turn_timeout);
            }
            // Failover opencode quota/limit errors to 9router/free-forever once.
            let quota_err = match &response {
                Ok(crate::provider::ProviderResponse::Error(e)) if is_quota_error(e) => Some(e.clone()),
                Err(e) if is_quota_error(&e.to_string()) => Some(e.to_string()),
                _ => None,
            };
            if let Some(qe) = quota_err {
                if !failed_over_to_9router && is_opencode_quota_failover_candidate(provider_id, &model) {
                    failed_over_to_9router = true;
                    let from_model = model.clone();
                    self.mark_model_quota_disabled(provider_id, &from_model, "quota_exhausted").await;
                    crate::gateway::lb::cooldown_after_quota(crate::gateway::DEFAULT_FREE_POOL, &from_model, provider_id).await;
                    model = self
                        .pick_available_free_model_for(provider_id, req_for_provider.session_id.as_deref(), "quota_exhausted", Some(&from_model))
                        .await
                        .unwrap_or_else(|| "opencode/9router/free-forever".to_string());
                    req_for_provider.model = model.clone();
                    req_for_provider.agentgate_force_fresh_cli_session = true;
                    if let Some(sid) = req_for_provider.session_id.clone() {
                        crate::provider::clear_cli_session_for_chain(&sid).await;
                    }
                    tracing::warn!(
                        provider_id = %provider_id,
                        from_model = %from_model,
                        to_model = %model,
                        "quota/free-limit on opencode bridge model; failing over to 9router/free-forever"
                    );
                    crate::metrics::init_metrics().record_quota_error(provider_id, &from_model);
                    continue;
                }
            }
            break;
        }

        match response {
            Ok(crate::provider::ProviderResponse::Success { content: text, reasoning, mut tool_calls, session_id, mcp_session_id, .. }) => {
                // CLI-provided session id for the next turn (auto-resume chain).
                let next_cli_sid = session_id.clone().filter(|s| !s.is_empty());
                let has_tool_calls = tool_calls.as_ref().map(|c| !c.is_empty()).unwrap_or(false);
                if let Some(sid) = next_cli_sid.clone() {
                    if has_tool_calls {
                        crate::provider::register_pending_cli_session(&req_for_provider.session_id.clone().unwrap_or_default(), Some(sid.clone())).await;
                        info!(event = "bridge.cli_session.captured", provider_id = %provider_id,
                             cli_session_id = %sid, "CLI session captured for next turn (tool calls pending)");
                    } else {
                        crate::provider::register_pending_cli_session(&req_for_provider.session_id.clone().unwrap_or_default(), Some(sid.clone())).await;
                        crate::provider::mark_bridge_cli_session_active(&req_for_provider.session_id.clone().unwrap_or_default()).await;
                        info!(event = "bridge.cli_session.committed", provider_id = %provider_id,
                             cli_session_id = %sid, "CLI session committed for next turn (final turn)");
                    }
                }
                crate::metrics::init_metrics().record_cli_exit(true);
                crate::metrics::init_metrics().record_provider_call(provider_id, &request.model, true);
                let has_tool_calls = tool_calls.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                let finish_reason = if has_tool_calls { "tool_calls" } else { "stop" };

                let run_id_string = run.as_ref().map(|r| r.id.clone()).unwrap_or_default();

                // P0-AUDIT seq2: Replace truncate_tool_calls with batch/queue remaining.
                // Serial-mode gate: instead of truncating to 1 tool call, keep first call
                // active and queue remaining calls for sequential processing on resume.
                let parallel = request.parallel_tool_calls == Some(false);
                let mut batched_tool_calls = tool_calls.clone();
                if parallel {
                    if let Some(ref mut calls) = batched_tool_calls {
                        if calls.len() > 1 {
                            let remaining = calls.split_off(1);
                            self.bridge.queue_tool_calls(&run_id_string, remaining).await
                                .map_err(|e| AppError::Internal(e.to_string()))?;
                        }
                    }
                }

                let tool_calls = batched_tool_calls;
                // Serial-mode: only the first tool call is returned as pending.
                let has_tool_calls = tool_calls.as_ref().map(|c| !c.is_empty()).unwrap_or(false);
                let final_tool_calls_for_response = tool_calls.clone();

                // Persist the assistant turn (including tool_calls) so a follow-up
                // request carrying role:"tool" can be classified as Resume.
                if let Some(mut run) = run {
                    let assistant_msg = crate::types::ChatMessage {
                        role: crate::types::MessageRole::Assistant,
                        content: if text.is_empty() { None } else { Some(text.clone()) },
                        reasoning_content: reasoning.clone(),
                        tool_calls: tool_calls.clone(),
                        tool_call_id: None,
                        name: None,
                    };
                    run.messages.push(assistant_msg);
                    if has_tool_calls {
                        if let Some(calls) = tool_calls.as_ref() {
                            if calls.len() > 1 {
                                // Parallel batch issued in a single assistant turn
                                // (spec §10.3) — structured event with batch size.
                                info!(event = "batch.issued", batch_size = calls.len());
                            }
                        }
                        if let Some(calls) = tool_calls.as_ref() {
                            if let Some(first) = calls.first() {
                                run.park(first.id.clone(), first.function.name.clone());
                                // PR-S4: linkage for continuation/replay routing (capability-gated).
                                run.mcp_session_id = mcp_session_id.clone();
                                run.provider_session_key = mcp_session_id.clone().or(session_id.clone());
                                run.pending_tool_call_ids = calls.iter().map(|c| c.id.clone()).collect();
                                run.continuation_capable = mcp_session_id.is_some();
                                // Hold the provider semaphore permit while parked
                                // (config `parked_holds_permit`); released on resume/reap.
                                run.permit = if self.bridge_config.parked_holds_permit {
                                    Some(permit)
                                } else {
                                    None
                                };
                                let _ = self.bridge.park_run(run.clone()).await;
                            }
                        }
                    } else {
                        run.complete(Vec::new());
                    }
                    self.run_registry.runs().write().await.insert(run.id.clone(), run);
                }

                Ok(ChatCompletionResponse {
                    id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                    object: "chat.completion".to_string(),
                    created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    model,
                    choices: vec![crate::types::Choice {
                        index: 0,
                        message: crate::types::ChatMessage {
                            role: crate::types::MessageRole::Assistant,
                            content: if text.is_empty() { None } else { Some(text) },
                            reasoning_content: reasoning,
                            tool_calls: tool_calls,
                            tool_call_id: None,
                            name: None,
                        },
                        finish_reason: Some(finish_reason.to_string()),
                    }],
                    usage: Some(crate::types::Usage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    }),
                    session_id,
                })
            }
            Ok(crate::provider::ProviderResponse::Error(e)) => {
                crate::metrics::init_metrics().record_cli_exit(false);
                crate::metrics::init_metrics().record_provider_call(provider_id, &request.model, false);
                record_quota_error_if_needed(&e, provider_id, &request.model);
                // Mark the run Failed so the sweeper reaps it after call_ttl.
                if let Some(mut r) = run {
                    r.fail(e.clone());
                    self.bridge.runs().write().await.insert(r.id.clone(), r);
                }
                Err(AppError::ProviderError(e))
            }
            Err(e) => {
                crate::metrics::init_metrics().record_cli_exit(false);
                crate::metrics::init_metrics().record_provider_call(provider_id, &request.model, false);
                let msg = e.to_string();
                record_quota_error_if_needed(&msg, provider_id, &request.model);
                if let Some(mut r) = run {
                    r.fail(msg.clone());
                    self.bridge.runs().write().await.insert(r.id.clone(), r);
                }
                Err(AppError::ProviderError(msg))
            }
        }
    }

    /// Acquire a permit from the provider semaphore to limit concurrent runs (spec §10.3).
    /// Returns an owned permit that can be held for the duration of the provider turn.
    /// Semaphore size is clamped to >= 1: a 0-sized semaphore would deadlock every caller.
    ///
    /// Acquisition retries with exponential backoff instead of failing on the
    /// first timeout: when the per-provider limit is reached, requests that
    /// would succeed seconds later (after a permit frees up) are aborted by an
    /// immediate timeout. The overall bound stays as a last-resort guard
    /// against permit leaks or starvation.
    async fn acquire_provider_permit(&self, provider_id: &str) -> Result<Arc<ProviderPermit>, AppError> {
        let limit = self.max_concurrency_per_provider.max(1);
        let semaphore = {
            let mut semaphores = self.provider_semaphores.write().await;
            semaphores
                .entry(provider_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(limit)))
                .clone()
        };
        const OVERALL_DEADLINE: Duration = Duration::from_secs(30);
        const PER_TRY_TIMEOUT: Duration = Duration::from_millis(500);
        const FIRST_RETRY_DELAY: Duration = Duration::from_millis(200);
        const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);
        let start = Instant::now();
        let mut retry_delay = FIRST_RETRY_DELAY;
        loop {
            match tokio::time::timeout(PER_TRY_TIMEOUT, semaphore.clone().acquire_owned()).await {
                Ok(Ok(permit)) => {
                    let metrics = crate::metrics::init_metrics();
                    metrics.observe_semaphore_wait(start.elapsed().as_millis() as u64);
                    metrics.record_provider_concurrency(provider_id, 1);
                    return Ok(Arc::new(ProviderPermit {
                        _permit: permit,
                        provider_id: provider_id.to_string(),
                    }));
                }
                Ok(Err(_)) => {
                    return Err(AppError::Internal("Provider semaphore closed".to_string()));
                }
                Err(_) => {
                    let elapsed = start.elapsed();
                    if elapsed >= OVERALL_DEADLINE {
                        return Err(AppError::Internal(format!(
                            "Provider semaphore acquisition timed out (provider={}, limit={})",
                            provider_id, limit
                        )));
                    }
                    let sleep_for = retry_delay.min(OVERALL_DEADLINE - elapsed);
                    tokio::time::sleep(sleep_for).await;
                    retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
                }
            }
        }
    }

    async fn chat_only_run(&self, provider_id: &str, request: ChatCompletionRequest) -> Result<ChatCompletionResponse, AppError> {
        // Chat-only requests (no client tools) use the native CLI provider
        // directly (`opencode run`, `gemini generate`, `ollama run`, …). The
        // bridge is only needed when client tools must be isolated/executed, so
        // avoiding it here removes the MCP session spawn/wait from the hot path.
        let native_providers = self.native_providers.read().await;
        tracing::info!(provider_id, native_count = native_providers.len(), "chat_only_run: checking native providers");
        if let Some(provider) = native_providers.get(provider_id).cloned() {
            drop(native_providers);
            tracing::info!(provider_id, "chat_only_run: using native CLI provider");
            return self.call_provider_native_with(provider, request).await;
        }
        drop(native_providers);

        tracing::info!(provider_id, "chat_only_run: no native provider, falling back to bridge");
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
    ) -> (mpsc::Receiver<RunEvent>, Vec<String>) {
        self.stream_run_events_with_depth(provider_id, request, 0).await
    }

    /// Combo streaming: forward each candidate's events, but only advance the
    /// chain when the candidate failed *before* meaningful output was emitted.
    async fn stream_combo_with_depth(
        &self,
        combo: ComboModel,
        request: ChatCompletionRequest,
        _depth: u8,
    ) -> (mpsc::Receiver<RunEvent>, Vec<String>) {
        let (tx, rx) = mpsc::channel(128);

        // Resolve all targets up front so the spawned task owns only Arcs and
        // owned data. This keeps the future `Send` and avoids recursion.
        let mut targets = Vec::new();
        for candidate in ComboRegistry::candidates(&combo) {
            let mut candidate_request = request.clone();
            candidate_request.model = candidate.model.clone();
            if let Some(provider) = candidate.provider.as_deref() {
                candidate_request.provider = Some(provider.to_string());
            }
            let provider_id = if let Some(id) = candidate.provider.as_deref() {
                Some(id.to_string())
            } else {
                self.provider_for_model(&candidate.model).await
            };
            let target = match provider_id {
                Some(id) => {
                    if let Some(http) = self.http_provider(&id).await {
                        let prefix = format!("{}/", id);
                        let upstream = candidate
                            .model
                            .strip_prefix(&prefix)
                            .unwrap_or(&candidate.model)
                            .to_string();
                        ComboStreamTarget::Http {
                            provider: http,
                            upstream,
                            sticky: candidate_request.session_id.clone(),
                            request: candidate_request,
                        }
                    } else {
                        match self.providers.read().await.get(&id).cloned() {
                            Some(provider) => ComboStreamTarget::Cli {
                                provider,
                                model: candidate.model.clone(),
                                request: candidate_request,
                            },
                            None => ComboStreamTarget::Missing(format!("provider not found: {}", id)),
                        }
                    }
                }
                None => ComboStreamTarget::Missing(format!(
                    "combo candidate provider not found: {}",
                    candidate.model
                )),
            };
            targets.push((candidate.model.clone(), target));
        }

        tokio::spawn(async move {
            let mut had_output = false;
            for (attempt, (candidate_model, target)) in targets.into_iter().enumerate() {
                ComboRegistry::record_attempt(&combo.id, &candidate_model, attempt);
                let (inner_tx, mut inner_rx) = mpsc::channel::<RunEvent>(256);
                let inner_tx_clone = inner_tx.clone();
                match target {
                    ComboStreamTarget::Http { provider, upstream, sticky, request } => {
                        if let Err(error) = provider
                            .stream_events(&upstream, &request, inner_tx_clone, sticky.as_deref())
                            .await
                        {
                            let _ = inner_tx.send(RunEvent::Error {
                                message: error.to_string(),
                                code: Some("provider_error".to_string()),
                            }).await;
                        }
                    }
                    ComboStreamTarget::Cli { provider, model, request } => {
                        if let Err(error) = provider
                            .send_chat_completion_stream(&model, request, inner_tx_clone)
                            .await
                        {
                            let _ = inner_tx.send(RunEvent::Error {
                                message: error.to_string(),
                                code: Some("provider_error".to_string()),
                            }).await;
                        }
                    }
                    ComboStreamTarget::Missing(message) => {
                        let _ = inner_tx.send(RunEvent::Error {
                            message,
                            code: Some("provider_not_found".to_string()),
                        }).await;
                    }
                }
                // Release our sender so `recv()` can observe stream completion.
                drop(inner_tx);

                let mut advance = false;
                while let Some(event) = inner_rx.recv().await {
                    match event {
                        RunEvent::Error { message, code } => {
                            if !had_output {
                                let failure = ComboFailure::from_message(message.clone());
                                if should_fallback(&combo.fallback_on, &failure) {
                                    ComboRegistry::record_fallback(
                                        &combo.id,
                                        code.as_deref().unwrap_or("error"),
                                    );
                                    advance = true;
                                    break;
                                }
                            }
                            let _ = tx.send(RunEvent::Error { message, code }).await;
                            return;
                        }
                        RunEvent::Done { .. } => {
                            let _ = tx.send(event).await;
                            return;
                        }
                        other => {
                            if matches!(
                                other,
                                RunEvent::Text { .. }
                                    | RunEvent::ToolCalls { .. }
                                    | RunEvent::Usage { .. }
                                    | RunEvent::ReasoningDelta { .. }
                            ) {
                                had_output = true;
                            }
                            let _ = tx.send(other).await;
                        }
                    }
                }
                if !advance {
                    return;
                }
            }
            let _ = tx
                .send(RunEvent::Error {
                    message: format!("combo '{}' exhausted all candidates", combo.id),
                    code: Some("combo_exhausted".to_string()),
                })
                .await;
        });
        (rx, Vec::new())
    }

    async fn stream_run_events_with_depth(
        &self,
        provider_id: &str,
        request: ChatCompletionRequest,
        depth: u8,
    ) -> (mpsc::Receiver<RunEvent>, Vec<String>) {
        // Virtual combo models are resolved before any provider lookup.
        if depth < 8 {
            if let Some(combo) = self
                .combo_for_model(&request.model)
                .await
                .filter(|combo| combo.enabled)
            {
                return Box::pin(self.stream_combo_with_depth(combo, request, depth)).await;
            }
        }

        info!("stream_run_events START: provider='{}', model='{}'", provider_id, request.model);
        let (tx, rx) = mpsc::channel(128);
        let mut session_tool_names: Vec<String> = Vec::new();

        // Strip "agg/" gateway namespace prefix from model id
        let mut request = request;
        if let Some(stripped) = request.model.strip_prefix("agg/") {
            request.model = stripped.to_string();
        }

        // Direct HTTP upstream: stream SSE into RunEvents without a CLI/bridge
        // session. Sticky key is the AgentGate session id when available.
        if self
            .select_execution_mode(&request.model, request.tools.as_ref().map(|t| !t.is_empty()).unwrap_or(false))
            .await
            == ExecutionMode::Http
        {
            if let Some(provider) = self.http_provider(provider_id).await {
                let prefix = format!("{}/", provider_id);
                let upstream = request
                    .model
                    .strip_prefix(&prefix)
                    .unwrap_or(&request.model)
                    .to_string();
                let req = request.clone();
                let sticky = req.session_id.clone();
                tokio::spawn(async move {
                    let result = provider
                        .stream_events(&upstream, &req, tx.clone(), sticky.as_deref())
                        .await;
                    if let Err(err) = result {
                        let _ = tx
                            .send(RunEvent::Error {
                                message: err.to_string(),
                                code: Some("provider_error".to_string()),
                            })
                            .await;
                    }
                });
                return (rx, Vec::new());
            }
        }

        let providers = self.providers.read().await;
        let provider = providers.get(provider_id).cloned();
        drop(providers);

        // Chat-only requests (no client tools) should stream from the native
        // CLI provider, mirroring `chat_only_run`. Without this, streaming a
        // tool-less opencode/gemini/claude turn went through the bridge adapter
        // (which spawns an MCP session and waits for it) and could hang.
        let has_client_tools = request
            .tools
            .as_ref()
            .map(|tools| !tools.is_empty())
            .unwrap_or(false);
        let provider = if has_client_tools {
            provider
        } else {
            let native = self.native_providers.read().await.get(provider_id).cloned();
            native.or(provider)
        };

        let is_ollama = provider_id == "ollama" || request.model.starts_with("ollama/");
        // When ollama_cli_mode=true, use bridge provider (OllamaCliProvider) instead of HTTP native
        let use_ollama_cli = self.ollama_cli_mode && is_ollama;
        
        if use_ollama_cli {
            // Use bridge provider which has OllamaCliProvider registered
            if let Some(provider) = provider {
                let model = request.model.clone();
                let req = request.clone();
                tokio::spawn(async move {
                    if let Err(err) = provider.send_chat_completion_stream(&model, req, tx.clone()).await {
                        tracing::error!("ollama CLI stream error: {}", err);
                        let _ = tx
                            .send(RunEvent::Error {
                                message: err.to_string(),
                                code: Some("provider_error".to_string()),
                            })
                            .await;
                    } else {
                        tracing::info!("ollama CLI stream DONE: model='{}'", model);
                    }
                });
            } else {
                let _ = tx
                    .send(RunEvent::Error {
                        message: "ollama provider not found in bridge".to_string(),
                        code: Some("provider_not_found".to_string()),
                    })
                    .await;
            }
        } else if is_ollama {
            // HTTP native fallback
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
            let mut req = request.clone();

            // FIX-STREAM-RUN: the stream path never created/parked a Run, so a
            // tool-result follow-up was always classified as Replay (full
            // transcript re-render + fresh CLI spawn per tool round). Mirror
            // the non-stream lifecycle: create a Run, thread its id into the
            // provider, then park it when tool_calls are emitted.
            let run = if has_client_tools {
                let tool_schemas: Vec<crate::bridge::ToolSchema> = req
                    .tools
                    .as_ref()
                    .map(|tools| {
                        tools
                            .iter()
                            .map(|t| crate::bridge::ToolSchema {
                                name: t.function.name.clone(),
                                description: t.function.description.clone().unwrap_or_default(),
                                parameters: t.function.parameters.clone().unwrap_or_default(),
                                provider: provider_id.to_string(),
                                workspace: "default".to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                session_tool_names = tool_schemas.iter().map(|t| t.name.clone()).collect();
                for t in crate::bridge::agentgate_builtin_tools() {
                    if !session_tool_names.iter().any(|n| n.eq_ignore_ascii_case(&t.name)) {
                        session_tool_names.push(t.name);
                    }
                }
                let created = self
                    .run_registry
                    .create_run(
                        "default".to_string(),
                        model.clone(),
                        req.messages.clone(),
                        tool_schemas,
                        crate::openai::ToolChoice::from_value(req.tool_choice.clone())
                            .unwrap_or_default(),
                    )
                    .await
                    .map_err(|e| {
                        tracing::warn!(error = %e, provider_id, "stream_run_events: failed to create Run; tool resume unavailable");
                        crate::metrics::init_metrics().record_error("run", "create_failed");
                    })
                    .ok();
                if let Some(r) = &created {
                    req.agentgate_run_id = Some(r.id.clone());
                }
                created
            } else {
                None
            };
            let bridge_for_park = self.bridge.clone();

            let provider_id_owned = provider_id.to_string();
            let reroute_free_models = self.bridge_config.reroute_free_models;
            let quota_state_clone = self.quota_state.clone();
            let quota_state_path_clone = self.quota_state_path.clone();
            let opencode_free_models: Vec<String> = if provider_id_owned == "opencode" {
                let providers = self.providers.read().await;
                providers.get(&provider_id_owned).map(|p| p.models()).unwrap_or_default()
            } else { Vec::new() };
            tokio::spawn(async move {
                use std::sync::atomic::{AtomicBool, Ordering};
                let had_meaningful_output = Arc::new(AtomicBool::new(false));
                let mut failed_over_to_9router = false;
                let mut transient_retries: u32 = 0;
                let mut model = model;
                let mut req = req;
                let mut run_for_park = run;
                let provider_id = provider_id_owned;
                if provider_id == "opencode" {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                    let model_key = {
                        let ml = model.trim().to_lowercase();
                        if ml.starts_with("agg/") { ml[4..].to_string() } else { ml.clone() }
                    };
                    let disabled = { quota_state_clone.read().await.is_disabled(&model_key, now) };
                    let ml = model.to_lowercase();
                    let is_free = ml.ends_with("-free") || ml == "free";
                    if disabled || (reroute_free_models && is_free && !ml.contains("9router/free-forever") && !ml.contains("9router/free_forever")) {
                        let reason = if disabled { "quota_disabled" } else { "free_policy" };
                        // FIX-LB: use the quota-aware free pool instead of a
                        // fixed alphabetical first pick.
                        let mut candidates: Vec<crate::gateway::LbCandidate> = opencode_free_models.iter()
                            .filter(|m| {
                                let l = m.to_lowercase();
                                (l.ends_with("-free") || l.contains("free-forever")) && !l.contains("9router/free-forever") && !l.contains("9router/free_forever")
                            })
                            .map(|m| crate::gateway::LbCandidate::new(m.clone(), provider_id.clone()))
                            .collect();
                        candidates.sort_by(|a, b| a.model.cmp(&b.model));
                        let mut eligible = Vec::new();
                        {
                            let st = quota_state_clone.read().await;
                            for candidate in candidates {
                                if st.is_disabled(&candidate.key(), now) || candidate.model.eq_ignore_ascii_case(&model) { continue; }
                                eligible.push(candidate);
                            }
                        }
                        let to = match crate::gateway::lb::select(
                            crate::gateway::DEFAULT_FREE_POOL,
                            &eligible,
                            &model,
                            reason,
                            req.session_id.as_deref(),
                            None,
                        ).await {
                            Some(selection) => selection.model,
                            None => crate::gateway::lb::fallback_selection(
                                crate::gateway::DEFAULT_FREE_POOL, &model, "pool_exhausted",
                            ).await.model,
                        };
                        if to.to_lowercase() != ml {
                            let from = model.clone();
                            model = to;
                            req.model = model.clone();
                            req.agentgate_force_fresh_cli_session = true;
                            if let Some(sid) = req.session_id.clone() {
                                crate::provider::clear_cli_session_for_chain(&sid).await;
                            }
                            tracing::info!(event="quota.model.reroute", provider_id=%provider_id, from_model=%from, to_model=%model, reason=if disabled {"quota_disabled"} else {"free_policy"}, "Rerouted model");
                        }
                    }
                }
                loop {
                    // Proxy the event channel so we can park the Run at the exact
                    // moment tool_calls are emitted — BEFORE the chunk is forwarded
                    // to the client. The SSE handler terminates a tool turn as soon
                    // as it sees tool_calls, so parking after the stream finishes is
                    // too late: the client's tool-result request arrives first and
                    // is classified as Replay.
                    let (inner_tx, mut inner_rx) = mpsc::channel::<RunEvent>(128);
                    let outer_tx = tx.clone();
                    let run_for_park_this = run_for_park.take();
                    let bridge_for_park_clone = bridge_for_park.clone();
                    let had_output = had_meaningful_output.clone();
                    let failover_capture: Arc<tokio::sync::Mutex<Option<String>>> = Arc::new(tokio::sync::Mutex::new(None));
                    let failover_capture_fw = failover_capture.clone();
                    let can_failover = !failed_over_to_9router
                        && !had_meaningful_output.load(Ordering::Relaxed)
                        && is_opencode_quota_failover_candidate(&provider_id, &model);
                    let can_transient_retry = transient_retries < 1
                        && !had_meaningful_output.load(Ordering::Relaxed);
                    let forwarder = tokio::spawn(async move {
                        let mut run_for_park = run_for_park_this;
                        let bridge_for_park = bridge_for_park_clone;
                        let mut suppressed_retry_error: Option<String> = None;
                        loop {
                            tokio::select! {
                                maybe_event = inner_rx.recv() => {
                                    let Some(event) = maybe_event else { break };
                                    match &event {
                                        RunEvent::Text { .. } | RunEvent::ReasoningDelta { .. } | RunEvent::ToolCalls { .. } => {
                                            had_output.store(true, Ordering::Relaxed);
                                        }
                                        RunEvent::Error { message, .. } => {
                                            if !had_output.load(Ordering::Relaxed) {
                                                if can_failover && is_quota_error(message) {
                                                    suppressed_retry_error = Some(message.clone());
                                                    continue; // don't forward; let caller failover
                                                }
                                                if can_transient_retry && is_transient_bridge_error(message) {
                                                    suppressed_retry_error = Some(message.clone());
                                                    continue; // don't forward; let caller retry
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                    if let RunEvent::ToolCalls { calls } = &event {
                                        if let Some(mut r) = run_for_park.take() {
                                            r.messages.push(crate::types::ChatMessage {
                                                role: crate::types::MessageRole::Assistant,
                                                content: None,
                                                reasoning_content: None,
                                                tool_calls: Some(calls.clone()),
                                                tool_call_id: None,
                                                name: None,
                                            });
                                            r.park(calls[0].id.clone(), calls[0].function.name.clone());
                                            r.pending_tool_call_ids = calls.iter().map(|c| c.id.clone()).collect();
                                            bridge_for_park.runs().write().await.insert(r.id.clone(), r);
                                            tracing::info!(event = "run.parked.from_stream", calls = calls.len());
                                        }
                                    }
                                    if outer_tx.send(event).await.is_err() {
                                        break;
                                    }
                                }
                                // Client disconnected: drop inner_rx so the provider
                                // sees its channel close and kills the CLI child.
                                _ = outer_tx.closed() => break,
                            }
                        }
                        if let Some(msg) = suppressed_retry_error {
                            *failover_capture_fw.lock().await = Some(msg);
                            // Return run so caller can retry/failover; do not mark complete.
                            return run_for_park;
                        }
                        // FIX-RUN-COMPLETE: a stream Run without tool_calls must be
                        // marked terminal, otherwise it stays Running forever and
                        // blocks max_runs_per_key for every later request.
                        if let Some(mut r) = run_for_park {
                            let final_messages = r.messages.clone();
                            r.complete(final_messages);
                            bridge_for_park.runs().write().await.insert(r.id.clone(), r);
                        }
                        None
                    });

                    let stream_result = provider.send_chat_completion_stream(&model, req.clone(), inner_tx).await;
                    let returned_run = forwarder.await.ok().flatten();
                    let captured_failover = failover_capture.lock().await.clone();

                    // Prefer explicit error event captured by forwarder; fall back to Err.
                    let captured_or_err = if let Some(msg) = captured_failover.clone() {
                        Some(msg)
                    } else if let Err(ref err) = stream_result {
                        Some(err.to_string())
                    } else {
                        None
                    };

                    if let Some(err_str) = captured_or_err {
                        // FIX-CLIENT-DISCONNECT: when the SSE receiver is gone,
                        // the client already hung up. Retrying or failing over
                        // here would spawn yet another CLI child nobody reads,
                        // inflating Tasks until the cgroup limit is hit (which
                        // in turn causes more broken pipes/readiness failures).
                        if skip_retry_when_client_gone(tx.is_closed(), &err_str) {
                            tracing::info!(
                                target: "agentgate.transport",
                                provider_id = %provider_id,
                                model = %model,
                                error_class = crate::transport::classify_message(&err_str).as_str(),
                                "client disconnected; skipping retry/failover"
                            );
                            crate::metrics::init_metrics().record_error("bridge_transport_client_gone", "skip");
                            break;
                        }
                        let quota = is_quota_error(&err_str)
                            && !failed_over_to_9router
                            && !had_meaningful_output.load(Ordering::Relaxed)
                            && is_opencode_quota_failover_candidate(&provider_id, &model);
                        if quota {
                            failed_over_to_9router = true;
                            let from_model = model.clone();
                            // Mark model as quota-disabled until next UTC day
                            {
                                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                                let retry_after = ((now / 86_400) + 1) * 86_400;
                                let key = { let ml = from_model.trim().to_lowercase(); if ml.starts_with("agg/") { ml[4..].to_string() } else { ml.clone() } };
                                let mut st = quota_state_clone.write().await;
                                let previous = st.disabled.get(&key).cloned();
                                st.disable(key.clone(), QuotaDisabledModel {
                                    model: from_model.clone(),
                                    provider_id: provider_id.clone(),
                                    disabled_at_unix: previous.as_ref().map(|e| e.disabled_at_unix).unwrap_or(now),
                                    retry_after_unix: retry_after,
                                    reason: "quota_exhausted".to_string(),
                                    last_seen_quota_at: now,
                                    recheck_count: previous.as_ref().map(|e| e.recheck_count).unwrap_or(0),
                                    source: "quota_exhausted".to_string(),
                                });
                                if let Some(parent) = quota_state_path_clone.parent() { let _ = std::fs::create_dir_all(parent); }
                                if let Ok(s) = serde_json::to_string_pretty(&*st) { let _ = std::fs::write(&quota_state_path_clone, s); }
                                tracing::warn!(event="quota.model.disabled", model=%from_model, provider_id=%provider_id, retry_after_unix=retry_after, "Model disabled due to quota exhaustion");
                            }
                            // FIX-LB: after disabling the exhausted model, try
                            // the remaining free pool before the hard fallback.
                            crate::gateway::lb::cooldown_after_quota(crate::gateway::DEFAULT_FREE_POOL, &from_model, &provider_id).await;
                            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                            let mut failover_candidates: Vec<crate::gateway::LbCandidate> = opencode_free_models.iter()
                                .filter(|m| !m.eq_ignore_ascii_case(&from_model))
                                .map(|m| crate::gateway::LbCandidate::new(m.clone(), provider_id.clone()))
                                .collect();
                            let mut eligible = Vec::new();
                            {
                                let st = quota_state_clone.read().await;
                                for candidate in failover_candidates.drain(..) {
                                    if !st.is_disabled(&candidate.key(), now) { eligible.push(candidate); }
                                }
                            }
                            model = match crate::gateway::lb::select(
                                crate::gateway::DEFAULT_FREE_POOL,
                                &eligible,
                                &from_model,
                                "quota_exhausted",
                                req.session_id.as_deref(),
                                None,
                            ).await {
                                Some(selection) => selection.model,
                                None => crate::gateway::lb::fallback_selection(
                                    crate::gateway::DEFAULT_FREE_POOL, &from_model, "pool_exhausted",
                                ).await.model,
                            };
                            req.model = model.clone();
                            req.agentgate_force_fresh_cli_session = true;
                            if let Some(sid) = req.session_id.clone() {
                                crate::provider::clear_cli_session_for_chain(&sid).await;
                            }
                            tracing::warn!(
                                provider_id = %provider_id,
                                from_model = %from_model,
                                to_model = %model,
                                "quota/free-limit on opencode stream; rerouted via free pool"
                            );
                            crate::metrics::init_metrics().record_quota_error(&provider_id, &from_model);
                            run_for_park = returned_run;
                            had_meaningful_output.store(false, Ordering::Relaxed);
                            continue;
                        }
                        if transient_retries < 1
                            && !had_meaningful_output.load(Ordering::Relaxed)
                            && is_transient_bridge_error(&err_str)
                        {
                            transient_retries += 1;
                            // FIX-TRANSPORT-CLASSIFY: structured diagnostics for
                            // ECONNREFUSED / ECONNRESET / broken pipe / readiness.
                            let class = crate::transport::classify_message(&err_str);
                            crate::metrics::init_metrics().record_error("bridge_transport_retry", class.as_str());
                            tracing::warn!(
                                target: "agentgate.transport",
                                provider_id = %provider_id,
                                model = %model,
                                error_class = class.as_str(),
                                attempt = transient_retries,
                                error = %crate::transport::redact_diagnostics(&err_str),
                                "transient bridge/CLI error with no output; retrying once"
                            );
                            run_for_park = returned_run;
                            had_meaningful_output.store(false, Ordering::Relaxed);
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            continue;
                        }
                        // FIX-TRANSPORT-CLASSIFY: terminal stream errors also
                        // carry a class label and redacted message for triage.
                        let terminal_class = crate::transport::classify_message(&err_str);
                        crate::metrics::init_metrics().record_error("bridge_transport_terminal", terminal_class.as_str());
                        tracing::error!(
                            target: "agentgate.transport",
                            provider_id = %provider_id,
                            model = %model,
                            error_class = terminal_class.as_str(),
                            error = %crate::transport::redact_diagnostics(&err_str),
                            "provider stream error"
                        );
                        let _ = tx
                            .send(RunEvent::Error {
                                message: err_str,
                                code: Some("provider_error".to_string()),
                            })
                            .await;
                    } else {
                        tracing::info!("provider stream DONE: model='{}'", model);
                    }
                    break;
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

        (rx, session_tool_names)
    }

    pub async fn add_provider(&mut self, request: AddProviderRequest) -> Result<(), AppError> {
        println!("Adding provider: {}", request.id);
        self.add_providers.write().await.push(request);
        Ok(())
    }
}

/// Deadline (seconds) for a single non-streaming provider call.
///
/// CLI agents can block indefinitely: a missing ollama model starts a multi-GB
/// auto-pull, and some CLI versions do not exit after an upstream error. Without
/// this bound the HTTP client sees no response at all. Override with
/// `AGENTGATE_PROVIDER_TIMEOUT_SECS`. Default: 60s.
fn provider_call_timeout_secs() -> u64 {
    std::env::var("AGENTGATE_PROVIDER_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(60)
}

#[cfg(test)]
mod transport_regression_tests {
    use super::*;

    /// Regression for the reported incident: transient transport failures must
    /// be retried, quota/auth failures must not be retried as transport errors.
    #[test]
    fn incident_transport_messages_are_classified_transient() {
        for message in [
            "provider_error: Transport: ECONNREFUSED: Unable to connect. Is the computer able to access the url?",
            "provider_error: Transport: ECONNRESET: The socket connection was closed unexpectedly",
            "Error: Broken pipe (os error 32)",
            "provider_error: Standalone server exited before reporting readiness",
            "connection refused (os error 111)",
            "connection reset by peer (os error 104)",
            "failed to spawn opencode process: resource temporarily unavailable",
        ] {
            assert!(is_transient_bridge_error(message), "should be transient: {message}");
        }

        for fatal in [
            "free usage limit reached",
            "provider.free-limit",
            "model_no_tool_support: no tools",
            "tool_isolation_violation",
        ] {
            assert!(!is_transient_bridge_error(fatal), "should be fatal: {fatal}");
        }
    }
}

#[cfg(test)]
mod http_router_tests {
    use super::*;

    #[test]
    fn skip_retry_only_when_client_gone_with_error() {
        assert!(skip_retry_when_client_gone(true, "Broken pipe (os error 32)"));
        assert!(skip_retry_when_client_gone(true, "readiness"));
        assert!(!skip_retry_when_client_gone(false, "Broken pipe (os error 32)"));
        assert!(!skip_retry_when_client_gone(true, ""));
    }

    fn nvidia_cfg() -> ProviderConfig {
        ProviderConfig {
            id: "nvidia".to_string(),
            enabled: true,
            provider_type: crate::provider::mappings::HTTP_PROVIDER_TYPE.to_string(),
            mode: "http".to_string(),
            adapter: crate::provider::mappings::OPENAI_COMPATIBLE_ADAPTER.to_string(),
            base_url: Some("https://integrate.api.nvidia.com/v1".to_string()),
            chat_completions_path: "/chat/completions".to_string(),
            models_path: "/models".to_string(),
            api_key: Some("nvapi-test-key".to_string()),
            api_key_prefix: Some("nvapi-".to_string()),
            mapping_status: "verified".to_string(),
            forward_tools: true,
            models: vec!["meta/llama-3.3-70b-instruct".to_string()],
            timeout_secs: 10,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn http_models_are_merged_and_resolved() {
        let registry = ServiceRegistry::new();
        assert!(registry.register_http_provider(&nvidia_cfg()).await);
        assert!(registry.is_provider_available("nvidia").await);
        let gateway_id = "agg/nvidia/meta/llama-3.3-70b-instruct";
        assert_eq!(
            registry.provider_for_model(gateway_id).await.as_deref(),
            Some("nvidia")
        );
        assert_eq!(
            registry.upstream_model_for(gateway_id).await.as_deref(),
            Some("meta/llama-3.3-70b-instruct")
        );
        let models = registry.list_models(None).await.expect("model list");
        assert!(
            models.data.iter().any(|m| m.id == gateway_id && m.provider == "nvidia"),
            "HTTP models must be published in /v1/models"
        );
    }

    #[tokio::test]
    async fn http_provider_never_registers_as_cli() {
        let mut registry = ServiceRegistry::new();
        registry.register_configured(&[nvidia_cfg()]).await;
        assert!(registry.http_provider("nvidia").await.is_some());
        assert!(!registry.providers.read().await.contains_key("nvidia"));
        assert!(!registry.native_providers.read().await.contains_key("nvidia"));
    }

    #[tokio::test]
    async fn explicit_http_mode_is_honored_even_with_tools() {
        let registry = ServiceRegistry::new();
        registry.register_http_provider(&nvidia_cfg()).await;
        assert_eq!(
            registry.select_execution_mode("agg/nvidia/meta/llama-3.3-70b-instruct", true).await,
            ExecutionMode::Http
        );
        // Unknown models keep the legacy CLI path.
        assert_eq!(
            registry.select_execution_mode("unknown/model", false).await,
            ExecutionMode::Cli
        );
    }

    #[tokio::test]
    async fn chat_completion_dispatches_to_http_upstream() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-test",
                "object": "chat.completion",
                "created": 1,
                "model": "meta/llama-3.3-70b-instruct",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "hello from NIM" },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
            })))
            .mount(&server)
            .await;

        let mut cfg = nvidia_cfg();
        cfg.base_url = Some(format!("{}/v1", server.uri()));
        let registry = ServiceRegistry::new();
        assert!(registry.register_http_provider(&cfg).await);

        let request = ChatCompletionRequest {
            model: "agg/nvidia/meta/llama-3.3-70b-instruct".to_string(),
            messages: vec![crate::types::ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        let response = registry.chat_completion(request).await.expect("HTTP chat ok");
        assert_eq!(response.choices[0].message.content.as_deref(), Some("hello from NIM"));
        assert_eq!(response.usage.map(|u| u.total_tokens), Some(8));
    }

    #[tokio::test]
    async fn combo_falls_back_on_429_to_next_candidate() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let failing = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limit reached"))
            .mount(&failing)
            .await;
        let working = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-fallback",
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": "from fallback" }, "finish_reason": "stop" }]
            })))
            .mount(&working)
            .await;

        let mut first = nvidia_cfg();
        first.id = "nvidia".to_string();
        first.base_url = Some(format!("{}/v1", failing.uri()));
        let mut second = nvidia_cfg();
        second.id = "groq".to_string();
        second.base_url = Some(format!("{}/v1", working.uri()));

        let registry = ServiceRegistry::new();
        assert!(registry.register_http_provider(&first).await);
        assert!(registry.register_http_provider(&second).await);

        let combo = ComboModel {
            id: "combo/test-fallback".to_string(),
            enabled: true,
            strategy: "fallback".to_string(),
            candidates: vec![
                crate::provider::combo::ComboCandidate {
                    model: "agg/nvidia/meta/llama-3.3-70b-instruct".to_string(),
                    provider: None,
                    mode: None,
                    weight: 1,
                },
                crate::provider::combo::ComboCandidate {
                    model: "agg/groq/llama-3.1-8b-instant".to_string(),
                    provider: None,
                    mode: None,
                    weight: 1,
                },
            ],
            fallback_on: crate::provider::combo::default_fallback_conditions(),
            max_attempts: 2,
        };
        let resolver = |id: &str| {
            if id.starts_with("agg/nvidia/") || id.starts_with("agg/groq/") {
                Some(ExecutionMode::Http)
            } else {
                None
            }
        };
        crate::provider::combo::registry().create(combo, &resolver).await.unwrap();

        let request = ChatCompletionRequest {
            model: "combo/test-fallback".to_string(),
            messages: vec![crate::types::ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        let response = registry.chat_completion(request).await.expect("combo fallback ok");
        assert_eq!(response.choices[0].message.content.as_deref(), Some("from fallback"));
    }
}




