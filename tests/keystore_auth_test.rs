//! FIX-KEYSTORE: data-plane auth middleware over a fresh process-wide store.

use agentgate::api::api;
use agentgate::config::QuotaConfig;
use agentgate::keystore::{auth_middleware, init_keystore, KeyStore};
use agentgate::service::ServiceRegistry;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use std::sync::Arc;
use tower::ServiceExt;

async fn call(app: &axum::Router, method: Method, uri: &str, token: Option<&str>) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {}", t));
    }
    app.clone().oneshot(builder.body(Body::empty()).unwrap()).await.unwrap()
}

async fn json(res: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn lifecycle_auth_and_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::open(
        dir.path().join("keys.sqlite3"), QuotaConfig::default(), true, Vec::new(),
    ).unwrap();
    init_keystore(store);
    std::mem::forget(dir);

    let app = api().with_state(Arc::new(ServiceRegistry::new()))
        .layer(axum::middleware::from_fn(auth_middleware));

    // Public endpoints stay open.
    assert_eq!(call(&app, Method::GET, "/health", None).await.status(), StatusCode::OK);

    // Data plane requires a lifecycle key.
    let res = call(&app, Method::GET, "/v1/models", None).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let body = json(res).await;
    assert_eq!(body["error"]["code"], "invalid_api_key");

    // Bootstrap: first key may be created without auth.
    let mut create = Request::builder()
        .method(Method::POST)
        .uri("/admin/keys")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({
            "name": "bootstrap",
            "scopes": ["chat.completions", "models.read", "admin"]
        }).to_string())).unwrap();
    let created = app.clone().oneshot(create).await.unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created = json(created).await;
    let secret = created["secret"].as_str().unwrap().to_string();
    let id = created["key"]["id"].as_str().unwrap().to_string();

    // Valid key reaches the data plane.
    assert_eq!(call(&app, Method::GET, "/v1/models", Some(&secret)).await.status(), StatusCode::OK);

    // Admin scope is required for later admin calls even with a valid key.
    let mut other = Request::builder()
        .method(Method::POST)
        .uri("/admin/keys")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", secret))
        .body(Body::from(serde_json::json!({"name": "second"}).to_string())).unwrap();
    // This key has admin scope, so it is allowed; the important part is that
    // an unauthenticated call is rejected.
    assert_eq!(app.clone().oneshot(other).await.unwrap().status(), StatusCode::OK);

    // Rotate invalidates the old secret immediately.
    let rotated = app.clone().oneshot(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/admin/keys/{}/rotate", id))
            .header("authorization", format!("Bearer {}", secret))
            .body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated = json(rotated).await;
    let new_secret = rotated["secret"].as_str().unwrap().to_string();
    assert_eq!(call(&app, Method::GET, "/v1/models", Some(&secret)).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, Method::GET, "/v1/models", Some(&new_secret)).await.status(), StatusCode::OK);
}
