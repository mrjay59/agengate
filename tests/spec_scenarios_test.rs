//! P2 Suite spec_tooling.md §16.2 T1–T23 — skenario yang dapat dijalankan
//! hari ini. Yang belum dijalankan (butuh rearchitecture/CLI asli) ditandai
//! skip/ignored dengan alasan.
//!
//! Matrix hasil ada di docs/spec_tooling_progress.md.

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::service::{ServiceRegistry, truncate_parallel_tool_calls, truncate_tool_calls};
use agentgate::types::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, FunctionCall, FunctionDefinition,
    MessageRole, ToolCall, ToolDefinition,
};
use async_trait::async_trait;
use axum::{body::Body, http::{Request, StatusCode}, response::Response};
use std::sync::Arc;
use tower::util::ServiceExt;

// ---------------------------------------------------------------------------
// Dummy bridge-capable providers
// ---------------------------------------------------------------------------

struct ScriptedToolsProvider {
    parallel_calls: usize,
}

impl ScriptedToolsProvider {
    fn new(n: usize) -> Self { Self { parallel_calls: n } }
}

#[async_trait]
impl CliProvider for ScriptedToolsProvider {
    fn id(&self) -> String { "scripted".into() }
    fn bin(&self) -> Option<String> { Some("scripted".into()) }
    fn models(&self) -> Vec<String> { vec!["m1".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _available: bool) {}
    async fn check_available(&self) -> bool { true }
    fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> { Ok(vec!["m1".into()]) }
    fn is_bridge(&self) -> bool { true }

    async fn send_chat_completion(&self, _model: &str, req: ChatCompletionRequest) -> Result<ProviderResponse, anyhow::Error> {
        // Respect tool_choice=none - no tool calls should be made
        let tool_choice_none = req.tool_choice.as_ref()
            .and_then(|v| v.as_str())
            .map(|s| s == "none")
            .unwrap_or(false);
        
        let has_tool_result = req.messages.iter().any(|m| m.role == MessageRole::Tool);
        if has_tool_result || tool_choice_none {
            return Ok(ProviderResponse::Success {
                content: "done".into(),
                reasoning: None,
                tool_calls: None,
                session_id: None,
            });
        }
        let calls: Vec<ToolCall> = (0..self.parallel_calls)
            .map(|i| ToolCall {
                id: format!("call_{}", i + 1),
                r#type: "function".into(),
                function: FunctionCall {
                    name: "write_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            })
            .collect();
        Ok(ProviderResponse::Success {
            content: String::new(),
            reasoning: None,
            tool_calls: Some(calls),
            session_id: None,
        })
    }
}

fn client_tools() -> Option<Vec<ToolDefinition>> {
    Some(vec![ToolDefinition {
        r#type: "function".into(),
        function: FunctionDefinition {
            name: "write_file".into(),
            description: Some("write".into()),
            parameters: Some(serde_json::json!({"type": "object", "properties": {}})),
        },
    }])
}

async fn post_chat(registry: ServiceRegistry, body: serde_json::Value) -> Response {
    let app = api().with_state(Arc::new(registry));
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    app.oneshot(req).await.unwrap()
}

// ---------------------------------------------------------------------------
// T2 — satu tool, stream: chunk finish_reason + [DONE]
// ---------------------------------------------------------------------------
#[tokio::test]
async fn t2_stream_emits_done_terminator() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(ScriptedToolsProvider::new(1))).await;
    let resp = post_chat(registry, serde_json::json!({
        "model": "scripted/m1",
        "provider": "scripted",
        "stream": true,
        "messages": [],
        "tools": client_tools().unwrap()
    })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains("data:"), "sse body: {}", s);
    assert!(s.contains("[DONE]"), "sse body: {}", s);
}

// ---------------------------------------------------------------------------
// T3 — dua tool paralel dalam 1 response, finish_reason=tool_calls
// ---------------------------------------------------------------------------
#[tokio::test]
async fn t3_parallel_tool_calls_in_one_response() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(ScriptedToolsProvider::new(2))).await;
    let resp = post_chat(registry, serde_json::json!({
        "model": "scripted/m1",
        "provider": "scripted",
        "messages": [],
        "tools": client_tools().unwrap()
    })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    let calls = v["choices"][0]["message"]["tool_calls"].as_array().expect("tool_calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[1]["id"], "call_2");
}

// ---------------------------------------------------------------------------
// T4 — multi-langkah (2 turn): tool_calls -> resume -> teks final stop
// ---------------------------------------------------------------------------
#[tokio::test]
async fn t4_multi_turn_resume() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(ScriptedToolsProvider::new(1))).await;

