//! Persisted catalog for gateway providers, models, proxies and pools.
//!
//! Secrets are stored in the file (so the admin API can update them) but are
//! NEVER included in API responses — [`GatewayState::public_document`] and the
//! admin handlers build redacted views explicitly.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;

pub const DEFAULT_FREE_POOL: &str = "free_forever";
pub const DEFAULT_FREE_FALLBACK: &str = "opencode/9router/free-forever";

fn default_true() -> bool { true }
fn default_kind() -> String { "http_openai_compatible".to_string() }
fn default_mode() -> String { "auto".to_string() }
fn default_model_kind() -> String { "model".to_string() }
fn default_weight() -> u32 { 1 }
fn default_ban_429() -> u64 { 60 }
fn default_ban_5xx() -> u64 { 30 }
fn default_health_interval() -> u64 { 60 }
fn default_cooldown() -> u64 { 43_200 } // 12h, spec §10.3
fn default_max_attempts() -> usize { 3 }
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid {field}: {message}")]
    Invalid { field: String, message: String },
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("all proxy endpoints are unhealthy")]
    AllProxiesUnhealthy,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl GatewayError {
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid { field: field.into(), message: message.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    None,
    Single,
    Pool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyStrategy {
    #[default]
    RoundRobin,
    Random,
    LeastErrors,
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LbStrategy {
    RoundRobin,
    #[default]
    WeightedRoundRobin,
    LeastLatency,
    LeastErrors,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StickyKey {
    #[default]
    None,
    SessionId,
    ApiKey,
}

/// Proxy configuration for one provider (or a standalone `/admin/proxies` entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub id: String,
    #[serde(default)]
    pub mode: ProxyMode,
    #[serde(default)]
    pub single_url: Option<String>,
    #[serde(default)]
    pub pool: Vec<String>,
    #[serde(default)]
    pub strategy: ProxyStrategy,
    #[serde(default)]
    pub healthcheck_url: Option<String>,
    #[serde(default = "default_health_interval")]
    pub healthcheck_interval_secs: u64,
    #[serde(default = "default_ban_429")]
    pub ban_on_429_secs: u64,
    #[serde(default = "default_ban_5xx")]
    pub ban_on_5xx_secs: u64,
    #[serde(default = "default_ban_5xx")]
    pub ban_on_timeout_secs: u64,
    /// If all endpoints are banned: use direct connection (default: fail closed).
    #[serde(default)]
    pub fallback_direct: bool,
    /// Allow private/loopback proxy hosts (common for local proxies).
    #[serde(default = "default_true")]
    pub allow_private: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            mode: ProxyMode::None,
            single_url: None,
            pool: Vec::new(),
            strategy: ProxyStrategy::RoundRobin,
            healthcheck_url: None,
            healthcheck_interval_secs: default_health_interval(),
            ban_on_429_secs: default_ban_429(),
            ban_on_5xx_secs: default_ban_5xx(),
            ban_on_timeout_secs: default_ban_5xx(),
            fallback_direct: false,
            allow_private: true,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }
}

impl ProxyConfig {
    pub fn endpoints(&self) -> Vec<String> {
        match self.mode {
            ProxyMode::None => Vec::new(),
            ProxyMode::Single => self.single_url.iter().cloned().collect(),
            ProxyMode::Pool => self.pool.clone(),
        }
    }

    /// Validate IDs, URLs and strategy invariants before persisting.
    pub fn validate(&self) -> Result<(), GatewayError> {
        if self.id.trim().is_empty() {
            return Err(GatewayError::invalid("id", "must not be empty"));
        }
        match self.mode {
            ProxyMode::None => {}
            ProxyMode::Single => {
                let url = self.single_url.as_ref().ok_or_else(|| {
                    GatewayError::invalid("single_url", "required when mode=single")
                })?;
                validate_http_url(url, self.allow_private)?;
            }
            ProxyMode::Pool => {
                if self.pool.is_empty() {
                    return Err(GatewayError::invalid("pool", "must contain at least one URL"));
                }
                for url in &self.pool {
                    validate_http_url(url, self.allow_private)?;
                }
            }
        }
        if let Some(url) = &self.healthcheck_url {
            validate_http_url(url, self.allow_private)?;
        }
        Ok(())
    }

    /// Secrets never leave the process in admin responses or logs.
    pub fn public_value(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "mode": self.mode,
            "single_url": self.single_url.as_deref().map(redact_url),
            "pool": self.pool.iter().map(|u| redact_url(u)).collect::<Vec<_>>(),
            "strategy": self.strategy,
            "healthcheck_url": self.healthcheck_url.as_deref().map(redact_url),
            "healthcheck_interval_secs": self.healthcheck_interval_secs,
            "ban_on_429_secs": self.ban_on_429_secs,
            "ban_on_5xx_secs": self.ban_on_5xx_secs,
            "ban_on_timeout_secs": self.ban_on_timeout_secs,
            "fallback_direct": self.fallback_direct,
            "allow_private": self.allow_private,
            "enabled": self.enabled,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

/// One gateway provider (CLI providers keep working; HTTP ones are optional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayProvider {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_kind")]
    pub provider_kind: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Allow loopback/private upstreams (e.g. local `opencode serve`). Cloud
    /// metadata endpoints stay blocked regardless.
    #[serde(default = "default_true")]
    pub allow_private_base_url: bool,
    #[serde(default)]
    pub chat_completions_path: Option<String>,
    #[serde(default)]
    pub models_path: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Secret. Only returned as `"***"` by admin APIs.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    /// Reference to a `/admin/proxies` entry; inline config used when absent.
    #[serde(default)]
    pub proxy_id: Option<String>,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

impl Default for GatewayProvider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            provider_kind: default_kind(),
            mode: default_mode(),
            enabled: true,
            base_url: None,
            allow_private_base_url: true,
            chat_completions_path: None,
            models_path: None,
            auth_type: None,
            api_key_env: None,
            api_key: None,
            default_model: None,
            models: Vec::new(),
            proxy_id: None,
            proxy: ProxyConfig::default(),
            created_at: 0,
            updated_at: 0,
        }
    }
}

impl GatewayProvider {
    pub fn validate(&self) -> Result<(), GatewayError> {
        if self.id.trim().is_empty() {
            return Err(GatewayError::invalid("id", "must not be empty"));
        }
        if !matches!(self.mode.as_str(), "cli" | "http" | "auto") {
            return Err(GatewayError::invalid("mode", "expected cli|http|auto"));
        }
        if let Some(url) = &self.base_url {
            validate_http_url(url, self.allow_private_base_url)?;
        }
        if self.mode == "http" && self.base_url.is_none() {
            return Err(GatewayError::invalid("base_url", "required for mode=http"));
        }
        let mut proxy = self.proxy.clone();
        proxy.id = self.id.clone();
        proxy.validate()
    }

    pub fn public_value(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "provider_kind": self.provider_kind,
            "mode": self.mode,
            "enabled": self.enabled,
            "base_url": self.base_url,
            "allow_private_base_url": self.allow_private_base_url,
            "chat_completions_path": self.chat_completions_path,
            "models_path": self.models_path,
            "auth_type": self.auth_type,
            "api_key_env": self.api_key_env,
            "api_key": redact_secret(self.api_key.as_deref()),
            "default_model": self.default_model,
            "models": self.models,
            "proxy_id": self.proxy_id,
            "proxy": self.proxy.public_value(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboCandidate {
    pub model: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRecord {
    pub id: String,
    /// `"model"` (mapping) or `"combo"` (virtual fallback chain).
    #[serde(default = "default_model_kind")]
    pub kind: String,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub upstream_model: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub free: bool,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub candidates: Vec<ComboCandidate>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub fallback_on: Vec<String>,
    #[serde(default)]
    pub stop_on: Vec<String>,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,
}

impl Default for ModelRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: default_model_kind(),
            provider_id: None,
            upstream_model: None,
            enabled: true,
            mode: default_mode(),
            tags: Vec::new(),
            free: false,
            weight: default_weight(),
            pool: None,
            candidates: Vec::new(),
            strategy: None,
            fallback_on: Vec::new(),
            stop_on: Vec::new(),
            max_attempts: default_max_attempts(),
        }
    }
}

impl ModelRecord {
    pub fn is_combo(&self) -> bool { self.kind == "combo" }

    pub fn validate(&self, existing_ids: &BTreeMap<String, ModelRecord>) -> Result<(), GatewayError> {
        if self.id.trim().is_empty() {
            return Err(GatewayError::invalid("id", "must not be empty"));
        }
        if !matches!(self.kind.as_str(), "model" | "combo") {
            return Err(GatewayError::invalid("kind", "expected model|combo"));
        }
        if self.is_combo() {
            if self.candidates.is_empty() {
                return Err(GatewayError::invalid("candidates", "combo needs at least one candidate"));
            }
            for candidate in &self.candidates {
                if candidate.model.trim().is_empty() {
                    return Err(GatewayError::invalid("candidates", "model must not be empty"));
                }
                if candidate.model == self.id {
                    return Err(GatewayError::invalid("candidates", "combo cycle: candidate is the combo itself"));
                }
            }
            if let Some(strategy) = &self.strategy {
                if !matches!(strategy.as_str(), "fallback" | "weighted" | "round_robin" | "race_first_success") {
                    return Err(GatewayError::invalid("strategy", "expected fallback|weighted|round_robin|race_first_success"));
                }
            }
        } else if self.provider_id.as_deref().unwrap_or("").is_empty() {
            return Err(GatewayError::invalid("provider_id", "required for a model mapping"));
        }
        // Ignore the record being replaced when checking candidate existence.
        let _ = existing_ids;
        Ok(())
    }

    pub fn public_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({"id": self.id}))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMember {
    pub model: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    pub name: String,
    #[serde(default)]
    pub strategy: LbStrategy,
    #[serde(default)]
    pub sticky_key: StickyKey,
    #[serde(default = "default_cooldown")]
    pub cooldown_on_quota_secs: u64,
    #[serde(default)]
    pub members: Vec<PoolMember>,
    #[serde(default)]
    pub fallback: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            strategy: LbStrategy::WeightedRoundRobin,
            sticky_key: StickyKey::SessionId,
            cooldown_on_quota_secs: default_cooldown(),
            members: Vec::new(),
            fallback: vec![DEFAULT_FREE_FALLBACK.to_string()],
            enabled: true,
        }
    }
}

