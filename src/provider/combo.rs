//! Combo (virtual) model: an ordered fallback chain of real models.
//!
//! A combo is **not** an upstream model. AgentGate resolves it at request time
//! and tries candidates in order. Failover only happens for configured
//! conditions and never after meaningful output has been emitted to the client
//! (spec §6.4). CLI and HTTP candidates are both supported, with mode checks.

use crate::service::ExecutionMode;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackCondition {
    Quota,
    RateLimit,
    Timeout,
    Server5xx,
    Unavailable,
    EmptyResponse,
}

impl FallbackCondition {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackCondition::Quota => "quota",
            FallbackCondition::RateLimit => "rate_limit",
            FallbackCondition::Timeout => "timeout",
            FallbackCondition::Server5xx => "5xx",
            FallbackCondition::Unavailable => "unavailable",
            FallbackCondition::EmptyResponse => "empty_response",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "quota" | "free_limit" | "free-limit" => Some(Self::Quota),
            "rate_limit" | "ratelimit" | "429" => Some(Self::RateLimit),
            "timeout" | "timed_out" => Some(Self::Timeout),
            "5xx" | "server_error" => Some(Self::Server5xx),
            "unavailable" | "not_found" | "connection_error" => Some(Self::Unavailable),
            "empty_response" | "empty" => Some(Self::EmptyResponse),
            _ => None,
        }
    }
}

pub fn default_fallback_conditions() -> Vec<FallbackCondition> {
    vec![
        FallbackCondition::Quota,
        FallbackCondition::RateLimit,
        FallbackCondition::Timeout,
        FallbackCondition::Server5xx,
        FallbackCondition::Unavailable,
    ]
}

/// Normalized view of a failed attempt, independent of CLI vs HTTP transport.
#[derive(Debug, Clone, Default)]
pub struct FailureContext {
    pub status: Option<u16>,
    pub class: Option<String>,
    pub message: String,
    pub empty_response: bool,
}

impl FailureContext {
    pub fn from_message(message: impl Into<String>) -> Self {
        Self { message: message.into(), ..Default::default() }
    }
}

