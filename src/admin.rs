//! FIX-KEYSTORE / FIX-QUOTA-ACCOUNTING — admin handlers for API keys, usage
//! and quota settings (spec §8.2, §10, §11).
//!
//! Kept in a dedicated module so the provider/model/proxy admin handlers owned
//! by another teammate in `src/api.rs` are never overwritten here.

use crate::config::QuotaLimits;
use crate::error::AppError;
use crate::keystore::{
    bearer, keystore, CreateKeyRequest, PatchKeyRequest, StoreError, UsageFilter,
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use crate::service::ServiceRegistry;
use crate::types::AddKeyRequest;

fn map_err(e: StoreError) -> AppError {
    match e {
        StoreError::NotFound(msg) => AppError::NotFound(msg),
        StoreError::InvalidKey => AppError::KeyInvalid,
        StoreError::Expired => AppError::KeyExpired,
        StoreError::Revoked => AppError::KeyRevoked,
        StoreError::Disabled => AppError::Forbidden("api key disabled".to_string()),
        StoreError::BadRequest(msg) => AppError::BadRequest(msg),
        StoreError::Conflict(msg) => AppError::Conflict(msg),
        StoreError::Db(msg) => AppError::StoreError(msg),
    }
}

pub fn admin_keys_from_env() -> Vec<String> {
    std::env::var("AGENTGATE_ADMIN_API_KEYS")
        .or_else(|_| std::env::var("AGENTGATE_ADMIN_API_KEY"))
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Admin scope check. Honours the env-based admin keys used by the sibling
/// admin backend, and falls back to key-store `admin` scope. When lifecycle
/// auth is disabled (default) this is a no-op to preserve legacy behaviour.
fn require_admin(headers: &HeaderMap, allow_bootstrap: bool) -> Result<(), AppError> {
    if !crate::keystore::auth_enabled() { return Ok(()); }
    let token = bearer(headers).unwrap_or("");
    if admin_keys_from_env().iter().any(|k| k == token) { return Ok(()); }
    let store = keystore().ok_or(AppError::KeyInvalid)?;
    if allow_bootstrap && store.count_keys() == 0 {
        tracing::warn!(event = "key.bootstrap", "No keys in store; allowing first key creation");
        return Ok(());
    }
    let identity = store.authenticate(token).map_err(map_err)?;
    if !identity.has_scope("admin") {
        return Err(AppError::ScopeDenied("admin".to_string()));
    }
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
pub struct UsageQuery {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub key_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct QuotaPatch {
    #[serde(flatten)]
    pub limits: QuotaLimits,
}

// -- keys -------------------------------------------------------------------

pub async fn create_key(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, true)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let created = store.create_key(req).map_err(map_err)?;
    tracing::info!(event = "key.created", key_id = %created.key.id, name = %created.key.name);
    // Plaintext is returned exactly once, in this response only.
    Ok(Json(json!({
        "id": created.key.id,
        "secret": created.secret,
        "key": created.key,
    })))
}

/// Deprecated alias for the former stub `POST /admin/key`.
pub async fn legacy_add_key(
    State(registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(req): Json<AddKeyRequest>,
) -> Result<Json<Value>, AppError> {
    tracing::warn!(event = "admin.deprecated", endpoint = "/admin/key", use_instead = "/admin/keys");
    let legacy = CreateKeyRequest {
        name: req.label.unwrap_or_else(|| "legacy".to_string()),
        allowed_providers: req.provider.map(|p| vec![p]),
        value: Some(req.value),
        ..Default::default()
    };
    create_key(State(registry), headers, Json(legacy)).await
}

pub async fn list_keys(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let keys = store.list_keys().map_err(map_err)?;
    Ok(Json(json!({ "keys": keys, "total": keys.len() })))
}

pub async fn get_key(
    State(_registry): State<Arc<ServiceRegistry>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    Ok(Json(json!(store.get_key(&id).map_err(map_err)?)))
}

pub async fn patch_key_handler(
    State(_registry): State<Arc<ServiceRegistry>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<PatchKeyRequest>,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let meta = store.patch_key(&id, patch).map_err(map_err)?;
    tracing::info!(event = "key.updated", key_id = %id);
    Ok(Json(json!(meta)))
}

pub async fn revoke_key_handler(
    State(_registry): State<Arc<ServiceRegistry>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let meta = store.revoke_key(&id).map_err(map_err)?;
    tracing::warn!(event = "key.revoked", key_id = %id);
    Ok(Json(json!(meta)))
}

pub async fn rotate_key_handler(
    State(_registry): State<Arc<ServiceRegistry>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let rotated = store.rotate_key(&id).map_err(map_err)?;
    tracing::warn!(event = "key.rotated", key_id = %id);
    Ok(Json(json!({
        "id": rotated.key.id,
        "secret": rotated.secret,
        "key": rotated.key,
    })))
}

// -- usage / quotas ---------------------------------------------------------

pub async fn usage(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<UsageQuery>,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let result = store.usage_query(UsageFilter {
        from: q.from, to: q.to, provider: q.provider, model: q.model, key_id: q.key_id,
        limit: q.limit.unwrap_or(100),
    }).map_err(map_err)?;
    Ok(Json(json!(result)))
}

pub async fn quotas_get(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let cfg = store.quota_config();
    Ok(Json(json!({ "defaults": cfg.defaults, "rules": cfg.rules })))
}

pub async fn quotas_patch_defaults(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(patch): Json<QuotaPatch>,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let defaults = store.patch_quota_defaults(patch.limits);
    Ok(Json(json!({ "defaults": defaults })))
}

pub async fn key_quota_get(
    State(_registry): State<Arc<ServiceRegistry>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let meta = store.get_key(&id).map_err(map_err)?;
    Ok(Json(json!({ "key_id": meta.id, "quota": meta.quota })))
}

pub async fn key_quota_patch(
    State(_registry): State<Arc<ServiceRegistry>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<QuotaPatch>,
) -> Result<Json<Value>, AppError> {
    require_admin(&headers, false)?;
    let store = keystore().ok_or_else(|| AppError::StoreError("key store not initialised".to_string()))?;
    let meta = store.patch_key(&id, PatchKeyRequest { quota: Some(patch.limits), ..Default::default() }).map_err(map_err)?;
    Ok(Json(json!({ "key_id": meta.id, "quota": meta.quota })))
}