    // Turn 1: client sends tools; expect tool_calls back.
    let app = api().with_state(Arc::new(registry));
    let mk = |body: serde_json::Value| {
        Request::builder()
            .uri("/v1/chat/completions")
            .method(axum::http::Method::POST)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let r1 = app
        .clone()
        .oneshot(mk(serde_json::json!({
            "model": "scripted/m1",
            "provider": "scripted",
            "messages": [{ "role": "user", "content": "tulis a.txt" }],
            "tools": client_tools().unwrap()
        })))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let b1 = axum::body::to_bytes(r1.into_body(), usize::MAX).await.unwrap();
    let v1: serde_json::Value = serde_json::from_slice(&b1).unwrap();
    let calls = v1["choices"][0]["message"]["tool_calls"].as_array().expect("tool_calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(v1["choices"][0]["finish_reason"], "tool_calls");

    // Turn 2: client appends tool result; provider answers with final text.
    let r2 = app
        .oneshot(mk(serde_json::json!({
            "model": "scripted/m1",
            "provider": "scripted",
            "messages": [
                { "role": "user", "content": "tulis a.txt" },
                { "role": "assistant", "tool_calls": calls, "content": null },
                { "role": "tool", "tool_call_id": "call_1", "content": "OK" }
            ],
            "tools": client_tools().unwrap()
        })))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let b2 = axum::body::to_bytes(r2.into_body(), usize::MAX).await.unwrap();
    let v2: serde_json::Value = serde_json::from_slice(&b2).unwrap();
    assert_eq!(v2["choices"][0]["finish_reason"], "stop");
    assert_eq!(v2["choices"][0]["message"]["content"], "done");
}

// ---------------------------------------------------------------------------
// T5 — parallel_tool_calls:false -> hanya 1 tool_call per giliran (native path helper)
// ---------------------------------------------------------------------------
#[test]
fn t5_parallel_tool_calls_false_keeps_first_call() {
    let mut resp = ChatCompletionResponse {
        id: "id".into(),
        object: "chat.completion".into(),
        created: 0,
        model: "m".into(),
        choices: vec![Choice {
            index: 0,
            message: agentgate::types::ChatMessage {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: Some(vec![
                    ToolCall { id: "c1".into(), r#type: "function".into(),
                        function: FunctionCall { name: "a".into(), arguments: "{}".into() } },
                    ToolCall { id: "c2".into(), r#type: "function".into(),
                        function: FunctionCall { name: "b".into(), arguments: "{}".into() } },
                ]),
                ..Default::default()
            },
            finish_reason: Some("tool_calls".into()),
        }],
        usage: None,
        session_id: None,
    };
    truncate_parallel_tool_calls(&mut resp, Some(false));
    let calls = resp.choices[0].message.tool_calls.as_ref().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "a");

    truncate_parallel_tool_calls(&mut resp, Some(true));
    truncate_parallel_tool_calls(&mut resp, None);
    assert_eq!(resp.choices[0].message.tool_calls.as_ref().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// T12 — skema oneOf + $ref dinormalisasi oleh bridge registration (gemini)
// ---------------------------------------------------------------------------
#[test]
fn t12_oneof_and_ref_normalized() {
    let provider = agentgate::provider::BridgeCliProvider::with_bridge(
        agentgate::config::ProviderConfig {
            id: "gemini".into(),
            bin: Some("gemini".into()),
            ..Default::default()
        },
        None,
    ).expect("gemini bridge provider");

    let req = ChatCompletionRequest {
        model: "gemini/g".into(),
        messages: vec![],
        tools: Some(vec![ToolDefinition {
            r#type: "function".into(),
            function: FunctionDefinition {
                name: "read_doc".into(),
                description: None,
                parameters: Some(serde_json::json!({
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "$defs": {"Target": {"type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"]}},
                    "properties": {
                        "target": {"$ref": "#/$defs/Target"},
                        "payload": {"oneOf": [{"type": "string"}, {"type": "integer"}]}
                    }
                })),
            },
        }]),
        ..Default::default()
    };
    let schemas = provider.tool_schemas_from_request(&req);
    let p = &schemas[0].parameters;
    assert!(p.get("$defs").is_none(), "refs must be inlined");
    assert_eq!(p["properties"]["target"]["type"], "object");
    assert_eq!(p["properties"]["target"]["properties"]["path"]["type"], "string");
    // oneOf survives normalization untouched.
    assert!(p["properties"]["payload"].get("oneOf").is_some());
    // StrictGemini profile: object properties get additionalProperties: false.
    assert_eq!(
        p["properties"]["payload"].get("additionalProperties"),
        Some(&serde_json::json!(false))
    );
}

// ---------------------------------------------------------------------------
// T16 — tool_choice:none => chat_only, tidak ada tool yang dipasang
// ---------------------------------------------------------------------------
#[tokio::test]
async fn t16_tool_choice_none_is_chat_only() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(ScriptedToolsProvider::new(1))).await;
    let resp = post_chat(registry, serde_json::json!({
        "model": "scripted/m1",
        "provider": "scripted",
        "messages": [],
        "tools": client_tools().unwrap(),
        "tool_choice": "none"
    })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mode_header = resp.headers().get("x-agentgate-mode").cloned();
    let mode_str = mode_header.as_ref().and_then(|v| v.to_str().ok().map(|s| s.to_string()));
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["choices"][0]["message"].get("tool_calls").is_none());
    assert_eq!(
        mode_str,
        Some("chat_only".to_string())
    );
}

