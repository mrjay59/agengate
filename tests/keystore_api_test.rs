//! FIX-KEYSTORE / FIX-QUOTA-ACCOUNTING: admin key lifecycle E2E over axum.

use agentgate::api::api;
use agentgate::config::QuotaConfig;
use agentgate::keystore::{init_keystore, KeyStore};
use agentgate::service::ServiceRegistry;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use std::sync::Arc;
use tower::ServiceExt;

async fn call(app: &axum::Router, method: Method, uri: &str, body: serde_json::Value) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(res: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn key_lifecycle_admin_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::open(
        dir.path().join("keys.sqlite3"), QuotaConfig::default(), false, Vec::new(),
    ).unwrap();
    init_keystore(store);
    // The global store outlives this test; keep its DB file alive too.
    std::mem::forget(dir);
    let store = agentgate::keystore::keystore().unwrap().clone();
    assert_eq!(store.count_keys(), 0);

    let app = api().with_state(Arc::new(ServiceRegistry::new()));

    // Create
    let res = call(&app, Method::POST, "/admin/keys", serde_json::json!({
        "name": "aionui-prod",
        "scopes": ["chat.completions", "models.read", "admin"],
        "allowed_models": ["agg/*"],
        "max_uses_total": 100,
        "quota": { "requests_per_day": 5 }
    })).await;
    assert_eq!(res.status(), StatusCode::OK);
    let created = body_json(res).await;
    let id = created["key"]["id"].as_str().unwrap().to_string();
    let secret = created["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("agk_"));
    assert_eq!(created["key"]["used_count"], 0);
    assert_eq!(created["key"]["quota"]["requests_per_day"], 5);

    // List / get expose metadata only.
    let list = body_json(call(&app, Method::GET, "/admin/keys", serde_json::json!({})).await).await;
    assert_eq!(list["total"], 1);
    assert!(serde_json::to_string(&list).unwrap().find(&secret).is_none());
    let got = body_json(call(&app, Method::GET, &format!("/admin/keys/{}", id), serde_json::json!({})).await).await;
    assert_eq!(got["name"], "aionui-prod");

    // Patch
    let patched = body_json(call(&app, Method::PATCH, &format!("/admin/keys/{}", id),
        serde_json::json!({ "name": "renamed", "status": "active" })).await).await;
    assert_eq!(patched["name"], "renamed");

    // Rotate: new secret works, old one is revoked.
    let rotated = body_json(call(&app, Method::POST, &format!("/admin/keys/{}/rotate", id),
        serde_json::json!({})).await).await;
    let new_secret = rotated["secret"].as_str().unwrap().to_string();
    assert_ne!(new_secret, secret);
    assert!(store.authenticate(&secret).is_err());
    assert!(store.authenticate(&new_secret).is_ok());

    // Revoke
    let revoked = body_json(call(&app, Method::POST, &format!("/admin/keys/{}/revoke", id),
        serde_json::json!({})).await).await;
    assert_eq!(revoked["status"], "revoked");
    assert!(store.authenticate(&new_secret).is_err());
}

#[tokio::test]
async fn quota_exceeded_error_envelope() {
    use axum::response::IntoResponse;
    let res = agentgate::error::AppError::QuotaExceeded {
        reason: "requests_per_day".to_string(),
        scope: "key/provider/model".to_string(),
        reset_at: Some(1_800_000_000),
    }.into_response();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = body_json(res).await;
    assert_eq!(body["error"]["type"], "quota_exceeded");
    assert_eq!(body["error"]["code"], "quota_exceeded");
    assert_eq!(body["error"]["scope"], "key/provider/model");
    assert_eq!(body["error"]["reset_at"], 1_800_000_000);
}

#[tokio::test]
async fn usage_and_quota_admin_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::open(
        dir.path().join("keys.sqlite3"), QuotaConfig::default(), false, Vec::new(),
    ).unwrap();
    // The process-global instance is set by the first test; use it if so.
    init_keystore(store);
    std::mem::forget(dir);
    let _store = agentgate::keystore::keystore().unwrap().clone();

    let app = api().with_state(Arc::new(ServiceRegistry::new()));
    let usage = body_json(call(&app, Method::GET, "/admin/usage", serde_json::json!({})).await).await;
    assert!(usage["summary"]["requests"].is_number());

    let quotas = body_json(call(&app, Method::GET, "/admin/quotas", serde_json::json!({})).await).await;
    assert_eq!(quotas["defaults"]["requests_per_minute"], 60);
    let patched = body_json(call(&app, Method::PATCH, "/admin/quotas",
        serde_json::json!({ "requests_per_minute": 7 })).await).await;
    assert_eq!(patched["defaults"]["requests_per_minute"], 7);
}