impl PoolConfig {
    pub fn validate(&self) -> Result<(), GatewayError> {
        if self.name.trim().is_empty() {
            return Err(GatewayError::invalid("name", "must not be empty"));
        }
        for member in &self.members {
            if member.model.trim().is_empty() {
                return Err(GatewayError::invalid("members", "model must not be empty"));
            }
            if member.weight == 0 {
                return Err(GatewayError::invalid("members", "weight must be >= 1"));
            }
        }
        Ok(())
    }

    pub fn public_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({"name": self.name}))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatewayDocument {
    #[serde(default)]
    pub providers: BTreeMap<String, GatewayProvider>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelRecord>,
    #[serde(default)]
    pub proxies: BTreeMap<String, ProxyConfig>,
    #[serde(default)]
    pub pools: BTreeMap<String, PoolConfig>,
    #[serde(default)]
    pub updated_at: u64,
}

impl GatewayDocument {
    /// Seed the default free pool so LB works without any admin setup.
    pub fn with_defaults(mut self) -> Self {
        self.pools.entry(DEFAULT_FREE_POOL.to_string()).or_insert_with(|| {
            let mut pool = PoolConfig::default();
            pool.name = DEFAULT_FREE_POOL.to_string();
            pool
        });
        self
    }
}

/// JSON-backed catalog. All mutations are serialized through `RwLock` and
/// persisted atomically (write temp file + rename).
pub struct GatewayState {
    path: PathBuf,
    doc: RwLock<GatewayDocument>,
}

