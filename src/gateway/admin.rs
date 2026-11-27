//! Admin handlers for providers, models/combos, proxies and pools.
//!
//! All handlers require an admin-scoped key (`AGENTGATE_ADMIN_API_KEYS` or
//! `AGENTGATE_ADMIN_API_KEY`, comma separated) and never return secrets.
//! Legacy `/admin/provider`, `/admin/key` and `/admin/proxy` aliases are wired
//! in `api.rs` with `Deprecation` response headers.

use crate::gateway::proxy::{self, ProxyEndpoint, ProxyOutcome};
use crate::provider::combo::{self, ComboError, FallbackCondition};
use crate::service::ExecutionMode;
use crate::gateway::store::{
    GatewayError, GatewayProvider, GatewayState, ModelRecord, PoolConfig, ProxyConfig,
};
use crate::gateway::state;
use crate::service::ServiceRegistry;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEPRECATION_HEADER: &str = "deprecation";
const DEPRECATION_VALUE: &str = "true";

/// Admin scope check. Fail closed: when no admin key is configured, access is
/// denied unless `AGENTGATE_ADMIN_ALLOW_NO_AUTH=1` is explicitly set for local
/// development.
pub fn authorize(headers: &HeaderMap) -> Result<(), Response> {
    let raw = std::env::var("AGENTGATE_ADMIN_API_KEYS")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("AGENTGATE_ADMIN_API_KEY").ok().filter(|v| !v.trim().is_empty()))
        .or_else(|| std::env::var("AGENTGATE_ADMIN_KEY").ok().filter(|v| !v.trim().is_empty()));

    let keys: Vec<String> = match raw {
        Some(value) => value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        None => Vec::new(),
    };
    // Also accept static bootstrap keys configured under `[auth] admin_keys`.
    // Keystore middleware already treats these as admin-scoped; keep the
    // gateway admin plane consistent with the key lifecycle plane.
    let keys = if keys.is_empty() {
        crate::keystore::keystore()
            .map(|s| s.admin_keys().to_vec())
            .unwrap_or_default()
    } else {
        keys
    };
    let provided = headers
        .get("x-agentgate-admin-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string)
        })
        .unwrap_or_default();

    if keys.is_empty() {
        let dev_allowed = std::env::var("AGENTGATE_ADMIN_ALLOW_NO_AUTH")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if dev_allowed {
            tracing::warn!(event = "admin.auth.dev_open", "Admin API is open (AGENTGATE_ADMIN_ALLOW_NO_AUTH=1)");
            return Ok(());
        }
        return Err(error_response(
            StatusCode::FORBIDDEN,
            "admin_scope_required",
            "Set AGENTGATE_ADMIN_API_KEYS to enable the admin API",
        ));
    }
    if keys.iter().any(|key| constant_time_eq(key.as_bytes(), provided.as_bytes())) {
        Ok(())
    } else {
        Err(error_response(StatusCode::UNAUTHORIZED, "unauthorized", "Invalid admin key"))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) { diff |= x ^ y; }
    diff == 0
}

pub fn deprecation_headers(response: &mut Response, new_path: &str) {
    if let Ok(value) = HeaderValue::from_str(new_path) {
        response.headers_mut().insert(DEPRECATION_HEADER, HeaderValue::from_static(DEPRECATION_VALUE));
        response.headers_mut().insert("x-agentgate-deprecated-for", value);
    }
}

pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(serde_json::json!({
        "error": { "code": code, "message": message, "type": "invalid_request_error" }
    }))).into_response()
}

fn gateway_error(error: GatewayError) -> Response {
    match error {
        GatewayError::NotFound(message) => error_response(StatusCode::NOT_FOUND, "not_found", &message),
        GatewayError::Invalid { field, message } => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request", &format!("{}: {}", field, message))
        }
        GatewayError::Conflict(message) => error_response(StatusCode::CONFLICT, "conflict", &message),
        GatewayError::AllProxiesUnhealthy => {
            error_response(StatusCode::SERVICE_UNAVAILABLE, "all_proxies_unhealthy", "all proxy endpoints are banned")
        }
        other => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", &other.to_string()),
    }
}

fn ok(value: Value) -> Response { (StatusCode::OK, Json(value)).into_response() }

