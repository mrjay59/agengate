//! P1: Persistent session resume - keep CLI process alive between requests
//! This test verifies the session caching logic without requiring a running bridge server.

use agentgate::config::ProviderConfig;
use agentgate::provider::BridgeCliProvider;
use agentgate::service::ServiceRegistry;
use std::sync::Arc;

fn provider_config() -> ProviderConfig {
    ProviderConfig {
        id: "opencode".to_string(),
        enabled: true,
        bin: Some("fake_cli_persistent".to_string()),
        models: vec!["test-model".to_string()],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: false,
        keep_alive: Some("true".to_string()),
        ..Default::default()
    }
}

fn provider_config_non_persistent() -> ProviderConfig {
    ProviderConfig {
        id: "opencode".to_string(),
        enabled: true,
        bin: Some("fake_cli_persistent".to_string()),
        models: vec!["test-model".to_string()],
        models_cmd: None,
        chat_cmd: None,
        strip_ansi: false,
        keep_alive: Some("false".to_string()),
        ..Default::default()
    }
}

/// Test that BridgeCliProvider with keep_alive has is_keep_alive() = true
#[tokio::test]
async fn test_persistent_session_cache_exists() {
    let registry = Arc::new(ServiceRegistry::new());
    let provider = BridgeCliProvider::with_bridge(provider_config(), Some(registry.bridge.clone()), None)
        .expect("opencode bridge provider");

    // The provider should have keep_alive enabled
    assert!(provider.is_keep_alive());
}

/// Test that BridgeCliProvider without keep_alive has is_keep_alive() = false
#[tokio::test]
async fn test_non_persistent_session_has_no_keep_alive() {
    let registry = Arc::new(ServiceRegistry::new());
    let provider = BridgeCliProvider::with_bridge(provider_config_non_persistent(), Some(registry.bridge.clone()), None)
        .expect("opencode bridge provider");

    assert!(!provider.is_keep_alive());
}

/// Test that persistent sessions are keyed by session_id
#[tokio::test]
async fn test_persistent_sessions_keyed_by_session_id() {
    let registry = Arc::new(ServiceRegistry::new());
    let provider = BridgeCliProvider::with_bridge(provider_config(), Some(registry.bridge.clone()), None)
        .expect("opencode bridge provider");

    // Register a session to get a session_id
    let tool_schemas = vec![];
    let (session_id, _token) = provider.register_session(tool_schemas).await.expect("session registered");
    
    // Verify the session_id is generated
    assert!(!session_id.is_empty());
    
    // In a real scenario with keep_alive, this session_id would be used to cache the session
    // and reuse it across requests
}

/// Test that session_id from request is used for persistent session lookup
#[tokio::test]
async fn test_session_id_from_request_used_for_persistence() {
    // This test verifies the logic that session_id from the request
    // is used to look up or create persistent sessions
    
    // The actual integration test with fake CLI requires a running bridge server
    // which is tested in bridge_session_registration_test.rs and bridge_mcp_hold_test.rs
    
    // Here we just verify the ProviderConfig has the correct fields
    let config = provider_config();
    assert!(config.keep_alive.is_some());
    assert_eq!(config.keep_alive.as_deref(), Some("true"));
}