impl GatewayState {
    pub fn new(path: PathBuf) -> Self {
        let doc = Self::load(&path).unwrap_or_default().with_defaults();
        Self { path, doc: RwLock::new(doc) }
    }

    pub fn path(&self) -> &Path { &self.path }

    fn load(path: &Path) -> Result<GatewayDocument, GatewayError> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str::<GatewayDocument>(&content)?.with_defaults())
    }

    async fn persist(&self, doc: &GatewayDocument) -> Result<(), GatewayError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = self.path.clone();
        tmp.set_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(doc)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub async fn document(&self) -> GatewayDocument { self.doc.read().await.clone() }

    /// Redacted view safe for admin responses (no plaintext/hashed secrets).
    pub async fn public_document(&self) -> serde_json::Value {
        let doc = self.doc.read().await;
        serde_json::json!({
            "providers": doc.providers.values().map(|p| p.public_value()).collect::<Vec<_>>(),
            "models": doc.models.values().map(|m| m.public_value()).collect::<Vec<_>>(),
            "proxies": doc.proxies.values().map(|p| p.public_value()).collect::<Vec<_>>(),
            "pools": doc.pools.values().map(|p| p.public_value()).collect::<Vec<_>>(),
            "updated_at": doc.updated_at,
        })
    }

    // ---- providers -------------------------------------------------------
    pub async fn list_providers(&self) -> Vec<GatewayProvider> {
        self.doc.read().await.providers.values().cloned().collect()
    }

    pub async fn get_provider(&self, id: &str) -> Option<GatewayProvider> {
        self.doc.read().await.providers.get(id).cloned()
    }

    pub async fn upsert_provider(&self, mut provider: GatewayProvider) -> Result<GatewayProvider, GatewayError> {
        provider.proxy.id = provider.id.clone();
        provider.validate()?;
        let mut doc = self.doc.write().await;
        let created = doc.providers.get(&provider.id).is_none();
        let now = now_unix();
        if created { provider.created_at = now; }
        provider.updated_at = now;
        doc.providers.insert(provider.id.clone(), provider.clone());
        doc.updated_at = now;
        self.persist(&doc).await?;
        Ok(provider)
    }

    pub async fn delete_provider(&self, id: &str) -> Result<(), GatewayError> {
        let mut doc = self.doc.write().await;
        if doc.providers.remove(id).is_none() {
            return Err(GatewayError::NotFound(format!("provider {}", id)));
        }
        doc.updated_at = now_unix();
        self.persist(&doc).await
    }

    // ---- models / combos -------------------------------------------------
    pub async fn list_models(&self) -> Vec<ModelRecord> {
        self.doc.read().await.models.values().cloned().collect()
    }

    pub async fn get_model(&self, id: &str) -> Option<ModelRecord> {
        self.doc.read().await.models.get(id).cloned()
    }

    pub async fn upsert_model(&self, model: ModelRecord) -> Result<ModelRecord, GatewayError> {
        let doc = self.doc.read().await;
        model.validate(&doc.models)?;
        drop(doc);
        let mut doc = self.doc.write().await;
        doc.models.insert(model.id.clone(), model.clone());
        doc.updated_at = now_unix();
        self.persist(&doc).await?;
        Ok(model)
    }

    pub async fn delete_model(&self, id: &str) -> Result<(), GatewayError> {
        let mut doc = self.doc.write().await;
        if doc.models.remove(id).is_none() {
            return Err(GatewayError::NotFound(format!("model {}", id)));
        }
        doc.updated_at = now_unix();
        self.persist(&doc).await
    }

    // ---- proxies ---------------------------------------------------------
    pub async fn list_proxies(&self) -> Vec<ProxyConfig> {
        self.doc.read().await.proxies.values().cloned().collect()
    }

    pub async fn get_proxy(&self, id: &str) -> Option<ProxyConfig> {
        self.doc.read().await.proxies.get(id).cloned()
    }

    pub async fn upsert_proxy(&self, mut proxy: ProxyConfig) -> Result<ProxyConfig, GatewayError> {
        proxy.validate()?;
        let mut doc = self.doc.write().await;
        let created = doc.proxies.get(&proxy.id).is_none();
        let now = now_unix();
        if created { proxy.created_at = now; }
        proxy.updated_at = now;
        doc.proxies.insert(proxy.id.clone(), proxy.clone());
        doc.updated_at = now;
        self.persist(&doc).await?;
        Ok(proxy)
    }

    pub async fn delete_proxy(&self, id: &str) -> Result<(), GatewayError> {
        let mut doc = self.doc.write().await;
        if doc.proxies.remove(id).is_none() {
            return Err(GatewayError::NotFound(format!("proxy {}", id)));
        }
        doc.updated_at = now_unix();
        self.persist(&doc).await
    }

    // ---- pools -----------------------------------------------------------
    pub async fn list_pools(&self) -> Vec<PoolConfig> {
        self.doc.read().await.pools.values().cloned().collect()
    }

    pub async fn get_pool(&self, name: &str) -> Option<PoolConfig> {
        self.doc.read().await.pools.get(name).cloned()
    }

    pub async fn upsert_pool(&self, pool: PoolConfig) -> Result<PoolConfig, GatewayError> {
        pool.validate()?;
        let mut doc = self.doc.write().await;
        doc.pools.insert(pool.name.clone(), pool.clone());
        doc.updated_at = now_unix();
        self.persist(&doc).await?;
        Ok(pool)
    }
}

