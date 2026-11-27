//! Admin gateway + proxy/LB integration tests.
//!
//! Uses a temporary gateway state file and the real axum router (including
//! deprecated aliases) so route conflicts and secret redaction are exercised.

use agentgate::api::api;
use agentgate::gateway::{GatewayState, LbStrategy, StickyKey};
use agentgate::service::ServiceRegistry;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

fn request(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        return builder.body(Body::from(body.to_string())).unwrap();
    }
    builder.body(Body::empty()).unwrap()
}

async fn call(router: axum::Router, req: Request<Body>) -> (StatusCode, String, String) {
    let response = router.oneshot(req).await.expect("router service");
    let status = response.status();
    let deprecation = response
        .headers()
        .get("deprecation")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string(), deprecation)
}

#[tokio::test]
async fn admin_crud_redaction_aliases_and_lb_pool() {
    let temp = tempfile::tempdir().unwrap();
    let state_path = temp.path().join("gateway.json");
    let installed = agentgate::gateway::try_init_state(GatewayState::new(state_path.clone()));
    assert!(installed, "gateway state must be installed before first use");

    std::env::set_var("AGENTGATE_ADMIN_API_KEYS", "test-admin-key");
    std::env::set_var("AGENTGATE_ADMIN_ALLOW_NO_AUTH", "0");
    let registry = Arc::new(ServiceRegistry::new());
    let router = api().with_state(registry);

    // Unauthenticated admin access is rejected.
    let (status, body, _) = call(router.clone(), request("GET", "/admin/providers", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {}", body);

    let authorized = |req: Request<Body>| {
        let mut req = req;
        req.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer test-admin-key"),
        );
        req
    };

    // Provider CRUD: secrets never appear in responses or the redacted list.
    let (status, body, _) = call(
        router.clone(),
        authorized(request(
            "POST",
            "/admin/providers",
            Some(json!({
                "id": "openrouter",
                "name": "OpenRouter",
                "provider_kind": "http_openai_compatible",
                "mode": "http",
                "base_url": "https://openrouter.ai/api/v1",
                "api_key": "sk-super-secret-value",
                "models": ["openai/gpt-4o-mini"]
            })),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    assert!(!body.contains("sk-super-secret-value"));
    assert!(body.contains("***"));

    let (status, body, _) = call(router.clone(), authorized(request("GET", "/admin/providers", None))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("sk-super-secret-value"));

    let (status, body, _) = call(
        router.clone(),
        authorized(request(
            "PATCH",
            "/admin/providers/openrouter",
            Some(json!({"enabled": false})),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    assert!(body.contains("\"enabled\":false"));

    // Proxy pool with credentials: every URL is redacted in API responses.
    let (status, body, _) = call(
        router.clone(),
        authorized(request(
            "POST",
            "/admin/proxies",
            Some(json!({
                "id": "primary-pool",
                "mode": "pool",
                "pool": ["http://user:pass@proxy-a:8080", "http://proxy-b:8080"],
                "strategy": "round_robin"
            })),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    assert!(!body.contains("user:pass"));
    assert!(body.contains("***"));

    let (status, body, _) = call(router.clone(), authorized(request("GET", "/admin/proxies", None))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("user:pass"));

    // Combo model CRUD (unified with models per spec §8.2).
    let (status, body, _) = call(
        router.clone(),
        authorized(request(
            "POST",
            "/admin/combos",
            Some(json!({
                "id": "combo/free-smart",
                "candidates": [
                    {"model": "agg/opencode/beam-free", "weight": 1},
                    {"model": "opencode/9router/free-forever", "weight": 1}
                ],
                "fallback_on": ["quota", "timeout"],
                "max_attempts": 3
            })),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["kind"], "combo");
    // Combo is registered in the runtime registry, not only in the catalog.
    let runtime_combo = agentgate::provider::combo::registry().get("combo/free-smart").await;
    assert!(runtime_combo.is_some(), "combo must be executable by the router");
    assert_eq!(runtime_combo.unwrap().max_attempts, 2);

    // Free pool config: LB strategies and sticky sessions persist.
    let (status, body, _) = call(
        router.clone(),
        authorized(request(
            "PUT",
            "/admin/pools/free_forever",
            Some(json!({
                "strategy": "least_errors",
                "sticky_key": "session_id",
                "cooldown_on_quota_secs": 3600,
                "members": [
                    {"model": "opencode/beam-free", "weight": 3, "enabled": true},
                    {"model": "opencode/flint-40b-free", "weight": 1, "enabled": true}
                ],
                "fallback": ["opencode/9router/free-forever"]
            })),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {}", body);
    let pool = agentgate::gateway::state().get_pool("free_forever").await.unwrap();
    assert_eq!(pool.strategy, LbStrategy::LeastErrors);
    assert_eq!(pool.sticky_key, StickyKey::SessionId);
    assert_eq!(pool.members.len(), 2);

    // Legacy aliases still work and advertise deprecation.
    let (status, _, deprecation) = call(
        router.clone(),
        authorized(request(
            "POST",
            "/admin/provider",
            Some(json!({"id": "legacy-cli", "provider_kind": "cli", "base_url": null})),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deprecation, "true");

    let (status, _, deprecation) = call(
        router.clone(),
        authorized(request(
            "POST",
            "/admin/proxy",
            Some(json!({"path": "/", "method": "GET", "target": "https://example.com/api"})),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deprecation, "true");

    let stored = std::fs::read_to_string(&state_path).unwrap();
    assert!(stored.contains("openrouter"));
    // Credentials stay in the local state file; only API responses are redacted.
    assert!(stored.contains("user:pass"));
}