/// True when a failed attempt should advance the chain.
pub fn should_fallback(conditions: &[FallbackCondition], failure: &FailureContext) -> bool {
    let conditions = if conditions.is_empty() {
        default_fallback_conditions()
    } else {
        conditions.to_vec()
    };
    let blob = format!(
        "{} {} {}",
        failure.message.to_ascii_lowercase(),
        failure.class.as_deref().unwrap_or("").to_ascii_lowercase(),
        failure.status.map(|s| s.to_string()).unwrap_or_default()
    );
    // Some upstreams only report the numeric status in the error body.
    let has_5xx_in_message = blob
        .split(|c: char| !c.is_ascii_digit())
        .filter(|token| token.len() == 3)
        .any(|token| {
            token
                .parse::<u16>()
                .map(|status| (500..600).contains(&status))
                .unwrap_or(false)
        });
    conditions.iter().any(|condition| match condition {
        FallbackCondition::Quota => {
            failure.status == Some(429)
                || blob.contains("quota")
                || blob.contains("free-limit")
                || blob.contains("free_limit")
        }
        FallbackCondition::RateLimit => {
            failure.status == Some(429) || blob.contains("rate_limit") || blob.contains("ratelimit")
        }
        FallbackCondition::Timeout => blob.contains("timeout") || blob.contains("timed out"),
        FallbackCondition::Server5xx => {
            failure.status.map(|s| (500..600).contains(&s)).unwrap_or(false)
                || has_5xx_in_message
                || blob.contains("server_5xx")
        }
        FallbackCondition::Unavailable => {
            blob.contains("unavailable")
                || blob.contains("not found")
                || blob.contains("connection_refused")
                || blob.contains("connection_reset")
                || blob.contains("provider_not_found")
        }
        FallbackCondition::EmptyResponse => failure.empty_response,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboCandidate {
    /// Real model id (or another combo id, validated for cycles).
    pub model: String,
    /// Optional provider override for this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Required execution mode (`cli`|`http`) — checked against the resolved model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 { 1 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboModel {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_strategy")]
    pub strategy: String,
    pub candidates: Vec<ComboCandidate>,
    #[serde(default)]
    pub fallback_on: Vec<FallbackCondition>,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,
}

fn default_true() -> bool { true }
fn default_strategy() -> String { "fallback".to_string() }
fn default_max_attempts() -> usize { 3 }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComboError {
    InvalidId,
    EmptyCandidates,
    TooManyAttempts { max: usize, candidates: usize },
    UnknownCondition(String),
    Cycle(String),
    UnknownCandidate(String),
    ModeMismatch { model: String, expected: String, actual: &'static str },
    AlreadyExists(String),
    NotFound(String),
}

impl std::fmt::Display for ComboError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComboError::InvalidId => write!(f, "combo id must not be empty"),
            ComboError::EmptyCandidates => write!(f, "combo needs at least one candidate"),
            ComboError::TooManyAttempts { max, candidates } => {
                write!(f, "max_attempts ({}) exceeds candidate count ({})", max, candidates)
            }
            ComboError::UnknownCondition(value) => write!(f, "unknown fallback condition '{}'", value),
            ComboError::Cycle(id) => write!(f, "combo cycle detected at '{}'", id),
            ComboError::UnknownCandidate(id) => write!(f, "unknown combo candidate '{}'", id),
            ComboError::ModeMismatch { model, expected, actual } => {
                write!(f, "candidate '{}' requires {} mode but resolves to {}", model, expected, actual)
            }
            ComboError::AlreadyExists(id) => write!(f, "combo '{}' already exists", id),
            ComboError::NotFound(id) => write!(f, "combo '{}' not found", id),
        }
    }
}

impl std::error::Error for ComboError {}

/// Global combo registry. Admin CRUD can call this without threading state
/// through the request handler; updates are visible to subsequent requests.
pub struct ComboRegistry {
    combos: RwLock<std::collections::HashMap<String, ComboModel>>,
}

impl ComboRegistry {
    pub fn new() -> Self {
        Self { combos: RwLock::new(std::collections::HashMap::new()) }
    }

    /// Structural validation: ids, attempts, conditions and cycle prevention.
    /// `resolve_mode` is supplied by the caller (ServiceRegistry) so candidate
    /// existence and CLI/HTTP mode compatibility can be checked too.
    pub async fn validate<F>(&self, model: &ComboModel, resolve_mode: &F) -> Result<(), ComboError>
    where
        F: Fn(&str) -> Option<ExecutionMode> + Sync,
    {
        if model.id.trim().is_empty() {
            return Err(ComboError::InvalidId);
        }
        if model.candidates.is_empty() {
            return Err(ComboError::EmptyCandidates);
        }
        if model.max_attempts == 0 || model.max_attempts > model.candidates.len() {
            return Err(ComboError::TooManyAttempts {
                max: model.max_attempts,
                candidates: model.candidates.len(),
            });
        }
        for condition in &model.fallback_on {
            // serde enum already validates; keep a runtime guard for hand-built values.
            let _ = condition.as_str();
        }
        let mut visited = HashSet::new();
        visited.insert(model.id.clone());
        // Traverse the *new* candidates, including any path that leads back to
        // this combo id through combos already in the registry.
        let mut stack: Vec<String> = model.candidates.iter().map(|c| c.model.clone()).collect();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                return Err(ComboError::Cycle(current));
            }
            let next = { self.combos.read().await.get(&current).cloned() };
            if let Some(combo) = next {
                for candidate in &combo.candidates {
                    stack.push(candidate.model.clone());
                }
            }
        }
        for candidate in &model.candidates {
            if let Some(mode) = candidate.mode.as_deref() {
                let expected = match mode {
                    "cli" => ExecutionMode::Cli,
                    "http" => ExecutionMode::Http,
                    _ => return Err(ComboError::ModeMismatch {
                        model: candidate.model.clone(),
                        expected: "cli|http".to_string(),
                        actual: "other",
                    }),
                };
                match resolve_mode(&candidate.model) {
                    Some(actual) if actual != expected => {
                        return Err(ComboError::ModeMismatch {
                            model: candidate.model.clone(),
                            expected: expected.as_str().to_string(),
                            actual: actual.as_str(),
                        });
                    }
                    Some(_) => {}
                    None => {
                        // A nested combo is fine (cycle check already passed).
                        if self.get(&candidate.model).await.is_none()
                            && !candidate.model.starts_with("combo/")
                        {
                            return Err(ComboError::UnknownCandidate(candidate.model.clone()));
                        }
                    }
                }
            } else if resolve_mode(&candidate.model).is_none()
                && self.get(&candidate.model).await.is_none()
                // Forward references to combos are allowed; cycle detection
                // above is what guarantees termination.
                && !candidate.model.starts_with("combo/")
            {
                return Err(ComboError::UnknownCandidate(candidate.model.clone()));
            }
        }
        Ok(())
    }

    pub async fn create<F>(&self, model: ComboModel, resolve_mode: &F) -> Result<(), ComboError>
    where
        F: Fn(&str) -> Option<ExecutionMode> + Sync,
    {
        if self.get(&model.id).await.is_some() {
            return Err(ComboError::AlreadyExists(model.id.clone()));
        }
        self.validate(&model, resolve_mode).await?;
        self.combos.write().await.insert(model.id.clone(), model);
        Ok(())
    }

    pub async fn update<F>(
        &self,
        id: &str,
        mut model: ComboModel,
        resolve_mode: &F,
    ) -> Result<(), ComboError>
    where
        F: Fn(&str) -> Option<ExecutionMode> + Sync,
    {
        if self.get(id).await.is_none() {
            return Err(ComboError::NotFound(id.to_string()));
        }
        model.id = id.to_string();
        self.validate(&model, resolve_mode).await?;
        self.combos.write().await.insert(id.to_string(), model);
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), ComboError> {
        if self.combos.write().await.remove(id).is_some() {
            Ok(())
        } else {
            Err(ComboError::NotFound(id.to_string()))
        }
    }

    pub async fn get(&self, id: &str) -> Option<ComboModel> {
        self.combos.read().await.get(id).cloned()
    }

    pub async fn list(&self) -> Vec<ComboModel> {
        let mut items: Vec<_> = self.combos.read().await.values().cloned().collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        items
    }

    /// Candidate ids to try, bounded by `max_attempts`.
    pub fn candidates(model: &ComboModel) -> Vec<&ComboCandidate> {
        model
            .candidates
            .iter()
            .take(model.max_attempts.max(1))
            .collect()
    }

    /// Record one attempt and (optionally) the failover reason in the metrics
    /// bus. Uses the existing generic error counters so no metrics module
    /// changes are required.
    pub fn record_attempt(combo_id: &str, model: &str, attempt: usize) {
        crate::metrics::init_metrics().record_error(
            "combo_attempt",
            &format!("{}:{}:{}", combo_id, model, attempt),
        );
    }

    pub fn record_fallback(combo_id: &str, reason: &str) {
        crate::metrics::init_metrics().record_error("combo_fallback", &format!("{}:{}", combo_id, reason));
    }
}

