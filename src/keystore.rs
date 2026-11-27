//! FIX-KEYSTORE / FIX-QUOTA-ACCOUNTING — SQLite-backed API key lifecycle and
//! per-request usage accounting (spec §7.4, §10, §11, §14).
//!
//! Design invariants:
//! * Only a salted SHA-256 hash of a key secret is persisted. The plaintext is
//!   returned exactly once, at creation/rotation time.
//! * No new external crate is required: `ring` is already in the dependency
//!   graph (rustls) and provides secure random bytes + constant-time compare.
//! * Every write goes through a single `Mutex<rusqlite::Connection>`. Calls are
//!   short local SQLite operations; no async I/O is performed while locked.
//! * Legacy `server.api_keys` static keys keep working when `[server.auth]`
//!   is disabled (the default), so existing deployments are unaffected.

use ring::rand::SecureRandom;
use crate::config::{QuotaConfig, QuotaLimits, QuotaRule};
use crate::metrics::init_metrics;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

const DEFAULT_SCOPES: &[&str] = &["chat.completions", "models.read"];

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    NotFound(String),
    InvalidKey,
    Expired,
    Revoked,
    Disabled,
    Db(String),
    BadRequest(String),
    Conflict(String),
}

impl StoreError {
    pub fn message(&self) -> String {
        match self {
            StoreError::NotFound(id) => format!("not found: {}", id),
            StoreError::InvalidKey => "invalid api key".to_string(),
            StoreError::Expired => "api key expired".to_string(),
            StoreError::Revoked => "api key revoked".to_string(),
            StoreError::Disabled => "api key disabled".to_string(),
            StoreError::Db(e) => format!("key store error: {}", e),
            StoreError::BadRequest(e) => e.clone(),
            StoreError::Conflict(e) => e.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateKeyRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Unix seconds. `null` = never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses_total: Option<u64>,
    /// Per-key quota overlay (see `QuotaLimits`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<QuotaLimits>,
    /// Deprecated wire-compatibility field. Accepted but ignored: the server
    /// always generates its own high-entropy secret (returned once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl Default for CreateKeyRequest {
    fn default() -> Self {
        Self {
            name: "unnamed".to_string(),
            scopes: None,
            allowed_providers: None,
            allowed_models: None,
            expires_at: None,
            max_uses_total: None,
            quota: None,
            value: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PatchKeyRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_providers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Unix seconds; `0` clears expiry. `null` = leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses_total: Option<u64>,
    /// `active` | `disabled` (revocation has its own endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<QuotaLimits>,
}

/// Metadata only — never contains hash, salt, secret or full prefix index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyMeta {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
    pub status: String,
    pub scopes: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub allowed_models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses_total: Option<u64>,
    pub used_count: u64,
    pub quota: QuotaLimits,
}

/// The plaintext key is returned here and NEVER stored anywhere else.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCreated {
    pub key: KeyMeta,
    pub secret: String,
}

/// Authenticated caller context propagated from middleware to handlers.
#[derive(Debug, Clone)]
pub struct KeyIdentity {
    pub id: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub allowed_providers: Vec<String>,
    pub allowed_models: Vec<String>,
    pub used_count: u64,
    pub quota: QuotaLimits,
    /// Legacy static key (no row in the store).
    pub static_key: bool,
}

impl KeyIdentity {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == "*" || s == scope)
    }
    pub fn allows_provider(&self, provider: &str) -> bool {
        self.allowed_providers.is_empty()
            || self.allowed_providers.iter().any(|p| p == provider)
    }
    pub fn allows_model(&self, model: &str) -> bool {
        if self.allowed_models.is_empty() { return true; }
        self.allowed_models.iter().any(|m| {
            if let Some(pattern) = m.strip_suffix("/*") {
                model.starts_with(pattern)
            } else {
                m == model
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct QuotaDenial {
    pub reason: String,
    pub scope: String,
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub request_id: String,
    pub ts: i64,
    pub key_id: String,
    pub provider_id: String,
    pub model: String,
    pub upstream_model: Option<String>,
    pub mode: String,
    pub status: String,
    pub error_class: Option<String>,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub tokens_estimated: bool,
    pub content_in_bytes: u64,
    pub content_out_bytes: u64,
    pub stream: bool,
}

impl Default for UsageRecord {
    fn default() -> Self {
        Self {
            request_id: String::new(), ts: now_unix(), key_id: "anonymous".to_string(),
            provider_id: String::new(), model: String::new(), upstream_model: None,
            mode: String::new(), status: "ok".to_string(), error_class: None,
            latency_ms: 0, input_tokens: 0, output_tokens: 0, total_tokens: 0,
            tokens_estimated: false, content_in_bytes: 0, content_out_bytes: 0,
            stream: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UsageFilter {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub key_id: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageSummary {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub content_in_bytes: u64,
    pub content_out_bytes: u64,
    pub estimated_requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageQueryResult {
    pub summary: UsageSummary,
    pub returned: usize,
    pub rows: Vec<serde_json::Value>,
}

/// RAII concurrency permit. Dropping it releases the in-process slot.
pub struct ConcurrencyPermit {
    key: String,
    store: Arc<KeyStore>,
    released: bool,
}

// FIX-TRANSPORT-BUILD: `Debug` is required by `Result::unwrap_err()` in this
// module's tests. `KeyStore` is intentionally omitted from the output.
impl std::fmt::Debug for ConcurrencyPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcurrencyPermit")
            .field("key", &self.key)
            .field("released", &self.released)
            .finish_non_exhaustive()
    }
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        if self.released { return; }
        self.released = true;
        self.store.release_concurrency(&self.key);
    }
}

// ---------------------------------------------------------------------------
// KeyStore
// ---------------------------------------------------------------------------

pub struct KeyStore {
    conn: Mutex<Connection>,
    quota: RwLock<QuotaConfig>,
    auth_enabled: bool,
    admin_keys: Vec<String>,
    concurrency: dashmap::DashMap<String, u64>,
}

impl KeyStore {
    pub fn open(path: PathBuf, quota: QuotaConfig, auth_enabled: bool, admin_keys: Vec<String>) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Db(e.to_string()))?;
        }
        let conn = Connection::open(&path).map_err(|e| StoreError::Db(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS api_keys (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 prefix TEXT NOT NULL UNIQUE,
                 salt TEXT NOT NULL,
                 hash TEXT NOT NULL,
                 created_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 last_used_at INTEGER,
                 expires_at INTEGER,
                 revoked_at INTEGER,
                 status TEXT NOT NULL,
                 scopes TEXT NOT NULL,
                 allowed_providers TEXT NOT NULL,
                 allowed_models TEXT NOT NULL,
                 max_uses_total INTEGER,
                 used_count INTEGER NOT NULL DEFAULT 0,
                 quota TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS usage_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 request_id TEXT NOT NULL,
                 ts INTEGER NOT NULL,
                 key_id TEXT,
                 provider_id TEXT NOT NULL,
                 model TEXT NOT NULL,
                 upstream_model TEXT,
                 mode TEXT,
                 status TEXT,
                 error_class TEXT,
                 latency_ms INTEGER,
                 input_tokens INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0,
                 total_tokens INTEGER NOT NULL DEFAULT 0,
                 tokens_estimated INTEGER NOT NULL DEFAULT 0,
                 content_in_bytes INTEGER NOT NULL DEFAULT 0,
                 content_out_bytes INTEGER NOT NULL DEFAULT 0,
                 stream INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_usage_key_ts ON usage_events(key_id, ts);
             CREATE INDEX IF NOT EXISTS idx_usage_ts ON usage_events(ts);",
        ).map_err(|e| StoreError::Db(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
            quota: RwLock::new(quota),
            auth_enabled,
            admin_keys,
            concurrency: dashmap::DashMap::new(),
        })
    }

    pub fn auth_enabled(&self) -> bool { self.auth_enabled }
    pub fn admin_keys(&self) -> &[String] { &self.admin_keys }
    pub fn quota_config(&self) -> QuotaConfig {
        self.quota.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- key lifecycle ------------------------------------------------------

    pub fn count_keys(&self) -> u64 {
        self.conn()
            .query_row("SELECT COUNT(*) FROM api_keys", [], |r| r.get::<_, u64>(0))
            .unwrap_or(0)
    }

    pub fn create_key(&self, req: CreateKeyRequest) -> Result<KeyCreated, StoreError> {
        if req.name.trim().is_empty() {
            return Err(StoreError::BadRequest("name must not be empty".to_string()));
        }
        let rng = ring::rand::SystemRandom::new();
        let mut prefix_bytes = [0u8; 8];
        let mut secret_bytes = [0u8; 32];
        let mut salt_bytes = [0u8; 16];
        rng.fill(&mut prefix_bytes).map_err(|_| StoreError::Db("secure random failed".to_string()))?;
        rng.fill(&mut secret_bytes).map_err(|_| StoreError::Db("secure random failed".to_string()))?;
        rng.fill(&mut salt_bytes).map_err(|_| StoreError::Db("secure random failed".to_string()))?;

        let prefix = hex(&prefix_bytes);
        // Always generate our own high-entropy secret. The deprecated `value`
        // field is accepted for wire compatibility but never trusted: storing
        // caller-chosen secrets would weaken the hash-only guarantee.
        let secret = format!("agk_{}_{}", prefix, hex(&secret_bytes));
        let salt = hex(&salt_bytes);
        let hash = hash_secret(&salt, &secret, &prefix);
        let scopes = req.scopes.unwrap_or_else(|| DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect());
        let now = now_unix();
        let id = format!("key_{}", uuid::Uuid::new_v4().simple());

        self.conn().execute(
            "INSERT INTO api_keys
             (id,name,prefix,salt,hash,created_at,updated_at,last_used_at,expires_at,revoked_at,
              status,scopes,allowed_providers,allowed_models,max_uses_total,used_count,quota)
             VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,?8,NULL,'active',?9,?10,?11,?12,0,?13)",
            params![
                id, req.name, prefix, salt, hash, now, now, req.expires_at,
                serde_json::to_string(&scopes).unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(&req.allowed_providers.unwrap_or_default()).unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(&req.allowed_models.unwrap_or_default()).unwrap_or_else(|_| "[]".to_string()),
                req.max_uses_total,
                serde_json::to_string(&req.quota.unwrap_or_default()).unwrap_or_else(|_| "{}".to_string()),
            ],
        ).map_err(|e| StoreError::Db(e.to_string()))?;
        let meta = self.get_key(&id)?;
        Ok(KeyCreated { key: meta, secret })
    }

    pub fn get_key(&self, id: &str) -> Result<KeyMeta, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id,name,prefix,created_at,updated_at,last_used_at,expires_at,revoked_at,status,scopes,allowed_providers,allowed_models,max_uses_total,used_count,quota FROM api_keys WHERE id=?1")
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let row = stmt.query_row(params![id], |r| Self::row_to_meta(r))
            .optional()
            .map_err(|e| StoreError::Db(e.to_string()))?;
        row.ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    pub fn list_keys(&self) -> Result<Vec<KeyMeta>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id,name,prefix,created_at,updated_at,last_used_at,expires_at,revoked_at,status,scopes,allowed_providers,allowed_models,max_uses_total,used_count,quota FROM api_keys ORDER BY created_at DESC")
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt.query_map([], |r| Self::row_to_meta(r))
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows { out.push(r.map_err(|e| StoreError::Db(e.to_string()))?); }
        Ok(out)
    }

    fn row_to_meta(r: &rusqlite::Row<'_>) -> Result<KeyMeta, rusqlite::Error> {
        Ok(KeyMeta {
            id: r.get(0)?,
            name: r.get(1)?,
            prefix: r.get(2)?,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
            last_used_at: r.get(5)?,
            expires_at: r.get(6)?,
            revoked_at: r.get(7)?,
            status: r.get(8)?,
            scopes: parse_json_vec(r.get::<_, String>(9)?),
            allowed_providers: parse_json_vec(r.get::<_, String>(10)?),
            allowed_models: parse_json_vec(r.get::<_, String>(11)?),
            max_uses_total: r.get(12)?,
            used_count: r.get(13)?,
            quota: serde_json::from_str(&r.get::<_, String>(14)?).unwrap_or_default(),
        })
    }

    pub fn patch_key(&self, id: &str, patch: PatchKeyRequest) -> Result<KeyMeta, StoreError> {
        let meta = self.get_key(id)?;
        let conn = self.conn();
        let mut fields: Vec<String> = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        macro_rules! add {
            ($col:expr, $val:expr) => {{ fields.push(format!("{}=?{}", $col, values.len() + 1)); values.push(rusqlite::types::Value::from($val)); }};
        }
        if let Some(v) = patch.name { add!("name", v); }
        if let Some(v) = patch.scopes { add!("scopes", serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())); }
        if let Some(v) = patch.allowed_providers { add!("allowed_providers", serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())); }
        if let Some(v) = patch.allowed_models { add!("allowed_models", serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())); }
        if let Some(v) = patch.expires_at { add!("expires_at", if v == 0 { None } else { Some(v) }); }
        if let Some(v) = patch.max_uses_total { add!("max_uses_total", Some(v as i64)); }
        if let Some(v) = patch.status {
            if v != "active" && v != "disabled" {
                return Err(StoreError::BadRequest("status must be active or disabled".to_string()));
            }
            add!("status", v);
        }
        if let Some(v) = patch.quota { add!("quota", serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string())); }
        if fields.is_empty() { return Ok(meta); }
        fields.push(format!("updated_at=?{}", values.len() + 1));
        values.push(rusqlite::types::Value::Integer(now_unix()));
        values.push(rusqlite::types::Value::Text(id.to_string()));
        let sql = format!("UPDATE api_keys SET {} WHERE id=?{}", fields.join(","), values.len());
        conn.execute(&sql, rusqlite::params_from_iter(values))
            .map_err(|e| StoreError::Db(e.to_string()))?;
        drop(conn);
        self.get_key(id)
    }

    pub fn revoke_key(&self, id: &str) -> Result<KeyMeta, StoreError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE api_keys SET status='revoked', revoked_at=?1, updated_at=?1 WHERE id=?2",
            params![now_unix(), id],
        ).map_err(|e| StoreError::Db(e.to_string()))?;
        drop(conn);
        self.get_key(id)
    }

    /// Rotate the secret in place. Old secret stops working immediately.
    pub fn rotate_key(&self, id: &str) -> Result<KeyCreated, StoreError> {
        let meta = self.get_key(id)?;
        let rng = ring::rand::SystemRandom::new();
        let mut secret_bytes = [0u8; 32];
        let mut salt_bytes = [0u8; 16];
        rng.fill(&mut secret_bytes).map_err(|_| StoreError::Db("secure random failed".to_string()))?;
        rng.fill(&mut salt_bytes).map_err(|_| StoreError::Db("secure random failed".to_string()))?;
        let secret = format!("agk_{}_{}", meta.prefix, hex(&secret_bytes));
        let salt = hex(&salt_bytes);
        let hash = hash_secret(&salt, &secret, &meta.prefix);
        let conn = self.conn();
        conn.execute(
            "UPDATE api_keys SET salt=?1, hash=?2, status='active', revoked_at=NULL, updated_at=?3 WHERE id=?4",
            params![salt, hash, now_unix(), id],
        ).map_err(|e| StoreError::Db(e.to_string()))?;
        drop(conn);
        Ok(KeyCreated { key: self.get_key(id)?, secret })
    }

    // -- authentication -----------------------------------------------------

    pub fn authenticate(&self, token: &str) -> Result<KeyIdentity, StoreError> {
        let prefix = token.strip_prefix("agk_")
            .and_then(|rest| rest.split('_').next())
            .ok_or(StoreError::InvalidKey)?;
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id,name,prefix,salt,hash,status,expires_at,revoked_at,scopes,allowed_providers,allowed_models,used_count,quota FROM api_keys WHERE prefix=?1")
            .map_err(|e| StoreError::Db(e.to_string()))?;
        let found = stmt.query_row(params![prefix], |r| {
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?,
                r.get::<_, Option<i64>>(6)?, r.get::<_, Option<i64>>(7)?,
                r.get::<_, String>(8)?, r.get::<_, String>(9)?, r.get::<_, String>(10)?,
                r.get::<_, u64>(11)?, r.get::<_, String>(12)?,
            ))
        }).optional().map_err(|e| StoreError::Db(e.to_string()))?;
        let (id, name, stored_prefix, salt, expected_hash, status, expires_at, revoked_at,
             scopes, allowed_providers, allowed_models, used_count, quota_json) =
            found.ok_or(StoreError::InvalidKey)?;
        drop(stmt);
        drop(conn);

        // Constant-time hash comparison prevents prefix-based key enumeration.
        // Implemented locally so no internal/deprecated ring API is needed.
        let actual_hash = hash_secret(&salt, token, &stored_prefix);
        if !constant_time_eq(actual_hash.as_bytes(), expected_hash.as_bytes()) {
            return Err(StoreError::InvalidKey);
        }
        let now = now_unix();
        if revoked_at.is_some() || status == "revoked" { return Err(StoreError::Revoked); }
        if status == "disabled" { return Err(StoreError::Disabled); }
        if status != "active" { return Err(StoreError::InvalidKey); }
        if let Some(exp) = expires_at { if exp <= now { return Err(StoreError::Expired); } }

        Ok(KeyIdentity {
            id, name,
            scopes: parse_json_vec(scopes),
            allowed_providers: parse_json_vec(allowed_providers),
            allowed_models: parse_json_vec(allowed_models),
            used_count,
            quota: serde_json::from_str(&quota_json).unwrap_or_default(),
            static_key: false,
        })
    }

    /// Synthetic identity for unauthenticated requests when
    /// `[quota] enforce_anonymous = true`.
    pub fn anonymous_identity() -> KeyIdentity {
        KeyIdentity {
            id: "anonymous".to_string(),
            name: "anonymous".to_string(),
            scopes: DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect(),
            allowed_providers: Vec::new(),
            allowed_models: Vec::new(),
            used_count: 0,
            quota: QuotaLimits::default(),
            static_key: true,
        }
    }

    /// Synthetic identity for legacy static keys configured in TOML.
    pub fn static_identity(token: &str, admin: bool) -> KeyIdentity {
        let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
        let id = format!("static-{}", &hex(digest.as_ref())[..12]);
        let scopes = if admin {
            vec!["*".to_string()]
        } else {
            DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect()
        };
        KeyIdentity {
            id, name: "static".to_string(), scopes,
            allowed_providers: Vec::new(), allowed_models: Vec::new(),
            used_count: 0, quota: QuotaLimits::default(), static_key: true,
        }
    }

    // -- quota enforcement --------------------------------------------------

    fn effective_limits(&self, identity: &KeyIdentity, provider: &str, model: &str) -> QuotaLimits {
        let guard = self.quota.read().unwrap_or_else(|e| e.into_inner());
        let mut limits = guard.defaults.clone();
        let rules: Vec<QuotaRule> = guard.rules.clone();
        drop(guard);
        // Most-specific rule wins when several rules match.
        let mut best: Option<(&QuotaRule, u8)> = None;
        for rule in &rules {
            let provider_match = rule.provider.as_deref().map(|p| p == provider).unwrap_or(false);
            let model_match = rule.model.as_deref().map(|m| m == model).unwrap_or(false);
            if (rule.provider.is_some() && !provider_match) || (rule.model.is_some() && !model_match) { continue; }
            let specificity = provider_match as u8 + model_match as u8;
            if best.map(|(_, s)| specificity > s).unwrap_or(true) { best = Some((rule, specificity)); }
        }
        if let Some((rule, _)) = best { limits = limits.overlay(&rule.limits); }
        limits.overlay(&identity.quota)
    }

    /// Check all configured limits and reserve one concurrency slot.
    pub fn check_quota(
        self: &Arc<Self>,
        identity: &KeyIdentity,
        provider: &str,
        model: &str,
        body_bytes: u64,
    ) -> Result<Option<ConcurrencyPermit>, QuotaDenial> {
        let limits = self.effective_limits(identity, provider, model);
        let now = now_unix();
        let day_start = utc_day_start(now);

        let deny = |reason: &str, scope: &str, reset_at: Option<i64>| -> QuotaDenial {
            init_metrics().record_quota_denied(scope, reason);
            QuotaDenial { reason: reason.to_string(), scope: scope.to_string(), reset_at }
        };

        if let Some(max) = limits.max_body_bytes {
            if body_bytes > max { return Err(deny("max_body_bytes", "request", None)); }
        }
        let conn = self.conn();
        // FIX-QUOTA-ACCOUNTING: counters are scoped to key + provider + model,
        // so a limit on one model does not consume another model's budget.
        if let Some(max) = limits.requests_per_minute {
            let n: u64 = conn.query_row(
                "SELECT COUNT(*) FROM usage_events WHERE key_id=?1 AND provider_id=?2 AND model=?3 AND ts>=?4",
                params![identity.id, provider, model, now - 60], |r| r.get(0),
            ).unwrap_or(0);
            if n >= max { return Err(deny("requests_per_minute", "key/provider/model", Some(now + 60))); }
        }
        if let Some(max) = limits.requests_per_day {
            let n: u64 = conn.query_row(
                "SELECT COUNT(*) FROM usage_events WHERE key_id=?1 AND provider_id=?2 AND model=?3 AND ts>=?4",
                params![identity.id, provider, model, day_start], |r| r.get(0),
            ).unwrap_or(0);
            if n >= max { return Err(deny("requests_per_day", "key/provider/model", Some(utc_day_start(now + 86_400)))); }
        }
        if let Some(max) = limits.input_tokens_per_day {
            let n: u64 = conn.query_row(
                "SELECT COALESCE(SUM(input_tokens),0) FROM usage_events WHERE key_id=?1 AND provider_id=?2 AND model=?3 AND ts>=?4",
                params![identity.id, provider, model, day_start], |r| r.get(0),
            ).unwrap_or(0);
            if n >= max { return Err(deny("input_tokens_per_day", "key/provider/model", Some(utc_day_start(now + 86_400)))); }
        }
        if let Some(max) = limits.output_tokens_per_day {
            let n: u64 = conn.query_row(
                "SELECT COALESCE(SUM(output_tokens),0) FROM usage_events WHERE key_id=?1 AND provider_id=?2 AND model=?3 AND ts>=?4",
                params![identity.id, provider, model, day_start], |r| r.get(0),
            ).unwrap_or(0);
            if n >= max { return Err(deny("output_tokens_per_day", "key/provider/model", Some(utc_day_start(now + 86_400)))); }
        }
        if let Some(max) = limits.total_tokens_per_day {
            let n: u64 = conn.query_row(
                "SELECT COALESCE(SUM(total_tokens),0) FROM usage_events WHERE key_id=?1 AND provider_id=?2 AND model=?3 AND ts>=?4",
                params![identity.id, provider, model, day_start], |r| r.get(0),
            ).unwrap_or(0);
            if n >= max { return Err(deny("total_tokens_per_day", "key/provider/model", Some(utc_day_start(now + 86_400)))); }
        }
        if let Some(max) = limits.max_uses_total {
            if identity.used_count >= max { return Err(deny("max_uses_total", "key", None)); }
        }
        if let Some(max) = limits.max_uses_per_day {
            let n: u64 = conn.query_row(
                "SELECT COUNT(*) FROM usage_events WHERE key_id=?1 AND ts>=?2",
                params![identity.id, day_start], |r| r.get(0),
            ).unwrap_or(0);
            if n >= max { return Err(deny("max_uses_per_day", "key", Some(utc_day_start(now + 86_400)))); }
        }
        drop(conn);

        let permit = if let Some(max) = limits.max_concurrent_requests {
            let slot = format!("{}|{}|{}", identity.id, provider, model);
            let mut current = self.concurrency.entry(slot.clone()).or_insert(0);
            if *current >= max {
                drop(current);
                return Err(deny("max_concurrent_requests", "provider/model", None));
            }
            *current += 1;
            drop(current);
            Some(ConcurrencyPermit { key: slot, store: Arc::clone(self), released: false })
        } else {
            None
        };
        init_metrics().record_key_usage(&identity.id);
        Ok(permit)
    }

    fn release_concurrency(&self, slot: &str) {
        if let Some(mut v) = self.concurrency.get_mut(slot) {
            if *v > 0 { *v -= 1; }
        }
    }

    // -- usage accounting ---------------------------------------------------

    pub fn record_usage(&self, rec: UsageRecord) {
        let conn = self.conn();
        let _ = conn.execute(
            "INSERT INTO usage_events
             (request_id,ts,key_id,provider_id,model,upstream_model,mode,status,error_class,
              latency_ms,input_tokens,output_tokens,total_tokens,tokens_estimated,
              content_in_bytes,content_out_bytes,stream)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                rec.request_id, rec.ts, rec.key_id, rec.provider_id, rec.model, rec.upstream_model,
                rec.mode, rec.status, rec.error_class, rec.latency_ms as i64,
                rec.input_tokens as i64, rec.output_tokens as i64, rec.total_tokens as i64,
                rec.tokens_estimated as i64, rec.content_in_bytes as i64,
                rec.content_out_bytes as i64, rec.stream as i64,
            ],
        );
        // Static keys have no row; only lifecycle keys get counters updated.
        if rec.key_id.starts_with("key_") {
            let _ = conn.execute(
                "UPDATE api_keys SET used_count=used_count+1, last_used_at=?1, updated_at=?1 WHERE id=?2",
                params![rec.ts, rec.key_id],
            );
        }
        drop(conn);

        let m = init_metrics();
        m.record_quota_tokens("in", &rec.provider_id, &rec.model, &rec.key_id, rec.input_tokens);
        m.record_quota_tokens("out", &rec.provider_id, &rec.model, &rec.key_id, rec.output_tokens);
        m.record_quota_bytes("in", &rec.provider_id, &rec.model, &rec.key_id, rec.content_in_bytes);
        m.record_quota_bytes("out", &rec.provider_id, &rec.model, &rec.key_id, rec.content_out_bytes);
    }

    pub fn usage_query(&self, filter: UsageFilter) -> Result<UsageQueryResult, StoreError> {
        let conn = self.conn();
        let mut sql = String::from(
            "SELECT request_id,ts,key_id,provider_id,model,mode,status,latency_ms,
                    input_tokens,output_tokens,total_tokens,tokens_estimated,
                    content_in_bytes,content_out_bytes,stream
             FROM usage_events WHERE 1=1",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        macro_rules! push {
            ($clause:expr, $val:expr) => {{
                sql.push_str($clause);
                args.push(rusqlite::types::Value::from($val));
            }};
        }
        // Build the filter clause once so both the row query and the summary
        // aggregate use identical predicates. Previously the summary ignored
        // all filters (always global totals), which misled `/admin/usage`
        // consumers that filtered by key_id/provider/model.
        let mut where_sql = String::new();
        macro_rules! filterpush {
            ($clause:expr, $val:expr) => {{
                where_sql.push_str($clause);
                args.push(rusqlite::types::Value::from($val));
            }};
        }
        if let Some(v) = filter.from { filterpush!(" AND ts>=?", v); }
        if let Some(v) = filter.to { filterpush!(" AND ts<=?", v); }
        if let Some(v) = &filter.provider { filterpush!(" AND provider_id=?", v.to_string()); }
        if let Some(v) = &filter.model { filterpush!(" AND model=?", v.to_string()); }
        if let Some(v) = &filter.key_id { filterpush!(" AND key_id=?", v.to_string()); }
        sql.push_str(&where_sql);
        let summary_sql = format!(
            "SELECT COUNT(*),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0),
                    COALESCE(SUM(total_tokens),0),COALESCE(SUM(content_in_bytes),0),
                    COALESCE(SUM(content_out_bytes),0),COALESCE(SUM(tokens_estimated),0)
             FROM usage_events WHERE 1=1{}",
            where_sql
        );
        let limited = filter.limit.max(1).min(1000);
        sql.push_str(" ORDER BY ts DESC LIMIT ");
        sql.push_str(&limited.to_string());

        let mut stmt = conn.prepare(&sql).map_err(|e| StoreError::Db(e.to_string()))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.clone()), |r| {
            Ok(serde_json::json!({
                "request_id": r.get::<_, String>(0)?,
                "ts": r.get::<_, i64>(1)?,
                "key_id": r.get::<_, String>(2)?,
                "provider_id": r.get::<_, String>(3)?,
                "model": r.get::<_, String>(4)?,
                "mode": r.get::<_, String>(5)?,
                "status": r.get::<_, String>(6)?,
                "latency_ms": r.get::<_, i64>(7)?,
                "input_tokens": r.get::<_, i64>(8)?,
                "output_tokens": r.get::<_, i64>(9)?,
                "total_tokens": r.get::<_, i64>(10)?,
                "tokens_estimated": r.get::<_, i64>(11)? != 0,
                "content_in_bytes": r.get::<_, i64>(12)?,
                "content_out_bytes": r.get::<_, i64>(13)?,
                "stream": r.get::<_, i64>(14)? != 0,
            }))
        }).map_err(|e| StoreError::Db(e.to_string()))?;
        let mut out = Vec::new();
        for r in rows { out.push(r.map_err(|e| StoreError::Db(e.to_string()))?); }
        drop(stmt);

        let (requests, it, ot, tt, cib, cob, est): (u64, u64, u64, u64, u64, u64, u64) = conn.query_row(
            &summary_sql,
            rusqlite::params_from_iter(args), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).unwrap_or_default();
        Ok(UsageQueryResult {
            summary: UsageSummary {
                requests, input_tokens: it, output_tokens: ot, total_tokens: tt,
                content_in_bytes: cib, content_out_bytes: cob, estimated_requests: est,
            },
            returned: out.len(),
            rows: out,
        })
    }

    // -- quota config CRUD --------------------------------------------------

    pub fn quota_defaults(&self) -> QuotaLimits {
        self.quota.read().unwrap_or_else(|e| e.into_inner()).defaults.clone()
    }

    /// Runtime-only override so no restart is required. The TOML config file
    /// remains the source of truth on the next restart (admin PATCH is ephemeral
    /// by design here; persisted per-key overrides live in the key row).
    pub fn patch_quota_defaults(&self, patch: QuotaLimits) -> QuotaLimits {
        let mut guard = self.quota.write().unwrap_or_else(|e| e.into_inner());
        guard.defaults = guard.defaults.overlay(&patch);
        guard.defaults.clone()
    }
}

