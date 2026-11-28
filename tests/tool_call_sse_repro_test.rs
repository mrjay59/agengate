//! Repro: capture RAW SSE for a tool_call with long ExecCommand arguments.
//! Verifies gateway does not corrupt/truncate arguments (UTF-8, Windows path,
//! newlines, quotes/dashes).

use agentgate::api::api;
use agentgate::provider::{CliProvider, ProviderResponse};
use agentgate::reasoning::{FinishReason, RunEvent};
use agentgate::reasoning::event::{FunctionCall, ToolCall};
use agentgate::service::ServiceRegistry;
use agentgate::types::ChatCompletionRequest;
use async_trait::async_trait;
use axum::{body::Body, http::Request};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

struct EmitsLongToolCall {
    args: String,
}

#[async_trait]
impl CliProvider for EmitsLongToolCall {
    fn id(&self) -> String { "repro-tool".into() }
    fn bin(&self) -> Option<String> { Some("repro-tool".into()) }
    fn models(&self) -> Vec<String> { vec!["repro-tool-model".into()] }
    fn is_available(&self) -> bool { true }
    fn set_available(&mut self, _v: bool) {}
    async fn check_available(&self) -> bool { true }
    async fn scan_models(&self) -> Result<Vec<String>, anyhow::Error> {
        Ok(vec!["repro-tool-model".into()])
    }
    async fn send_chat_completion(
        &self, _m: &str, _r: ChatCompletionRequest,
    ) -> Result<ProviderResponse, anyhow::Error> {
        Ok(ProviderResponse::Success {
            content: String::new(), reasoning: None, tool_calls: None,
            session_id: None, mcp_session_id: None,
        })
    }
    async fn send_chat_completion_stream(
        &self, _m: &str, _r: ChatCompletionRequest, tx: mpsc::Sender<RunEvent>,
    ) -> Result<(), anyhow::Error> {
        let _ = tx.send(RunEvent::ToolCalls { calls: vec![
            ToolCall {
                id: "call_exec_1".into(),
                r#type: "function".into(),
                function: FunctionCall {
                    name: "ExecCommand".into(),
                    arguments: self.args.clone(),
                },
            },
        ]}).await;
        let _ = tx.send(RunEvent::Done { reason: FinishReason::Stop }).await;
        Ok(())
    }
}

fn make_long_args() -> String {
    // Include: ASCII, UTF-8 multibyte, Windows path, newlines, quotes/dashes.
    let windows_path = r"C:\Users\EDN\AppData\Roaming\AionUi\aionui\conversations\users\01a16d62\mcp_server.py";
    let mut s = String::new();
    s.push_str("{\"command\":\"python -c \\\"# -*- coding: utf-8 -*-\\n");
    s.push_str(&format!("path = '{}'\\n", windows_path.replace('\\', "\\\\")));
    s.push_str("print('hello-world -- dashed')\\n");
    s.push_str("print(\\\"double quotes\\\")\\n");
    s.push_str("msg = 'café naïve — \u{201C}smart quotes\u{201D}? maybe not'\\n");
    for i in 0..30 {
        s.push_str(&format!("line_{i}: alpha beta gamma € 中文 日本語 emoji=🧪\\n"));
    }
    s.push_str("tail -- end\\\"\"}");
    s
}

#[tokio::test]
async fn tool_call_sse_arguments_round_trip_exactly() {
    let args = make_long_args();
    let mut registry = ServiceRegistry::new();
    registry.first_token_timeout = Duration::from_secs(600);
    registry.register(Box::new(EmitsLongToolCall { args: args.clone() })).await;
    let app = api().with_state(Arc::new(registry));

    let body = serde_json::json!({
        "model": "repro-tool-model",
        "messages": [{ "role": "user", "content": "run it" }],
        "stream": true,
        "tools": [{
            "type": "function",
            "function": {
                "name": "ExecCommand",
                "description": "Run command",
                "parameters": {
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"]
                }
            }
        }]
    });

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method(axum::http::Method::POST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();

    let resp = tower::util::ServiceExt::oneshot(app, req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8(bytes.to_vec()).expect("SSE body must be valid UTF-8");

    // (a) first tool_call delta has function.name
    assert!(raw.contains(r#""name":"ExecCommand""#) || raw.contains(r#""name": "ExecCommand""#),
        "missing tool name in raw SSE: {raw}");

    // Collect argument fragments from tool_calls deltas.
    let mut fragments: Vec<String> = Vec::new();
    let mut saw_tool_call = false;
    for line in raw.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        if data == "[DONE]" { continue; }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        let Some(tcs) = v["choices"][0]["delta"]["tool_calls"].as_array() else { continue };
        for tc in tcs {
            saw_tool_call = true;
            if let Some(arg) = tc["function"]["arguments"].as_str() {
                if !arg.is_empty() {
                    fragments.push(arg.to_string());
                }
            }
        }
    }
    assert!(saw_tool_call, "no tool_calls deltas found in raw SSE: {raw}");

    let joined = fragments.concat();
    // (b) concatenate to EXACT original string
    assert_eq!(joined, args, "tool_call arguments corrupted/truncated.\njoined len={} args len={}\njoined={}\nargs={}", joined.len(), args.len(), joined, args);

    // (c) no extra newlines inserted between characters - exact equality already covers this,
    // but also assert raw doesn't have doubled newlines inside JSON argument values beyond what args contains.
    assert_eq!(joined.matches('\n').count(), args.matches('\n').count());

    // (d) no typographic quote/dash substitutions
    assert_eq!(joined.matches('—').count(), args.matches('—').count());
    assert_eq!(joined.matches('“').count(), args.matches('“').count());
    assert_eq!(joined.matches('”').count(), args.matches('”').count());
    assert!(joined.contains("€"));
    assert!(joined.contains("中文"));
    assert!(joined.contains("🧪"));
    assert!(joined.contains("AionUi\\\\aionui\\\\conversations\\\\users\\\\01a16d62\\\\mcp_server.py"));
}