//! Schema normalization wiring (spec §8 / audit P1): OpenAI-safe tool-name
//! aliasing, `$ref` inlining, and `normalize_schema` applied in the bridge
//! registration path.

use agentgate::api::api;
use agentgate::config::ProviderConfig;
use agentgate::openai::tools::{
    inline_refs, is_openai_tool_name, normalize_schema, openai_name_alias, openai_name_aliases,
    unalias_tool_calls, SchemaProfile,
};
use agentgate::provider::BridgeCliProvider;
use agentgate::service::ServiceRegistry;
use agentgate::bridge::ToolSchema;
use agentgate::types::{
    ChatCompletionRequest, FunctionCall, FunctionDefinition, ToolCall, ToolDefinition,
};
use axum::{body::Body, http::{Request, StatusCode}, response::Response};
use std::sync::Arc;
use tower::util::ServiceExt;

fn long_name() -> String {
    "write_file_with_a_very_long_name_that_exceeds_the_openai_limit_xy_more_padding".to_string()
}

fn tool_with_long_name() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: FunctionDefinition {
            name: long_name(),
            description: Some("long".into()),
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            })),
        },
    }
}

fn tool_with_refs() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: FunctionDefinition {
            name: "read_doc".into(),
            description: Some("refs".into()),
            parameters: Some(serde_json::json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "$defs": {
                    "Path": {"type": "object", "properties": {
                        "path": {"type": "string"}
                    }}
                },
                "properties": {
                    "file": {"$ref": "#/$defs/Path"},
                    "count": {"type": "integer", "minimum": 1}
                }
            })),
        },
    }
}

#[test]
fn openai_name_validation_and_aliasing() {
    assert!(is_openai_tool_name("simple_name-1"));
    assert!(!is_openai_tool_name(""));
    assert!(!is_openai_tool_name(&long_name()));

    let alias = openai_name_alias(&long_name(), 0);
    assert!(alias.len() <= 64);
    assert!(alias.starts_with("t00_"));
    assert!(is_openai_tool_name(&alias));
    // Short valid names pass through untouched.
    assert_eq!(openai_name_alias("write_file", 3), "write_file");
}

#[test]
fn aliases_are_deterministic_and_unaliasable() {
    let tools = vec![
        ToolDefinition {
            r#type: "function".into(),
            function: FunctionDefinition {
                name: "alpha".into(),
                description: None,
                parameters: None,
            },
        },
        tool_with_long_name(),
    ];
    let aliases = openai_name_aliases(&tools);
    assert!(aliases.get("alpha").is_none());

    let alias = aliases.get(&long_name()).expect("long name aliased").clone();
    assert!(alias.starts_with("t01_"));

    let mut calls = vec![ToolCall {
        id: "call_1".into(),
        r#type: "function".into(),
        function: FunctionCall {
            name: alias,
            arguments: "{}".into(),
        },
    }];
    unalias_tool_calls(&mut calls, &tools);
    assert_eq!(calls[0].function.name, long_name());
}

#[test]
fn inline_refs_resolves_defs_and_definitions() {
    let value = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "$defs": {
            "Name": {"type": "string", "minLength": 1},
            "Thing": {"type": "object", "properties": {"n": {"$ref": "#/$defs/Name"}}}
        },
        "properties": {
            "a": {"$ref": "#/$defs/Name"},
            "b": {"$ref": "#/$defs/Thing"}
        }
    });
    let out = inline_refs(value);
    assert!(out.get("$defs").is_none());
    assert!(out.get("$schema").is_none() == false); // $schema is left at root (not a ref target)
    assert_eq!(out["properties"]["a"]["type"], "string");
    assert_eq!(out["properties"]["b"]["properties"]["n"]["type"], "string");
}

#[test]
fn inline_refs_ignores_non_local_refs() {
    let value = serde_json::json!({
        "type": "object",
        "properties": {"x": {"$ref": "https://example.com/$defs/Thing"}}
    });
    let out = inline_refs(value);
    assert!(out["properties"]["x"].get("$ref").is_some());
}

#[test]
fn normalize_schema_openai_profile() {
    let schema = ToolSchema {
        name: "t".into(),
        description: "d".into(),
        parameters: serde_json::json!({
            "type": "object",
            "$schema": "draft-07",
            "properties": {"a": {"type": "string", "additionalProperties": true}}
        }),
        provider: "opencode".into(),
        workspace: "default".into(),
    };
    let out = normalize_schema(&schema, SchemaProfile::OpenAI);
    assert!(out.parameters.get("$schema").is_none());
    assert_eq!(
        out.parameters["properties"]["a"].get("additionalProperties"),
        Some(&serde_json::json!(false))
    );
}

fn tools_from_request(provider: &BridgeCliProvider) -> Vec<ToolSchema> {
    let request = ChatCompletionRequest {
        model: "opencode/m".into(),
        messages: vec![],
        tools: Some(vec![tool_with_refs(), tool_with_long_name()]),
        ..Default::default()
    };
    provider.tool_schemas_from_request(&request)
}

#[test]
fn bridge_registration_applies_alias_ref_and_normalize() {
    let provider = BridgeCliProvider::with_bridge(ProviderConfig {
        id: "opencode".into(),
        bin: Some("opencode".into()),
        ..Default::default()
    }, None, None).expect("bridge provider");

    let schemas = tools_from_request(&provider);
    assert_eq!(schemas.len(), 2);

    // $ref inlined + $schema removed + additionalProperties enforced.
    let refs = &schemas[0];
    assert_eq!(refs.name, "read_doc");
    assert_eq!(refs.parameters["properties"]["file"]["type"], "object");
    assert_eq!(refs.parameters["properties"]["file"]["properties"]["path"]["type"], "string");
    assert!(refs.parameters.get("$defs").is_none());
    assert!(refs.parameters.get("$schema").is_none());

    // Long name aliased to an OpenAI-safe tNN_ name.
    let long = &schemas[1];
    assert!(long.name.starts_with("t01_"));
    assert!(is_openai_tool_name(&long.name));
    assert!(long.name.len() <= 64);
}

#[tokio::test]
async fn tools_list_advertises_aliased_names_over_mcp() {
    let registry = Arc::new(ServiceRegistry::new());
    let provider = BridgeCliProvider::with_bridge(ProviderConfig {
        id: "opencode".into(),
        bin: Some("opencode".into()),
        ..Default::default()
    }, Some(registry.bridge.clone()), None).expect("bridge provider");

    let request = ChatCompletionRequest {
        model: "opencode/m".into(),
        messages: vec![],
        tools: Some(vec![tool_with_long_name()]),
        ..Default::default()
    };
    let schemas = provider.tool_schemas_from_request(&request);
    let aliased = schemas[0].name.clone();
    let (session_id, token) = provider.register_session(schemas).await.expect("session");

    let app = api().with_state(registry.clone());
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });
    let req = Request::builder()
        .uri(format!("/_bridge/mcp/{}", session_id))
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp: Response = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains(&aliased), "body should advertise alias: {}", s);
    assert!(!s.contains(&long_name()), "original long name must not be advertised: {}", s);
}
