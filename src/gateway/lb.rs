//! Quota-aware load balancing across free models.
//!
//! Candidate eligibility is decided by the caller (quota state, provider
//! availability, key permissions); this module owns strategy selection, sticky
//! sessions, per-candidate health/cooldown and the fallback decision. A
//! selection is always annotated so callers can log `from`, `to`, `reason`,
//! `pool`, `attempt` and `fallback` (spec §13.2 step 7).

use crate::gateway::store::{LbStrategy, PoolConfig, StickyKey, DEFAULT_FREE_FALLBACK, DEFAULT_FREE_POOL};
use crate::gateway::state;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LbCandidate {
    pub model: String,
    pub provider_id: String,
    #[allow(dead_code)] // populated by the model catalog; weight overridable by pool members
    pub weight: u32,
}

impl LbCandidate {
    pub fn new(model: impl Into<String>, provider_id: impl Into<String>) -> Self {
        Self { model: model.into(), provider_id: provider_id.into(), weight: 1 }
    }

    /// Canonical key used for quota/disabled lookups and selection stats.
    pub fn key(&self) -> String {
        let model = self.model.trim().to_lowercase();
        let stripped = model.strip_prefix("agg/").unwrap_or(&model);
        if stripped.starts_with(&format!("{}/", self.provider_id)) {
            stripped.to_string()
        } else {
            format!("{}/{}", self.provider_id, stripped)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LbSelection {
    pub model: String,
    pub from_model: String,
    pub reason: String,
    pub pool: String,
    pub fallback: bool,
    pub strategy: &'static str,
}

#[derive(Debug, Default)]
struct LbStats {
    selected: u64,
    errors: u64,
    latency_ewma_ms: f64,
    cooldown_until: Option<Instant>,
}

impl LbStats {
    fn available(&self) -> bool {
        self.cooldown_until.map(|t| Instant::now() >= t).unwrap_or(true)
    }
}

#[derive(Default)]
struct LbRuntime {
    stats: DashMap<String, LbStats>,
    sticky: DashMap<String, String>,
    rr: AtomicU64,
}

static RUNTIME: OnceLock<LbRuntime> = OnceLock::new();
fn runtime() -> &'static LbRuntime { RUNTIME.get_or_init(LbRuntime::default) }

fn stat_key(pool: &str, candidate: &LbCandidate) -> String {
    format!("{}|{}", pool, candidate.key())
}

/// Mark a candidate as temporarily unavailable (quota hit / 5xx / timeout).
pub fn cooldown(pool: &str, model: &str, provider_id: &str, seconds: u64) {
    let candidate = LbCandidate::new(model, provider_id);
    let mut entry = runtime().stats.entry(stat_key(pool, &candidate)).or_default();
    entry.cooldown_until = Some(Instant::now() + Duration::from_secs(seconds));
    entry.errors += 1;
    tracing::warn!(event = "quota.model.cooldown", pool, model, provider_id, seconds, "Candidate cooled down after error");
}

/// Apply the cooldown configured for a pool after a quota/429 hit.
pub async fn cooldown_after_quota(pool: &str, model: &str, provider_id: &str) {
    let config = pool_config(pool).await;
    cooldown(pool, model, provider_id, config.cooldown_on_quota_secs);
}

/// Record a successful attempt (feeds least-latency scoring).
pub fn record_success(pool: &str, model: &str, provider_id: &str, latency: Duration) {
    let candidate = LbCandidate::new(model, provider_id);
    let mut entry = runtime().stats.entry(stat_key(pool, &candidate)).or_default();
    entry.selected += 1;
    let sample = latency.as_millis() as f64;
    entry.latency_ewma_ms = if entry.latency_ewma_ms == 0.0 { sample } else { 0.8 * entry.latency_ewma_ms + 0.2 * sample };
}

/// Resolve pool config, falling back to a sane default for the free pool.
async fn pool_config(pool_name: &str) -> PoolConfig {
    match state().get_pool(pool_name).await {
        Some(pool) => pool,
        None => {
            let mut pool = PoolConfig::default();
            pool.name = pool_name.to_string();
            pool
        }
    }
}

/// Effective sticky key for this request. `session_id` is the default for the
/// free pool; API-key stickiness can be requested by passing the key id.
fn sticky_key_for(config: &PoolConfig, session_id: Option<&str>, api_key_id: Option<&str>) -> Option<String> {
    match config.sticky_key {
        StickyKey::None => None,
        StickyKey::SessionId => session_id.map(str::to_string),
        StickyKey::ApiKey => api_key_id.map(str::to_string).or_else(|| session_id.map(str::to_string)),
    }
}

/// Choose a candidate from an already-filtered list.
///
/// Returns `None` when no candidate is eligible — the caller must then apply
/// the pool's hard fallback (see [`fallback_selection`]).
pub async fn select(
    pool_name: &str,
    candidates: &[LbCandidate],
    from_model: &str,
    reason: &str,
    session_id: Option<&str>,
    api_key_id: Option<&str>,
) -> Option<LbSelection> {
    let config = pool_config(pool_name).await;
    if !config.enabled || candidates.is_empty() {
        return None;
    }

    // Pool member weights override catalog weights; enabled=false excludes.
    let mut eligible: Vec<LbCandidate> = Vec::new();
    for candidate in candidates {
        if !runtime().stats.get(&stat_key(pool_name, candidate)).map(|s| s.available()).unwrap_or(true) {
            continue;
        }
        let mut candidate = candidate.clone();
        if let Some(member) = config.members.iter().find(|m| m.model == candidate.model) {
            if !member.enabled { continue; }
            candidate.weight = member.weight.max(1);
        }
        eligible.push(candidate);
    }
    if eligible.is_empty() {
        return None;
    }

    let sticky = sticky_key_for(&config, session_id, api_key_id);
    if let Some(key) = &sticky {
        if let Some(saved) = runtime().sticky.get(key) {
            if let Some(found) = eligible.iter().find(|c| c.model == *saved.value()) {
                let selection = LbSelection {
                    model: found.model.clone(),
                    from_model: from_model.to_string(),
                    reason: format!("{}:sticky", reason),
                    pool: pool_name.to_string(),
                    fallback: false,
                    strategy: strategy_name(config.strategy),
                };
                record_selection(&selection);
                return Some(selection);
            }
        }
    }

    let picked = match config.strategy {
        LbStrategy::WeightedRoundRobin => weighted_rr(pool_name, &eligible),
        LbStrategy::RoundRobin => {
            let idx = runtime().rr.fetch_add(1, Ordering::Relaxed) as usize % eligible.len();
            eligible[idx].clone()
        }
        LbStrategy::Random => {
            let idx = uuid::Uuid::new_v4().as_bytes()[0] as usize % eligible.len();
            eligible[idx].clone()
        }
        LbStrategy::LeastErrors => least_by(pool_name, &eligible, |s| s.errors),
        LbStrategy::LeastLatency => least_by(pool_name, &eligible, |s| s.latency_ewma_ms as u64),
    };

    if let Some(key) = sticky {
        runtime().sticky.insert(key, picked.model.clone());
    }
    let selection = LbSelection {
        model: picked.model.clone(),
        from_model: from_model.to_string(),
        reason: reason.to_string(),
        pool: pool_name.to_string(),
        fallback: false,
        strategy: strategy_name(config.strategy),
    };
    record_selection(&selection);
    Some(selection)
}

fn weighted_rr(pool: &str, candidates: &[LbCandidate]) -> LbCandidate {
    let total: u64 = candidates.iter().map(|c| c.weight as u64).sum();
    let ticket = runtime().rr.fetch_add(1, Ordering::Relaxed) % total.max(1);
    let mut cursor = 0u64;
    for candidate in candidates {
        cursor += candidate.weight as u64;
        if ticket < cursor {
            let mut entry = runtime().stats.entry(stat_key(pool, candidate)).or_default();
            entry.selected += 1;
            return candidate.clone();
        }
    }
    candidates.last().cloned().unwrap_or_else(|| LbCandidate::new(DEFAULT_FREE_FALLBACK, "opencode"))
}

fn least_by(
    pool: &str,
    candidates: &[LbCandidate],
    score: impl Fn(&LbStats) -> u64,
) -> LbCandidate {
    candidates
        .iter()
        .min_by_key(|c| {
            runtime().stats.get(&stat_key(pool, c)).map(|s| score(&s)).unwrap_or(0)
        })
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// Hard fallback when no free candidate remains (spec §13.2 step 6).
pub async fn fallback_selection(pool_name: &str, from_model: &str, reason: &str) -> LbSelection {
    let config = pool_config(pool_name).await;
    let to_model = config
        .fallback
        .first()
        .cloned()
        .unwrap_or_else(|| DEFAULT_FREE_FALLBACK.to_string());
    let selection = LbSelection {
        model: to_model.clone(),
        from_model: from_model.to_string(),
        reason: reason.to_string(),
        pool: pool_name.to_string(),
        fallback: true,
        strategy: strategy_name(config.strategy),
    };
    record_selection(&selection);
    crate::metrics::init_metrics().record_lb_fallback(pool_name, &to_model, reason);
    selection
}

fn record_selection(selection: &LbSelection) {
    crate::metrics::init_metrics().record_lb_selection(
        &selection.pool,
        &selection.from_model,
        &selection.model,
        &selection.reason,
    );
    tracing::info!(
        event = if selection.fallback { "quota.model.fallback_9router" } else { "quota.model.reroute" },
        pool = %selection.pool,
        from = %selection.from_model,
        to = %selection.model,
        reason = %selection.reason,
        strategy = selection.strategy,
        fallback = selection.fallback,
        "Free-pool routing decision"
    );
}

pub fn strategy_name(strategy: LbStrategy) -> &'static str {
    match strategy {
        LbStrategy::RoundRobin => "round_robin",
        LbStrategy::WeightedRoundRobin => "weighted_round_robin",
        LbStrategy::LeastLatency => "least_latency",
        LbStrategy::LeastErrors => "least_errors",
        LbStrategy::Random => "random",
    }
}

/// Diagnostics for admin/health endpoints.
pub fn stats_snapshot() -> serde_json::Value {
    let entries: Vec<serde_json::Value> = runtime()
        .stats
        .iter()
        .map(|entry| {
            let (key, s) = entry.pair();
            serde_json::json!({
                "candidate": key,
                "selected": s.selected,
                "errors": s.errors,
                "latency_ewma_ms": s.latency_ewma_ms,
                "cooling_down": s.cooldown_until.map(|t| t > Instant::now()).unwrap_or(false),
            })
        })
        .collect();
    serde_json::json!({ "candidates": entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<LbCandidate> {
        vec![
            LbCandidate::new("opencode/beam-free", "opencode"),
            LbCandidate::new("opencode/flint-40b-free", "opencode"),
        ]
    }

    #[tokio::test]
    async fn weighted_rr_distributes_across_candidates() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            let picked = select(DEFAULT_FREE_POOL, &candidates(), "opencode/iceman-free", "quota_disabled", None, None)
                .await
                .expect("candidate available");
            seen.insert(picked.model);
        }
        assert_eq!(seen.len(), 2);
    }

    #[tokio::test]
    async fn sticky_keeps_same_model() {
        let first = select("sticky-pool", &candidates(), "from", "quota_disabled", Some("s1"), None)
            .await
            .unwrap();
        for _ in 0..5 {
            let next = select("sticky-pool", &candidates(), "from", "quota_disabled", Some("s1"), None)
                .await
                .unwrap();
            assert_eq!(next.model, first.model);
        }
    }

    #[tokio::test]
    async fn cooldown_filters_candidate() {
        cooldown("cool-pool", "opencode/beam-free", "opencode", 60);
        for _ in 0..10 {
            let picked = select("cool-pool", &candidates(), "from", "quota_disabled", None, None)
                .await
                .unwrap();
            assert_ne!(picked.model, "opencode/beam-free");
        }
    }

    #[tokio::test]
    async fn fallback_targets_9router_by_default() {
        let picked = fallback_selection(DEFAULT_FREE_POOL, "opencode/beam-free", "pool_exhausted").await;
        assert!(picked.fallback);
        assert_eq!(picked.model, DEFAULT_FREE_FALLBACK);
    }

    #[test]
    fn candidate_key_normalises_agg_prefix() {
        let c = LbCandidate::new("agg/opencode/beam-free", "opencode");
        assert_eq!(c.key(), "opencode/beam-free");
    }
}