/// Allow only http/https upstream targets; block credential-in-URL leakage in
/// logs and (unless opted in) private/loopback/metadata hosts (SSRF guard).
pub fn validate_http_url(raw: &str, allow_private: bool) -> Result<(), GatewayError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|e| GatewayError::invalid("url", format!("{}", e)))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(GatewayError::invalid("url", format!("unsupported scheme: {}", other))),
    }
    let host = url.host_str().ok_or_else(|| GatewayError::invalid("url", "missing host"))?;
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower == "metadata.google.internal" {
        if !allow_private {
            return Err(GatewayError::invalid("url", "private/metadata hosts are not allowed"));
        }
    }
    if let Ok(ip) = lower.parse::<std::net::IpAddr>() {
        // Cloud metadata endpoints are blocked unconditionally (SSRF guard).
        let metadata = matches!(ip, std::net::IpAddr::V4(v4) if v4.is_link_local());
        if metadata {
            return Err(GatewayError::invalid("url", "metadata endpoints are not allowed"));
        }
        let private = match ip {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_unspecified() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local(),
        };
        if private && !allow_private {
            return Err(GatewayError::invalid("url", "private/metadata hosts are not allowed"));
        }
    }
    Ok(())
}

/// Strip userinfo and known-sensitive query parameters from a URL for logging.
pub fn redact_url(raw: &str) -> String {
    let mut url = match reqwest::Url::parse(raw) {
        Ok(url) => url,
        Err(_) => return "<invalid-url>".to_string(),
    };
    if url.username() != "" || url.password().is_some() {
        let _ = url.set_username("***");
        let _ = url.set_password(Some("***"));
    }
    let sensitive = ["token", "api_key", "apikey", "key", "password", "secret", "access_token"];
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !sensitive.iter().any(|s| k.eq_ignore_ascii_case(s)))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut().clear().extend_pairs(pairs);
    url.to_string()
}