fn merge_patch<T: Serialize + DeserializeOwned>(current: &T, patch: &Value) -> Result<T, GatewayError> {
    let mut base = serde_json::to_value(current)?;
    merge_values(&mut base, patch);
    serde_json::from_value(base).map_err(GatewayError::Json)
}

fn merge_values(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base_map), Value::Object(patch_map)) => {
            for (key, value) in patch_map {
                match base_map.get_mut(key) {
                    Some(existing) => merge_values(existing, value),
                    None => { base_map.insert(key.clone(), value.clone()); }
                }
            }
        }
        (base, patch) => { *base = patch.clone(); }
    }
}

fn body_object(body: Value) -> Result<Map<String, Value>, GatewayError> {
    body.as_object()
        .cloned()
        .ok_or_else(|| GatewayError::invalid("body", "expected a JSON object"))
}

fn require_string(body: &Map<String, Value>, key: &str) -> Result<String, GatewayError> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| GatewayError::invalid(key, "required non-empty string"))
}

fn from_body<T: DeserializeOwned>(body: Value) -> Result<T, GatewayError> {
    serde_json::from_value(body).map_err(GatewayError::Json)
}

fn log_admin(action: &str, detail: &str) {
    tracing::info!(event = "admin.action", action, detail, "Admin action");
}

/// Map a combo record to the runtime registry's model.
///
/// `max_attempts` is clamped to the candidate count (the registry rejects
/// larger values) and unknown fallback conditions are reported to the admin
/// caller instead of being silently dropped.
fn to_runtime_combo(record: &ModelRecord) -> Result<combo::ComboModel, Response> {
    use crate::gateway::store::ComboCandidate as StoreCandidate;
    let map_candidate = |candidate: &StoreCandidate| combo::ComboCandidate {
        model: candidate.model.clone(),
        provider: None,
        mode: None,
        weight: candidate.weight.max(1),
    };
    let mut fallback_on = Vec::new();
    for condition in &record.fallback_on {
        match FallbackCondition::from_str(condition) {
            Some(parsed) => fallback_on.push(parsed),
            None => {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    &format!("unknown fallback condition: {}", condition),
                ));
            }
        }
    }
    Ok(combo::ComboModel {
        id: record.id.clone(),
        enabled: record.enabled,
        strategy: record.strategy.clone().unwrap_or_else(|| "fallback".to_string()),
        candidates: record.candidates.iter().map(map_candidate).collect(),
        fallback_on,
        max_attempts: record.max_attempts.min(record.candidates.len()).max(1),
    })
}

fn combo_error(error: ComboError) -> Response {
    let status = match error {
        ComboError::NotFound(_) => StatusCode::NOT_FOUND,
        ComboError::AlreadyExists(_) => StatusCode::CONFLICT,
        _ => StatusCode::BAD_REQUEST,
    };
    error_response(status, "invalid_combo", &error.to_string())
}

/// Resolver used by combo validation. Models in the gateway catalog carry
/// their execution mode; unregistered CLI-style ids are treated as CLI so
/// ordinary opencode models can be used as candidates.
async fn combo_mode_resolver() -> impl Fn(&str) -> Option<ExecutionMode> {
    let doc = state().document().await;
    let modes: std::collections::HashMap<String, ExecutionMode> = doc
        .models
        .iter()
        .map(|(id, record)| {
            let mode = if record.mode.eq_ignore_ascii_case("http") {
                ExecutionMode::Http
            } else {
                ExecutionMode::Cli
            };
            (id.clone(), mode)
        })
        .collect();
    move |model: &str| {
        modes.get(model).copied().or_else(|| {
            if model.trim().is_empty() { None } else { Some(ExecutionMode::Cli) }
        })
    }
}

// ---- providers -----------------------------------------------------------

pub async fn list_providers(headers: HeaderMap, State(_registry): State<Arc<ServiceRegistry>>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    ok(Value::Array(state().list_providers().await.iter().map(|p| p.public_value()).collect()))
}

pub async fn get_provider(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match state().get_provider(&id).await {
        Some(provider) => ok(provider.public_value()),
        None => gateway_error(GatewayError::NotFound(format!("provider {}", id))),
    }
}