// ---------------------------------------------------------------------------
// Global instance + helpers
// ---------------------------------------------------------------------------

static KEYSTORE: OnceLock<Arc<KeyStore>> = OnceLock::new();

pub fn init_keystore(store: KeyStore) -> Arc<KeyStore> {
    let arc = Arc::new(store);
    let _ = KEYSTORE.set(Arc::clone(&arc));
    arc
}

pub fn keystore() -> Option<&'static Arc<KeyStore>> { KEYSTORE.get() }

pub fn auth_enabled() -> bool { keystore().map(|s| s.auth_enabled()).unwrap_or(false) }

/// Data-plane auth layer for lifecycle keys. Bridge sessions keep their own
/// per-session tokens, and health/root remain public.
pub async fn auth_middleware(
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let path = req.uri().path().to_string();
    if path == "/health" || path == "/" || path.starts_with("/_bridge") {
        return next.run(req).await;
    }
    let token = bearer(req.headers()).unwrap_or("").to_string();
    let store = match keystore() {
        Some(s) => s,
        None => return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let unauthorized = |code: &str, message: &str| {
        axum::response::Response::builder()
            .status(axum::http::StatusCode::UNAUTHORIZED)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(serde_json::json!({
                "error": { "message": message, "type": "invalid_request_error", "code": code }
            }).to_string()))
            .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
    };

    if path.starts_with("/admin") {
        if crate::admin::admin_keys_from_env().iter().any(|k| k == &token)
            || store.admin_keys().iter().any(|k| k == &token)
        {
            return next.run(req).await;
        }
        if path == "/admin/keys" && req.method() == axum::http::Method::POST && store.count_keys() == 0 {
            tracing::warn!(event = "key.bootstrap", "No keys in store; allowing first key creation");
            return next.run(req).await;
        }
        return match store.authenticate(&token) {
            Ok(id) if id.has_scope("admin") => {
                req.extensions_mut().insert(id);
                next.run(req).await
            }
            Ok(_) => axum::response::Response::builder()
                .status(axum::http::StatusCode::FORBIDDEN)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(r#"{"error":{"message":"scope_denied: admin","type":"invalid_request_error","code":"scope_denied"}}"#))
                .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()),
            Err(e) => unauthorized(match &e {
                StoreError::Expired => "api_key_expired",
                StoreError::Revoked => "api_key_revoked",
                _ => "invalid_api_key",
            }, &e.message()),
        };
    }

    match store.authenticate(&token) {
        Ok(id) => {
            if path.starts_with("/v1/") && !id.has_scope("chat.completions") && !id.has_scope("*") {
                return axum::response::Response::builder()
                    .status(axum::http::StatusCode::FORBIDDEN)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(r#"{"error":{"message":"scope_denied: chat.completions","type":"invalid_request_error","code":"scope_denied"}}"#))
                    .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            req.extensions_mut().insert(id);
            next.run(req).await
        }
        Err(e) => unauthorized(match &e {
            StoreError::Expired => "api_key_expired",
            StoreError::Revoked => "api_key_revoked",
            StoreError::Disabled => "api_key_disabled",
            _ => "invalid_api_key",
        }, &e.message()),
    }
}

/// Default path mirrors the quota state path convention in service.rs.
pub fn default_key_store_path() -> PathBuf {
    std::env::var("AGENTGATE_KEY_STORE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
                .or_else(|_| std::env::var("USERPROFILE").map(|h| PathBuf::from(h).join(".config")))
                .unwrap_or_else(|_| PathBuf::from("."));
            base.join("agentgate").join("keys.sqlite3")
        })
}

/// Resolve a bearer token into a key identity. Used by middleware and handlers
/// (handlers may be called directly in unit tests, bypassing middleware).
pub fn identity_from_headers(headers: &axum::http::HeaderMap) -> Option<KeyIdentity> {
    if let Some(id) = headers.get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        // Legacy static-key / no-auth mode: no lifecycle identity exists, so
        // limits are not applied unless `[quota] enforce_anonymous = true`.
        if !auth_enabled() {
            return None;
        }
        if let Some(store) = keystore() {
            if let Ok(identity) = store.authenticate(id) { return Some(identity); }
        }
    }
    None
}