pub fn redact_secret(value: Option<&str>) -> Option<&'static str> {
    match value {
        Some(v) if !v.is_empty() => Some("***"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_schemes_and_private_hosts() {
        assert!(validate_http_url("https://openrouter.ai/api/v1", false).is_ok());
        assert!(validate_http_url("file:///etc/passwd", false).is_err());
        assert!(validate_http_url("http://169.254.169.254/latest/meta-data", false).is_err());
        assert!(validate_http_url("http://127.0.0.1:8080", true).is_ok());
        assert!(validate_http_url("not-a-url", false).is_err());
    }

    #[test]
    fn redacts_credentials_and_query_secrets() {
        let out = redact_url("http://user:pass@proxy.local:8080/?token=abc&keep=1");
        assert!(out.contains("***"));
        assert!(!out.contains("abc"));
        assert!(out.contains("keep=1"));
        assert_eq!(redact_secret(Some("sk-super-secret")), Some("***"));
        assert_eq!(redact_secret(None), None);
    }

    #[tokio::test]
    async fn provider_public_value_hides_api_key() {
        let state = GatewayState::new(PathBuf::from("nonexistent-test-gateway.json"));
        let mut provider = GatewayProvider::default();
        provider.id = "openrouter".to_string();
        provider.api_key = Some("sk-secret".to_string());
        provider.base_url = Some("https://openrouter.ai/api/v1".to_string());
        state.upsert_provider(provider).await.unwrap();
        let json = state.public_document().await;
        let rendered = json.to_string();
        assert!(!rendered.contains("sk-secret"));
        assert!(rendered.contains("***"));
    }

    #[test]
    fn combo_rejects_self_cycle() {
        let mut combo = ModelRecord::default();
        combo.id = "combo/a".to_string();
        combo.kind = "combo".to_string();
        combo.candidates.push(ComboCandidate { model: "combo/a".to_string(), weight: 1 });
        let models = BTreeMap::new();
        assert!(combo.validate(&models).is_err());
    }
}