pub async fn add_provider(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let id = match require_string(&body_object(body.clone()).unwrap_or_default(), "id") {
        Ok(id) => id,
        Err(e) => return gateway_error(e),
    };
    let provider: GatewayProvider = match from_body(body) {
        Ok(provider) => provider,
        Err(e) => return gateway_error(e),
    };
    match state().upsert_provider(provider).await {
        Ok(saved) => {
            log_admin("provider.upsert", &id);
            ok(saved.public_value())
        }
        Err(e) => gateway_error(e),
    }
}

pub async fn update_provider(headers: HeaderMap, Path(id): Path<String>, Json(patch): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let store: Arc<GatewayState> = state();
    let current = match store.get_provider(&id).await {
        Some(provider) => provider,
        None => return gateway_error(GatewayError::NotFound(format!("provider {}", id))),
    };
    let updated: GatewayProvider = match merge_patch(&current, &patch) {
        Ok(provider) => provider,
        Err(e) => return gateway_error(e),
    };
    match store.upsert_provider(updated).await {
        Ok(saved) => { log_admin("provider.patch", &id); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

pub async fn delete_provider(headers: HeaderMap, Path(id): Path<String>, Query(params): Query<BTreeMap<String, String>>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let store = state();
    let hard = params.get("hard").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
    if hard {
        match store.delete_provider(&id).await {
            Ok(()) => { log_admin("provider.delete", &id); ok(serde_json::json!({"status":"ok","deleted":id})) }
            Err(e) => gateway_error(e),
        }
    } else {
        let current = match store.get_provider(&id).await {
            Some(provider) => provider,
            None => return gateway_error(GatewayError::NotFound(format!("provider {}", id))),
        };
        let mut disabled = current.clone();
        disabled.enabled = false;
        match store.upsert_provider(disabled).await {
            Ok(_) => { log_admin("provider.disable", &id); ok(serde_json::json!({"status":"ok","disabled":id})) }
            Err(e) => gateway_error(e),
        }
    }
}

/// Strip the trailing `/test` segment from a wildcard-captured id.
///
/// The admin detail routes use `/admin/<kind>/*id` so ids containing `/`
/// (e.g. `agg/nvidia/<model>`) are addressable. Axum 0.7 cannot register
/// `*id` together with a separate `:id/test` route, so the `/test` action
/// shares the wildcard route and `id` arrives as `"<id>/test"`.
fn strip_test_suffix(id: &str) -> &str {
    id.strip_suffix("/test").unwrap_or(id)
}

/// Smoke test for a provider: HTTP `/models` probe or CLI availability check.
pub async fn test_provider(
    headers: HeaderMap,
    State(registry): State<Arc<ServiceRegistry>>,
    Path(raw_id): Path<String>,
) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let id = strip_test_suffix(&raw_id).to_string();
    let provider = match state().get_provider(&id).await {
        Some(provider) => provider,
        None => return gateway_error(GatewayError::NotFound(format!("provider {}", id))),
    };
    let started = Instant::now();
    let (status, detail) = if provider.mode == "http" {
        match probe_http_provider(&provider).await {
            Ok((status_code, latency)) => {
                (if status_code.is_success() { "ok" } else { "error" },
                 serde_json::json!({"http_status": status_code.as_u16(), "latency_ms": latency.as_millis()}))
            }
            Err(message) => ("error", serde_json::json!({"message": message})),
        }
    } else {
        let available = registry.is_provider_available(&id).await;
        (if available { "ok" } else { "error" }, serde_json::json!({"cli_available": available}))
    };
    log_admin("provider.test", &id);
    ok(serde_json::json!({
        "id": id,
        "status": status,
        "detail": detail,
        "elapsed_ms": started.elapsed().as_millis(),
        "base_url": provider.base_url,
    }))
}

async fn probe_http_provider(provider: &GatewayProvider) -> Result<(reqwest::StatusCode, Duration), String> {
    let base = provider.base_url.as_ref().ok_or("base_url is not configured")?;
    let path = provider.models_path.as_deref().unwrap_or("/models");
    let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
    crate::gateway::store::validate_http_url(&url, provider.allow_private_base_url).map_err(|e| e.to_string())?;
    let key = provider
        .api_key
        .clone()
        .or_else(|| provider.api_key_env.as_ref().and_then(|name| std::env::var(name).ok()))
        .unwrap_or_default();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(&url);
    if !key.is_empty() {
        request = request.header("Authorization", format!("Bearer {}", key));
    }
    let started = Instant::now();
    let response = request.send().await.map_err(|e| format!("upstream unreachable: {}", e))?;
    Ok((response.status(), started.elapsed()))
}

// ---- models / combos -----------------------------------------------------

pub async fn list_models(headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    ok(Value::Array(state().list_models().await.iter().map(|m| m.public_value()).collect()))
}

pub async fn add_model(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let object = match body_object(body.clone()) { Ok(object) => object, Err(e) => return gateway_error(e) };
    let id = match require_string(&object, "id") { Ok(id) => id, Err(e) => return gateway_error(e) };
    let model: ModelRecord = match from_body(body) { Ok(model) => model, Err(e) => return gateway_error(e) };
    match state().upsert_model(model).await {
        Ok(saved) => { log_admin("model.upsert", &id); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

pub async fn get_model(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match state().get_model(&id).await {
        Some(model) => ok(model.public_value()),
        None => gateway_error(GatewayError::NotFound(format!("model {}", id))),
    }
}

pub async fn update_model(headers: HeaderMap, Path(id): Path<String>, Json(patch): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let store = state();
    let current = match store.get_model(&id).await {
        Some(model) => model,
        None => return gateway_error(GatewayError::NotFound(format!("model {}", id))),
    };
    let updated: ModelRecord = match merge_patch(&current, &patch) { Ok(model) => model, Err(e) => return gateway_error(e) };
    match store.upsert_model(updated).await {
        Ok(saved) => { log_admin("model.patch", &id); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

pub async fn delete_model(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match state().delete_model(&id).await {
        Ok(()) => { log_admin("model.delete", &id); ok(serde_json::json!({"status":"ok","deleted":id})) }
        Err(e) => gateway_error(e),
    }
}

pub async fn list_combos(headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    // The persisted gateway catalog is authoritative; the in-process runtime
    // registry is only a mirror kept in sync by the write handlers.
    let combos: Vec<Value> = state().list_models().await.into_iter()
        .filter(|model| model.is_combo())
        .map(|model| model.public_value())
        .collect();
    ok(Value::Array(combos))
}

pub async fn get_combo(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match state().get_model(&id).await {
        Some(model) if model.is_combo() => ok(model.public_value()),
        _ => gateway_error(GatewayError::NotFound(format!("combo {}", id))),
    }
}

pub async fn add_combo(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let mut object = match body_object(body.clone()) { Ok(object) => object, Err(e) => return gateway_error(e) };
    object.insert("kind".to_string(), Value::String("combo".to_string()));
    let id = match require_string(&object, "id") { Ok(id) => id, Err(e) => return gateway_error(e) };
    let record: ModelRecord = match from_body(Value::Object(object)) { Ok(combo) => combo, Err(e) => return gateway_error(e) };
    let runtime_model = match to_runtime_combo(&record) { Ok(model) => model, Err(response) => return response };
    let exists_in_catalog = state().get_model(&id).await.map(|model| model.is_combo()).unwrap_or(false);
    if exists_in_catalog || combo::registry().get(&id).await.is_some() {
        return combo_error(ComboError::AlreadyExists(id));
    }
    let resolver = combo_mode_resolver().await;
    if let Err(e) = combo::registry().validate(&runtime_model, &resolver).await {
        return combo_error(e);
    }
    match state().upsert_model(record.clone()).await {
        Ok(_) => {
            if let Err(e) = combo::registry().create(runtime_model, &resolver).await {
                let _ = state().delete_model(&id).await; // roll back
                return combo_error(e);
            }
            log_admin("combo.create", &id);
            ok(record.public_value())
        }
        Err(e) => gateway_error(e),
    }
}

pub async fn update_combo(headers: HeaderMap, Path(id): Path<String>, Json(patch): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let store = state();
    let current = match store.get_model(&id).await {
        Some(model) if model.is_combo() => model,
        _ => return gateway_error(GatewayError::NotFound(format!("combo {}", id))),
    };
    let updated: ModelRecord = match merge_patch(&current, &patch) { Ok(model) => model, Err(e) => return gateway_error(e) };
    let runtime_model = match to_runtime_combo(&updated) { Ok(model) => model, Err(response) => return response };
    let resolver = combo_mode_resolver().await;
    if let Err(e) = combo::registry().validate(&runtime_model, &resolver).await {
        return combo_error(e);
    }
    match store.upsert_model(updated.clone()).await {
        Ok(_) => {
            if let Err(e) = combo::registry().update(&id, runtime_model, &resolver).await {
                let _ = store.upsert_model(current).await; // roll back
                return combo_error(e);
            }
            log_admin("combo.patch", &id);
            ok(updated.public_value())
        }
        Err(e) => gateway_error(e),
    }
}

pub async fn delete_combo(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match combo::registry().delete(&id).await {
        Ok(()) => {
            let _ = state().delete_model(&id).await;
            log_admin("combo.delete", &id);
            ok(serde_json::json!({"status":"ok","deleted":id}))
        }
        Err(e) => combo_error(e),
    }
}

/// Optional catalog sync: fetch `/models` from an HTTP provider and store the
/// returned ids as model mappings (best effort, never overwrites enabled state).
pub async fn sync_models(
    headers: HeaderMap,
    Query(params): Query<BTreeMap<String, String>>,
) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let provider_id = match params.get("provider_id") {
        Some(id) => id.clone(),
        None => return error_response(StatusCode::BAD_REQUEST, "invalid_request", "provider_id is required"),
    };
    let provider = match state().get_provider(&provider_id).await {
        Some(provider) => provider,
        None => return gateway_error(GatewayError::NotFound(format!("provider {}", provider_id))),
    };
    let base = match provider.base_url.as_deref() {
        Some(base) => base,
        None => return error_response(StatusCode::BAD_REQUEST, "invalid_request", "provider has no base_url"),
    };
    let path = provider.models_path.as_deref().unwrap_or("/models");
    let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
    if let Err(e) = crate::gateway::store::validate_http_url(&url, provider.allow_private_base_url) { return gateway_error(e); }
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
        Ok(client) => client,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", &e.to_string()),
    };
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, "upstream_error", &e.to_string()),
    };
    let payload: Value = match response.json().await {
        Ok(payload) => payload,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, "upstream_error", &e.to_string()),
    };
    let ids: Vec<String> = payload.get("data")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();
    let store = state();
    let mut synced = 0usize;
    for upstream_model in ids {
        let id = format!("{}/{}", provider_id, upstream_model);
        if store.get_model(&id).await.is_some() { continue; }
        let model = ModelRecord {
            id: id.clone(),
            provider_id: Some(provider_id.clone()),
            upstream_model: Some(upstream_model),
            mode: "http".to_string(),
            ..ModelRecord::default()
        };
        if store.upsert_model(model).await.is_ok() { synced += 1; }
    }
    ok(serde_json::json!({"status":"ok","provider_id":provider_id,"synced":synced}))
}

// ---- proxies -------------------------------------------------------------

pub async fn list_proxies(headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    ok(Value::Array(state().list_proxies().await.iter().map(|p| p.public_value()).collect()))
}

pub async fn add_proxy(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let object = match body_object(body.clone()) { Ok(object) => object, Err(e) => return gateway_error(e) };
    let id = match require_string(&object, "id") { Ok(id) => id, Err(e) => return gateway_error(e) };
    let proxy: ProxyConfig = match from_body(body) { Ok(proxy) => proxy, Err(e) => return gateway_error(e) };
    match state().upsert_proxy(proxy).await {
        Ok(saved) => { log_admin("proxy.upsert", &id); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

pub async fn get_proxy(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match state().get_proxy(&id).await {
        Some(proxy) => ok(proxy.public_value()),
        None => gateway_error(GatewayError::NotFound(format!("proxy {}", id))),
    }
}

pub async fn update_proxy(headers: HeaderMap, Path(id): Path<String>, Json(patch): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let store = state();
    let current = match store.get_proxy(&id).await {
        Some(proxy) => proxy,
        None => return gateway_error(GatewayError::NotFound(format!("proxy {}", id))),
    };
    let updated: ProxyConfig = match merge_patch(&current, &patch) { Ok(proxy) => proxy, Err(e) => return gateway_error(e) };
    match store.upsert_proxy(updated).await {
        Ok(saved) => { log_admin("proxy.patch", &id); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

pub async fn delete_proxy(headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    match state().delete_proxy(&id).await {
        Ok(()) => { log_admin("proxy.delete", &id); ok(serde_json::json!({"status":"ok","deleted":id})) }
        Err(e) => gateway_error(e),
    }
}

/// Smoke test all endpoints of a proxy config through real reqwest proxies.
pub async fn test_proxy(headers: HeaderMap, Path(raw_id): Path<String>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let id = strip_test_suffix(&raw_id).to_string();
    let config = match state().get_proxy(&id).await {
        Some(config) => config,
        None => return gateway_error(GatewayError::NotFound(format!("proxy {}", id))),
    };
    let mut results = Vec::new();
    let target = config.healthcheck_url.clone().unwrap_or_else(|| "https://example.com".to_string());
    for url in config.endpoints() {
        let display = crate::gateway::store::redact_url(&url);
        let outcome = match reqwest::Proxy::all(&url)
            .map_err(|e| e.to_string())
            .and_then(|proxy| reqwest::Client::builder().proxy(proxy).timeout(Duration::from_secs(8)).build().map_err(|e| e.to_string()))
        {
            Ok(client) => {
                let started = Instant::now();
                match client.get(&target).send().await {
                    Ok(response) => {
                        let latency = started.elapsed();
                        let outcome = if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            ProxyOutcome::Quota
                        } else if response.status().is_server_error() {
                            ProxyOutcome::ServerError
                        } else {
                            ProxyOutcome::Success { latency }
                        };
                        let endpoint = ProxyEndpoint { proxy_id: id.clone(), url: url.clone(), strategy: config.strategy };
                        proxy::report_result(&id, &endpoint, outcome);
                        serde_json::json!({"proxy": display, "status": response.status().as_u16(), "latency_ms": latency.as_millis(), "result": "ok"})
                    }
                    Err(e) => {
                        let endpoint = ProxyEndpoint { proxy_id: id.clone(), url: url.clone(), strategy: config.strategy };
                        proxy::report_result(&id, &endpoint, ProxyOutcome::Timeout);
                        serde_json::json!({"proxy": display, "result": "error", "message": e.to_string()})
                    }
                }
            }
            Err(message) => serde_json::json!({"proxy": display, "result": "error", "message": message}),
        };
        results.push(outcome);
    }
    log_admin("proxy.test", &id);
    ok(serde_json::json!({"id": id, "target": crate::gateway::store::redact_url(&target), "results": results, "health": proxy::health_snapshot(&id).await}))
}

// ---- pools ---------------------------------------------------------------

pub async fn list_pools(headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    ok(Value::Array(state().list_pools().await.iter().map(|p| p.public_value()).collect()))
}

pub async fn put_pool(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let pool: PoolConfig = match from_body(body) { Ok(pool) => pool, Err(e) => return gateway_error(e) };
    let name = pool.name.clone();
    match state().upsert_pool(pool).await {
        Ok(saved) => { log_admin("pool.upsert", &name); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

pub async fn put_pool_named(headers: HeaderMap, Path(name): Path<String>, Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    let mut object = match body_object(body) { Ok(object) => object, Err(e) => return gateway_error(e) };
    object.insert("name".to_string(), Value::String(name.clone()));
    let pool: PoolConfig = match from_body(Value::Object(object)) { Ok(pool) => pool, Err(e) => return gateway_error(e) };
    match state().upsert_pool(pool).await {
        Ok(saved) => { log_admin("pool.upsert", &name); ok(saved.public_value()) }
        Err(e) => gateway_error(e),
    }
}

/// Admin view of LB/proxy diagnostics (no secrets).
pub async fn gateway_status(headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&headers) { return response; }
    ok(serde_json::json!({
        "pools": Value::Array(state().list_pools().await.iter().map(|p| p.public_value()).collect()),
        "lb": crate::gateway::lb::stats_snapshot(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_patch_updates_nested_objects() {
        let mut provider = GatewayProvider::default();
        provider.id = "p1".to_string();
        provider.proxy.mode = crate::gateway::store::ProxyMode::None;
        let patch = serde_json::json!({"proxy": {"mode": "single", "single_url": "http://127.0.0.1:8080"}});
        let updated: GatewayProvider = merge_patch(&provider, &patch).unwrap();
        assert_eq!(updated.id, "p1");
        assert_eq!(updated.proxy.mode, crate::gateway::store::ProxyMode::Single);
        assert_eq!(updated.proxy.single_url.as_deref(), Some("http://127.0.0.1:8080"));
    }

    #[test]
    fn constant_time_eq_is_symmetric() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"short"));
    }
}
