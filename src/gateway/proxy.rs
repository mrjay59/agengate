//! Per-provider proxy selection with single/pool strategies and health bans.
//!
//! Runtime integration is pull-based so the HTTP provider can ask for a proxy
//! right before sending a request and report the outcome afterwards:
//!
//! ```ignore
//! let endpoint = gateway::proxy::select_for_provider("openrouter", Some(session_id)).await?;
//! // ... send request through endpoint.url ...
//! gateway::proxy::report_result("openrouter", &endpoint, outcome);
//! ```

use crate::gateway::store::{GatewayError, ProxyConfig, ProxyMode, ProxyStrategy};
use crate::gateway::state;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEndpoint {
    /// Id of the owning proxy config.
    pub proxy_id: String,
    /// Exact URL to use (may contain credentials — never log it directly).
    pub url: String,
    pub strategy: ProxyStrategy,
}

impl ProxyEndpoint {
    /// Safe form for logs, metrics and admin responses.
    pub fn display(&self) -> String { crate::gateway::store::redact_url(&self.url) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyOutcome {
    Success { latency: Duration },
    Quota,       // HTTP 429 / provider.free-limit
    ServerError, // HTTP 5xx
    Timeout,
}

#[derive(Debug, Clone)]
struct Health {
    successes: u64,
    errors: u64,
    quota_errors: u64,
    latency_ewma_ms: f64,
    last_error: Option<String>,
    banned_until: Option<Instant>,
}

impl Default for Health {
    fn default() -> Self {
        Self { successes: 0, errors: 0, quota_errors: 0, latency_ewma_ms: 0.0, last_error: None, banned_until: None }
    }
}

impl Health {
    fn healthy(&self) -> bool {
        self.banned_until.map(|t| Instant::now() >= t).unwrap_or(true)
    }
}

#[derive(Default)]
struct ProxyRuntime {
    health: DashMap<String, Health>,
    sticky: DashMap<String, String>,
    rr: AtomicU64,
}

static RUNTIME: OnceLock<ProxyRuntime> = OnceLock::new();
fn runtime() -> &'static ProxyRuntime { RUNTIME.get_or_init(ProxyRuntime::default) }

fn health_key(provider_id: &str, url: &str) -> String {
    format!("{}|{}", provider_id, crate::gateway::store::redact_url(url))
}

/// Resolve the effective proxy config for a provider: explicit proxy id first,
/// then the provider's inline config, then a standalone config with the same id.
pub async fn config_for_provider(provider_id: &str) -> Option<ProxyConfig> {
    let doc = state().document().await;
    if let Some(provider) = doc.providers.get(provider_id) {
        if let Some(proxy_id) = &provider.proxy_id {
            if let Some(proxy) = doc.proxies.get(proxy_id) {
                return Some(proxy.clone());
            }
            tracing::warn!(event = "proxy.config.missing", provider_id, proxy_id, "Referenced proxy config not found; falling back to inline config");
        }
        return Some(provider.proxy.clone());
    }
    doc.proxies.get(provider_id).cloned()
}

/// Select a proxy endpoint for a provider.
///
/// `Ok(None)` means "direct connection" (proxy mode none, or all endpoints
/// banned with `fallback_direct=true`). `Err(AllProxiesUnhealthy)` fails the
/// request closed when every endpoint is banned and direct fallback is off.
pub async fn select_for_provider(
    provider_id: &str,
    sticky_key: Option<&str>,
) -> Result<Option<ProxyEndpoint>, GatewayError> {
    let config = match config_for_provider(provider_id).await {
        Some(config) => config,
        None => return Ok(None),
    };
    select_with_config(provider_id, &config, sticky_key).await
}

/// Select from an explicit config (useful for tests and dynamically built configs).
pub async fn select_with_config(
    provider_id: &str,
    config: &ProxyConfig,
    sticky_key: Option<&str>,
) -> Result<Option<ProxyEndpoint>, GatewayError> {
    if !config.enabled || config.mode == ProxyMode::None {
        return Ok(None);
    }
    let mut endpoints = config.endpoints();
    if endpoints.is_empty() {
        return Ok(None);
    }
    endpoints.retain(|url| {
        runtime().health.get(&health_key(provider_id, url)).map(|h| h.healthy()).unwrap_or(true)
    });

    if endpoints.is_empty() {
        if config.fallback_direct {
            tracing::warn!(event = "proxy.all_banned.direct_fallback", provider_id);
            return Ok(None);
        }
        return Err(GatewayError::AllProxiesUnhealthy);
    }

    let url = match config.strategy {
        ProxyStrategy::Sticky => {
            let key = sticky_key.unwrap_or(provider_id).to_string();
            let saved = runtime().sticky.get(&key).map(|v| v.clone());
            match saved {
                Some(ref url) if endpoints.contains(url) => url.clone(),
                _ => {
                    let picked = endpoints[next_index(&endpoints.len(), config.strategy)].clone();
                    runtime().sticky.insert(key, picked.clone());
                    picked
                }
            }
        }
        ProxyStrategy::LeastErrors => endpoints
            .iter()
            .min_by_key(|url| {
                runtime().health.get(&health_key(provider_id, url)).map(|h| h.errors).unwrap_or(0)
            })
            .cloned()
            .unwrap_or_else(|| endpoints[next_index(&endpoints.len(), config.strategy)].clone()),
        ProxyStrategy::Random => {
            let seed = uuid::Uuid::new_v4().as_bytes()[0] as usize;
            endpoints[seed % endpoints.len()].clone()
        }
        ProxyStrategy::RoundRobin => endpoints[next_index(&endpoints.len(), config.strategy)].clone(),
    };

    crate::metrics::init_metrics().record_proxy_selected(
        provider_id,
        &crate::gateway::store::redact_url(&url),
        strategy_name(config.strategy),
    );
    tracing::info!(event = "proxy.selected", provider_id, proxy = %crate::gateway::store::redact_url(&url), strategy = strategy_name(config.strategy));
    Ok(Some(ProxyEndpoint { proxy_id: config.id.clone(), url, strategy: config.strategy }))
}

fn next_index(len: &usize, strategy: ProxyStrategy) -> usize {
    if *len == 0 { return 0; }
    match strategy {
        ProxyStrategy::Random => (uuid::Uuid::new_v4().as_bytes()[1] as usize) % len,
        _ => {
            let _ = strategy;
            (runtime().rr.fetch_add(1, Ordering::Relaxed) as usize) % len
        }
    }
}

/// Update health counters and temporary bans after an upstream attempt.
/// Synchronous so it can be called from any runtime context.
pub fn report_result(provider_id: &str, endpoint: &ProxyEndpoint, outcome: ProxyOutcome) {
    let (ban_429, ban_5xx, ban_timeout) = (60u64, 30u64, 30u64);
    apply_outcome(provider_id, endpoint, outcome, ban_429, ban_5xx, ban_timeout);
}

/// Same as [`report_result`] but honours the persisted ban durations.
pub async fn report_result_async(provider_id: &str, endpoint: &ProxyEndpoint, outcome: ProxyOutcome) {
    let (ban_429, ban_5xx, ban_timeout) = config_for_provider(provider_id)
        .await
        .map(|c| (c.ban_on_429_secs, c.ban_on_5xx_secs, c.ban_on_timeout_secs))
        .unwrap_or((60, 30, 30));
    apply_outcome(provider_id, endpoint, outcome, ban_429, ban_5xx, ban_timeout);
}

fn apply_outcome(
    provider_id: &str,
    endpoint: &ProxyEndpoint,
    outcome: ProxyOutcome,
    ban_429: u64,
    ban_5xx: u64,
    ban_timeout: u64,
) {
    let key = health_key(provider_id, &endpoint.url);
    let mut entry = runtime().health.entry(key).or_default();
    let health = entry.value_mut();
    let class = match outcome {
        ProxyOutcome::Success { latency } => {
            health.successes += 1;
            let sample = latency.as_millis() as f64;
            health.latency_ewma_ms = if health.latency_ewma_ms == 0.0 { sample } else { 0.8 * health.latency_ewma_ms + 0.2 * sample };
            health.banned_until = None;
            "success"
        }
        ProxyOutcome::Quota => {
            health.errors += 1;
            health.quota_errors += 1;
            health.last_error = Some("quota".to_string());
            health.banned_until = Some(Instant::now() + Duration::from_secs(ban_429));
            "quota"
        }
        ProxyOutcome::ServerError => {
            health.errors += 1;
            health.last_error = Some("5xx".to_string());
            health.banned_until = Some(Instant::now() + Duration::from_secs(ban_5xx));
            "server_error"
        }
        ProxyOutcome::Timeout => {
            health.errors += 1;
            health.last_error = Some("timeout".to_string());
            health.banned_until = Some(Instant::now() + Duration::from_secs(ban_timeout));
            "timeout"
        }
    };
    crate::metrics::init_metrics().record_proxy_error(provider_id, &endpoint.display(), class);
    tracing::warn!(event = "proxy.outcome", provider_id, proxy = %endpoint.display(), class, errors = health.errors, "Proxy health updated");
}

/// Health snapshot for `/admin/proxies/:id/test` and diagnostics. Keys are
/// already redacted URLs, so this is safe to return to admin clients.
pub async fn health_snapshot(provider_id: &str) -> serde_json::Value {
    let items: Vec<serde_json::Value> = runtime()
        .health
        .iter()
        .filter(|entry| entry.key().starts_with(&format!("{}|", provider_id)))
        .map(|entry| {
            let (key, h) = entry.pair();
            serde_json::json!({
                "proxy": key.split_once('|').map(|(_, u)| u).unwrap_or(key),
                "successes": h.successes,
                "errors": h.errors,
                "quota_errors": h.quota_errors,
                "latency_ewma_ms": h.latency_ewma_ms,
                "banned": h.banned_until.map(|t| t > Instant::now()).unwrap_or(false),
                "last_error": h.last_error,
            })
        })
        .collect();
    serde_json::json!({ "provider_id": provider_id, "endpoints": items })
}

pub fn strategy_name(strategy: ProxyStrategy) -> &'static str {
    match strategy {
        ProxyStrategy::RoundRobin => "round_robin",
        ProxyStrategy::Random => "random",
        ProxyStrategy::LeastErrors => "least_errors",
        ProxyStrategy::Sticky => "sticky",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(mode: ProxyMode, urls: Vec<&str>, strategy: ProxyStrategy) -> ProxyConfig {
        let mut cfg = ProxyConfig::default();
        cfg.id = "test-proxy".to_string();
        cfg.mode = mode;
        cfg.pool = urls.into_iter().map(str::to_string).collect();
        cfg.single_url = cfg.pool.first().cloned();
        cfg.strategy = strategy;
        cfg
    }

    #[tokio::test]
    async fn none_mode_returns_direct() {
        let cfg = config(ProxyMode::None, vec![], ProxyStrategy::RoundRobin);
        assert!(select_with_config("p1", &cfg, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn round_robin_cycles_endpoints() {
        // The RR counter is process-global and shared with other tests, so
        // assert distribution over several picks instead of exact adjacency.
        let cfg = config(ProxyMode::Pool, vec!["http://a:8080", "http://b:8080"], ProxyStrategy::RoundRobin);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            let picked = select_with_config("p-rr", &cfg, None).await.unwrap().unwrap();
            seen.insert(picked.url);
        }
        assert_eq!(seen.len(), 2);
    }

    #[tokio::test]
    async fn sticky_reuses_same_endpoint() {
        let cfg = config(ProxyMode::Pool, vec!["http://a:8080", "http://b:8080"], ProxyStrategy::Sticky);
        let first = select_with_config("p-sticky", &cfg, Some("session-1")).await.unwrap().unwrap();
        let second = select_with_config("p-sticky", &cfg, Some("session-1")).await.unwrap().unwrap();
        assert_eq!(first.url, second.url);
    }

    #[tokio::test]
    async fn bans_are_applied_and_fallback_direct_works() {
        let mut cfg = config(ProxyMode::Pool, vec!["http://banned-test:8080"], ProxyStrategy::RoundRobin);
        cfg.id = "ban-test".to_string();
        cfg.fallback_direct = true;
        let endpoint = select_with_config("p-ban", &cfg, None).await.unwrap().unwrap();
        report_result("p-ban", &endpoint, ProxyOutcome::Quota);
        assert!(select_with_config("p-ban", &cfg, None).await.unwrap().is_none());
        cfg.fallback_direct = false;
        assert!(select_with_config("p-ban", &cfg, None).await.is_err());
    }

    #[test]
    fn redaction_never_leaks_proxy_credentials() {
        let endpoint = ProxyEndpoint { proxy_id: "p".into(), url: "http://u:p@h:8080".into(), strategy: ProxyStrategy::RoundRobin };
        assert!(!endpoint.display().contains("u:p"));
    }
}