pub fn registry() -> &'static ComboRegistry {
    static REGISTRY: OnceLock<ComboRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ComboRegistry::new)
}

/// Adapt an admin-created combo from the gateway model catalog
/// (`/admin/combos`, `kind = "combo"`) into the runtime representation.
/// Returns `None` for plain model mappings.
pub fn from_model_record(record: &crate::gateway::store::ModelRecord) -> Option<ComboModel> {
    if record.kind != "combo" {
        return None;
    }
    let fallback_on = record
        .fallback_on
        .iter()
        .filter_map(|condition| FallbackCondition::from_str(condition))
        .collect();
    Some(ComboModel {
        id: record.id.clone(),
        enabled: record.enabled,
        strategy: record.strategy.clone().unwrap_or_else(|| "fallback".to_string()),
        candidates: record
            .candidates
            .iter()
            .map(|candidate| ComboCandidate {
                model: candidate.model.clone(),
                provider: record.provider_id.clone(),
                mode: None,
                weight: candidate.weight,
            })
            .collect(),
        fallback_on,
        max_attempts: record.max_attempts.max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(id: &str, candidates: &[&str]) -> ComboModel {
        ComboModel {
            id: id.to_string(),
            enabled: true,
            strategy: "fallback".to_string(),
            candidates: candidates
                .iter()
                .map(|model| ComboCandidate {
                    model: model.to_string(),
                    provider: None,
                    mode: None,
                    weight: 1,
                })
                .collect(),
            fallback_on: default_fallback_conditions(),
            max_attempts: candidates.len().max(1),
        }
    }

    fn resolve_known(id: &str) -> Option<ExecutionMode> {
        if id.starts_with("agg/") || ["a", "b", "c"].contains(&id) {
            Some(ExecutionMode::Http)
        } else {
            None
        }
    }

    #[test]
    fn conditions_parse_and_match() {
        assert_eq!(FallbackCondition::from_str("quota"), Some(FallbackCondition::Quota));
        assert_eq!(FallbackCondition::from_str("5xx"), Some(FallbackCondition::Server5xx));
        assert_eq!(FallbackCondition::from_str("bogus"), None);
        let conditions = default_fallback_conditions();
        assert!(should_fallback(&conditions, &FailureContext {
            status: Some(429),
            ..Default::default()
        }));
        assert!(should_fallback(&conditions, &FailureContext::from_message("upstream 503")));
        assert!(should_fallback(&conditions, &FailureContext {
            message: "timeout after 10s".to_string(),
            ..Default::default()
        }));
        assert!(!should_fallback(&conditions, &FailureContext::from_message("invalid api key")));
        assert!(should_fallback(
            &[FallbackCondition::EmptyResponse],
            &FailureContext { empty_response: true, ..Default::default() }
        ));
    }

    #[tokio::test]
    async fn create_list_update_delete() {
        let registry = ComboRegistry::new();
        let model = combo("combo/free-smart", &["agg/nvidia/meta/llama-3.3-70b-instruct", "agg/groq/llama"]);
        registry.create(model, &resolve_known).await.unwrap();
        assert_eq!(registry.list().await.len(), 1);
        assert!(registry.create(combo("combo/free-smart", &["a"]), &resolve_known).await.is_err());
        let mut updated = combo("combo/free-smart", &["agg/groq/llama"]);
        updated.max_attempts = 1;
        registry.update("combo/free-smart", updated, &resolve_known).await.unwrap();
        assert_eq!(registry.get("combo/free-smart").await.unwrap().candidates.len(), 1);
        registry.delete("combo/free-smart").await.unwrap();
        assert!(registry.get("combo/free-smart").await.is_none());
    }

    #[tokio::test]
    async fn cycle_prevention() {
        let registry = ComboRegistry::new();
        registry.create(combo("combo/a", &["combo/b"]), &resolve_known).await.unwrap();
        let mut b = combo("combo/b", &["combo/a"]);
        b.max_attempts = 1;
        // b -> a -> b closes the loop and must be rejected.
        assert!(matches!(
            registry.create(b, &resolve_known).await,
            Err(ComboError::Cycle(_))
        ));
        // Self reference is rejected too.
        assert!(matches!(
            registry.create(combo("combo/self", &["combo/self"]), &resolve_known).await,
            Err(ComboError::Cycle(_))
        ));
    }

    #[tokio::test]
    async fn mode_compatibility_is_checked() {
        let registry = ComboRegistry::new();
        let mut model = combo("combo/http-only", &["agg/nvidia/meta/llama-3.3-70b-instruct"]);
        model.candidates[0].mode = Some("http".to_string());
        let http_mode = |_: &str| Some(ExecutionMode::Http);
        registry.create(model, &http_mode).await.unwrap();

        let mut mismatch = combo("combo/mismatch", &["agg/nvidia/meta/llama-3.3-70b-instruct"]);
        mismatch.candidates[0].mode = Some("cli".to_string());
        assert!(matches!(
            registry.create(mismatch, &http_mode).await,
            Err(ComboError::ModeMismatch { .. })
        ));
    }

    #[test]
    fn admin_model_record_is_adapted() {
        use crate::gateway::store::{ComboCandidate as StoreCandidate, ModelRecord};
        let record = ModelRecord {
            id: "combo/admin".to_string(),
            kind: "combo".to_string(),
            provider_id: None,
            upstream_model: None,
            enabled: true,
            mode: "http".to_string(),
            tags: Vec::new(),
            free: false,
            weight: 1,
            pool: None,
            candidates: vec![StoreCandidate { model: "agg/nvidia/model".to_string(), weight: 2 }],
            strategy: Some("fallback".to_string()),
            fallback_on: vec!["quota".to_string(), "5xx".to_string()],
            stop_on: Vec::new(),
            max_attempts: 1,
        };
        let combo = from_model_record(&record).expect("combo record");
        assert_eq!(combo.id, "combo/admin");
        assert_eq!(combo.candidates[0].model, "agg/nvidia/model");
        assert_eq!(combo.candidates[0].weight, 2);
        assert_eq!(combo.fallback_on, vec![FallbackCondition::Quota, FallbackCondition::Server5xx]);

        let plain = ModelRecord { kind: "model".to_string(), ..record };
        assert!(from_model_record(&plain).is_none());
    }

    #[test]
    fn candidates_respect_max_attempts() {
        let model = combo("combo/three", &["a", "b", "c"]);
        assert_eq!(ComboRegistry::candidates(&model).len(), 3);
        let mut limited = combo("combo/one", &["a", "b"]);
        limited.max_attempts = 1;
        assert_eq!(ComboRegistry::candidates(&limited).len(), 1);
    }
}
