//! P1 Config lengkap (spec §14): `[bridge.batch]`, `[bridge.resume]`,
//! `[bridge.limits]`, `[bridge.guard]`, `[workspace.*]` parse + `bridge.limits`
//! limits ditegakkan + `parallel_tool_calls` diterima.

use agentgate::config::Config;
use agentgate::permission::ClientToolsPolicy;
use agentgate::service::ServiceRegistry;
use agentgate::types::{ChatCompletionRequest, FunctionDefinition, ToolDefinition};

const FULL_TOML: &str = r#"
[server]
host = "127.0.0.1"
port = 8787
request_timeout_secs = 600
cors = false

[defaults]
workdir = "."
max_concurrency_per_provider = 2
model_cache_ttl_secs = 300

[routing]
no_tools_policy = "chat_only"
on_client_tools = "reject"
default_workspace = "default"

[bridge]
enabled = true
host = "127.0.0.1"
port = 0
park_ttl_secs = 600
call_ttl_secs = 600
max_parked_runs = 8
max_runs_per_key = 4
max_run_lifetime_secs = 3600
max_steps = 64
parked_holds_permit = true
server_name = "agentgate"

[bridge.batch]
window_ms = 150
max_calls = 16

[bridge.resume]
replay = true
partial_grace_ms = 0
allow_retry = false

[bridge.limits]
max_tools = 128
max_schema_bytes = 65536
max_result_bytes = 262144
max_prompt_chars = 400000

[bridge.guard]
enforce = true
preflight_probe = true

[workspace.default]
path = "."
permission_mode = "read-only"

[[provider]]
id = "ollama"
base_url = "http://127.0.0.1:11434"
num_ctx = 16384
keep_alive = "10m"
prompt_tools_fallback = false
"#;

#[test]
fn full_spec_template_parses() {
    let cfg: Config = toml::from_str(FULL_TOML).expect("§14 config must parse");
    assert_eq!(cfg.routing.on_client_tools, ClientToolsPolicy::Reject);
    assert_eq!(cfg.routing.default_workspace, "default");
    assert_eq!(cfg.bridge.batch.window_ms, 150);
    assert_eq!(cfg.bridge.batch.max_calls, 16);
    assert!(cfg.bridge.resume.replay);
    assert_eq!(cfg.bridge.resume.partial_grace_ms, 0);
    assert_eq!(cfg.bridge.limits.max_tools, 128);
    assert_eq!(cfg.bridge.guard.enforce, true);
    assert_eq!(cfg.bridge.guard.preflight_probe, true);
    assert_eq!(cfg.bridge.max_steps, 64);
    assert!(matches!(
        cfg.workspace.get("default").expect("workspace.default").permission_mode.as_str(),
        "read-only"
    ));
    assert_eq!(cfg.provider.len(), 1);
    assert_eq!(cfg.provider[0].keep_alive.as_deref(), Some("10m"));
}

#[test]
fn config_default_has_spec_values() {
    let cfg = Config::default();
    assert_eq!(cfg.bridge.batch.window_ms, 150);
    assert_eq!(cfg.bridge.batch.max_calls, 16);
    assert_eq!(cfg.bridge.resume.replay, true);
    assert_eq!(cfg.bridge.limits.max_tools, 128);
    assert_eq!(cfg.bridge.guard.enforce, true);
    assert!(cfg.workspace.is_empty());
}

#[test]
fn env_overrides_take_priority_over_file_values() {
    std::env::set_var("AGENTGATE_BRIDGE_PORT", "9999");
    std::env::set_var("AGENTGATE_BRIDGE_CALL_TTL_SECS", "321");
    std::env::set_var("AGENTGATE_BRIDGE_MAX_PARKED_RUNS", "3");
    let cfg = Config::default().apply_env_overrides();
    assert_eq!(cfg.bridge.port, 9999);
    assert_eq!(cfg.bridge.call_ttl_secs, 321);
    assert_eq!(cfg.bridge.max_parked_runs, 3);
    std::env::remove_var("AGENTGATE_BRIDGE_PORT");
    std::env::remove_var("AGENTGATE_BRIDGE_CALL_TTL_SECS");
    std::env::remove_var("AGENTGATE_BRIDGE_MAX_PARKED_RUNS");
}

fn tools(n: usize) -> Vec<ToolDefinition> {
    (0..n)
        .map(|i| ToolDefinition {
            r#type: "function".into(),
            function: FunctionDefinition {
                name: format!("t{}", i),
                description: None,
                parameters: Some(serde_json::json!({"type": "object"})),
            },
        })
        .collect()
}

#[tokio::test]
async fn max_tools_limit_is_enforced() {
    let mut registry = ServiceRegistry::new();
    registry.bridge_config.limits.max_tools = 2;

    let req = ChatCompletionRequest {
        model: "x".into(),
        messages: vec![],
        tools: Some(tools(3)),
        ..Default::default()
    };
    let err = registry
        .chat_completion(req)
        .await
        .err()
        .expect("too many tools must fail");
    assert!(err.to_string().contains("too_many_tools"), "err: {}", err);
}

#[tokio::test]
async fn max_schema_bytes_limit_is_enforced() {
    let mut registry = ServiceRegistry::new();
    registry.bridge_config.limits.max_schema_bytes = 64;

    let mut big_tools = tools(1);
    big_tools[0].function.parameters = Some(serde_json::json!({
        "type": "object",
        "properties": {"huge": {"type": "string", "description": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}
    }));

    let req = ChatCompletionRequest {
        model: "x".into(),
        messages: vec![],
        tools: Some(big_tools),
        ..Default::default()
    };
    let err = registry
        .chat_completion(req)
        .await
        .err()
        .expect("oversized schema must fail");
    assert!(err.to_string().contains("schema too large"), "err: {}", err);
}

#[test]
fn parallel_tool_calls_is_accepted_in_request() {
    let req: ChatCompletionRequest = serde_json::from_str(
        r#"{"model":"m","messages":[],"parallel_tool_calls":false}"#,
    )
    .expect("parallel_tool_calls must be accepted");
    assert_eq!(req.parallel_tool_calls, Some(false));
}