// ---------------------------------------------------------------------------
// T17 — request tanpa tools => chat_only (sudah dicover default route)
// ---------------------------------------------------------------------------
#[tokio::test]
async fn t17_no_tools_default_chat_only() {
    let mut registry = ServiceRegistry::new();
    registry.register(Box::new(ScriptedToolsProvider::new(0))).await;
    let resp = post_chat(registry, serde_json::json!({
        "model": "scripted/m1",
        "provider": "scripted",
        "messages": []
    })).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let mode_header = resp.headers().get("x-agentgate-mode").cloned();
    let mode_str = mode_header.as_ref().and_then(|v| v.to_str().ok().map(|s| s.to_string()));
    assert_eq!(
        mode_str,
        Some("chat_only".to_string())
    );
}

// ---------------------------------------------------------------------------
// T20 — serial mode memotong list tool_calls langsung (batch) di bridge path
// ---------------------------------------------------------------------------
#[test]
fn t20_serial_truncates_tool_call_list() {
    let mut calls = Some(vec![
        ToolCall { id: "c1".into(), r#type: "function".into(),
            function: FunctionCall { name: "a".into(), arguments: "{}".into() } },
        ToolCall { id: "c2".into(), r#type: "function".into(),
            function: FunctionCall { name: "b".into(), arguments: "{}".into() } },
    ]);
    truncate_tool_calls(&mut calls, Some(false));
    assert_eq!(calls.as_ref().unwrap().len(), 1);

    let mut calls = Some(vec![
        ToolCall { id: "c1".into(), r#type: "function".into(),
            function: FunctionCall { name: "a".into(), arguments: "{}".into() } },
        ToolCall { id: "c2".into(), r#type: "function".into(),
            function: FunctionCall { name: "b".into(), arguments: "{}".into() } },
    ]);
    truncate_tool_calls(&mut calls, Some(true));
    assert_eq!(calls.as_ref().unwrap().len(), 2);

    truncate_tool_calls(&mut calls, None);
    assert_eq!(calls.as_ref().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// T11 — tools + provider tak bridge-capable => 501 (lihat fail_closed_test.rs)
// T13 — nama tool panjang/aneh => alias dibuat; nama asli dipulihkan
//        (lihat schema_normalization_test.rs)
// T14, T15, T18–T23 — belum bisa diuji tanpa rearchitecture/CLI asli; lihat
//        docs/spec_tooling_progress.md (matrix).
// ---------------------------------------------------------------------------