pub fn bearer(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers.get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

fn now_unix() -> i64 { chrono::Utc::now().timestamp() }

fn utc_day_start(ts: i64) -> i64 {
    use chrono::TimeZone;
    chrono::Utc.timestamp_opt(ts, 0).single()
        .map(|dt| dt.date_naive().and_hms_opt(0, 0, 0).map(|d| d.and_utc().timestamp()).unwrap_or(ts))
        .unwrap_or(ts)
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

/// Compare two equal-length byte strings without early exit.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) { diff |= x ^ y; }
    diff == 0
}

fn hash_secret(salt_hex: &str, secret: &str, prefix: &str) -> String {
    let mut input = Vec::with_capacity(salt_hex.len() + secret.len() + prefix.len() + 2);
    input.extend_from_slice(salt_hex.as_bytes());
    input.push(b':');
    input.extend_from_slice(prefix.as_bytes());
    input.push(b':');
    input.extend_from_slice(secret.as_bytes());
    hex(ring::digest::digest(&ring::digest::SHA256, &input).as_ref())
}

fn parse_json_vec(s: String) -> Vec<String> {
    serde_json::from_str(&s).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, KeyStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::open(
            dir.path().join("keys.sqlite3"), QuotaConfig::default(), false, Vec::new(),
        ).unwrap();
        (dir, store)
    }

    #[test]
    fn creates_key_stores_only_hash_and_authenticates() {
        let (_dir, store) = temp_store();
        let created = store.create_key(CreateKeyRequest {
            name: "prod".to_string(), ..Default::default()
        }).unwrap();
        assert!(created.secret.starts_with("agk_"));
        let meta = store.get_key(&created.key.id).unwrap();
        assert_ne!(meta.prefix, created.secret);
        let conn = store.conn();
        let hash: String = conn.query_row(
            "SELECT hash FROM api_keys WHERE id=?1", params![created.key.id], |r| r.get(0)
        ).unwrap();
        drop(conn);
        assert!(!hash.contains(&created.secret));
        let identity = store.authenticate(&created.secret).unwrap();
        assert_eq!(identity.id, created.key.id);
        assert!(identity.has_scope("chat.completions"));
        assert!(store.authenticate("agk_bad_bad").is_err());
    }

    #[test]
    fn expiry_revocation_and_rotation() {
        let (_dir, store) = temp_store();
        let created = store.create_key(CreateKeyRequest {
            name: "short".to_string(), expires_at: Some(now_unix() - 1), ..Default::default()
        }).unwrap();
        assert_eq!(store.authenticate(&created.secret).unwrap_err(), StoreError::Expired);

        let created = store.create_key(CreateKeyRequest { name: "revoke".to_string(), ..Default::default() }).unwrap();
        store.revoke_key(&created.key.id).unwrap();
        assert_eq!(store.authenticate(&created.secret).unwrap_err(), StoreError::Revoked);

        let rotated = store.rotate_key(&created.key.id).unwrap();
        assert_ne!(rotated.secret, created.secret);
        assert!(store.authenticate(&created.secret).is_err());
        assert!(store.authenticate(&rotated.secret).is_ok());
    }

    #[test]
    fn scope_and_model_allowlists() {
        let (_dir, store) = temp_store();
        let created = store.create_key(CreateKeyRequest {
            name: "scoped".to_string(),
            scopes: Some(vec!["chat.completions".to_string()]),
            allowed_models: Some(vec!["agg/*".to_string()]),
            ..Default::default()
        }).unwrap();
        let id = store.authenticate(&created.secret).unwrap();
        assert!(id.allows_model("agg/opencode/beam-free"));
        assert!(!id.allows_model("ollama/llama3"));
        assert!(!id.has_scope("admin"));
    }

    #[test]
    fn quota_limits_are_recorded_and_enforced() {
        let (_dir, store) = temp_store();
        let store = Arc::new(store);
        let created = store.create_key(CreateKeyRequest {
            name: "limited".to_string(),
            quota: Some(QuotaLimits { requests_per_day: Some(1), ..Default::default() }),
            ..Default::default()
        }).unwrap();
        let identity = store.authenticate(&created.secret).unwrap();
        assert!(store.check_quota(&identity, "p", "m", 10).is_ok());
        store.record_usage(UsageRecord {
            key_id: identity.id.clone(), provider_id: "p".to_string(), model: "m".to_string(),
            ..Default::default()
        });
        let err = store.check_quota(&identity, "p", "m", 10).unwrap_err();
        assert_eq!(err.reason, "requests_per_day");
    }

    #[test]
    fn body_limit_and_concurrency_permit() {
        let (_dir, store) = temp_store();
        let store = Arc::new(store);
        let created = store.create_key(CreateKeyRequest {
            name: "body".to_string(),
            quota: Some(QuotaLimits { max_body_bytes: Some(8), max_concurrent_requests: Some(1), ..Default::default() }),
            ..Default::default()
        }).unwrap();
        let identity = store.authenticate(&created.secret).unwrap();
        assert_eq!(store.check_quota(&identity, "p", "m", 9).unwrap_err().reason, "max_body_bytes");
        let permit = store.check_quota(&identity, "p", "m", 5).unwrap().unwrap();
        assert_eq!(store.check_quota(&identity, "p", "m", 5).unwrap_err().reason, "max_concurrent_requests");
        drop(permit);
        assert!(store.check_quota(&identity, "p", "m", 5).is_ok());
    }

    #[test]
    fn usage_query_reports_totals() {
        let (_dir, store) = temp_store();
        store.record_usage(UsageRecord {
            key_id: "key_test".to_string(), input_tokens: 10, output_tokens: 5,
            total_tokens: 15, content_in_bytes: 100, content_out_bytes: 50,
            ..Default::default()
        });
        let result = store.usage_query(UsageFilter::default()).unwrap();
        assert_eq!(result.summary.requests, 1);
        assert_eq!(result.summary.input_tokens, 10);
        assert_eq!(result.summary.content_out_bytes, 50);
    }
}
