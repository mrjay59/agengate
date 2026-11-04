use crate::config::ReasoningConfig;
use crate::error::AppError;
use crate::reasoning::{
    format::{parse_reasoning_format, reasoning_headers, SseWriter},
    Coalescer, CoalescerSettings, ReasoningFormat, ReasoningKind, ReasoningPipeline, RunEvent,
};
use crate::service::ServiceRegistry;
use crate::types::{
    AddKeyRequest, AddMcpRequest, AddProviderRequest, ChatCompletionRequest,
    ModelsResponse, ProxyRequest, AddSkillRequest,
};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, HeaderValue},
    response::{IntoResponse, Response, Sse},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

pub fn api() -> Router<Arc<ServiceRegistry>> {
    Router::new()
        .route("/", get(root_info))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/health", get(health_check))
        .route("/_bridge/mcp/:session_id", post(bridge_mcp).delete(bridge_mcp_delete))
        .route("/_bridge/resolve/:call_id", post(bridge_resolve))
        .route("/admin/provider", post(add_provider))
        .route("/admin/key", post(add_key))
        .route("/admin/proxy", post(proxy))
        .route("/admin/skill", post(add_skill))
        .route("/admin/mcp", post(add_mcp))
}

async fn root_info(State(registry): State<Arc<ServiceRegistry>>) -> Json<serde_json::Value> {
    let providers = registry.list_providers().await.unwrap_or_default();
    Json(serde_json::json!({
        "name": "agentgate",
        "version": env!("CARGO_PKG_VERSION"),
        "providers": providers.iter().map(|p| serde_json::json!({
            "id": p.id,
            "available": p.is_available,
            "models": p.models,
        })).collect::<Vec<_>>(),
        "endpoints": {
            "health": "/health",
            "models": "/v1/models",
            "chat": "/v1/chat/completions"
        }
    }))
}

async fn list_models(
    State(registry): State<Arc<ServiceRegistry>>,
) -> Result<Json<ModelsResponse>, AppError> {
    let models = registry.list_models(None).await?;
    Ok(Json(models))
}

/// Context shared by every chunk in one response (OpenAI ids are stable).
struct ChunkContext {
    id: String,
    created: i64,
    model: String,
}

impl ChunkContext {
    fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
        }
    }

    fn chunk(&self, delta: serde_json::Value, finish_reason: Option<&str>) -> String {
        serde_json::json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }]
        })
        .to_string()
    }
}

/// Determine the effective reasoning format.
///
/// Priority (spec_reasoning §5.1): request header > request body > config.
fn resolve_reasoning(
    registry: &ServiceRegistry,
    headers: &HeaderMap,
    request: &ChatCompletionRequest,
) -> (ReasoningConfig, ReasoningFormat, &'static str) {
    let mut cfg = registry.reasoning_config().clone();
    let mut source = "config";
    let mut format = parse_reasoning_format(&cfg.format).unwrap_or_default();

    if let Some(reasoning) = request.reasoning.as_ref() {
        if reasoning
            .get("exclude")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            format = ReasoningFormat::None;
            source = "body";
        }
        if let Some(value) = reasoning.get("format").and_then(|v| v.as_str()) {
            if let Some(parsed) = parse_reasoning_format(value) {
                format = parsed;
                source = "body";
            }
        }
    }

    if let Some(value) = headers
        .get("x-agentgate-reasoning")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(parsed) = parse_reasoning_format(value) {
            format = parsed;
            source = "header";
        }
    }

    if !cfg.enabled {
        format = ReasoningFormat::None;
    }
    if format == ReasoningFormat::None {
        cfg.activity_via_reasoning = false;
    }

    (cfg, format, source)
}

async fn chat_completions(
    State(registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(mut request): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, AppError> {
    info!(
        "Request received: model={}, stream={:?}, messages={}",
        request.model,
        request.stream,
        request.messages.len()
    );

    // Spec validation (spec_tooling §7.1)
    if request.n.unwrap_or(1) > 1 {
        return Err(AppError::ToolError("n > 1 is not supported".to_string()));
    }
    if let Some(tools) = &request.tools {
        if tools.len() > 128 {
            return Err(AppError::ToolError("too_many_tools".to_string()));
        }
        for t in tools {
            if t.r#type != "function" {
                return Err(AppError::UnsupportedToolType);
            }
        }
    }

    // tool_choice "none" => no tools exposed (chat_only)
    if request.tool_choice.as_ref().and_then(|v| v.as_str()) == Some("none") {
        request.tools = None;
    }

    let provider_id = match request.provider.clone() {
        Some(provider) => provider,
        None => registry
            .provider_for_model(&request.model)
            .await
            .ok_or_else(|| AppError::ModelNotFound(request.model.clone()))?,
    };
    let (reasoning_cfg, format, _source) = resolve_reasoning(&registry, &headers, &request);
    let is_stream = request.stream.unwrap_or(false);

    if is_stream {
        let (tx, rx) = mpsc::channel::<Result<axum::response::sse::Event, axum::Error>>(128);
        let ctx = ChunkContext::new(&request.model);
        let registry_clone = registry.clone();
        let activity_via_reasoning = reasoning_cfg.activity_via_reasoning;
        let response_cfg = reasoning_cfg.clone();

        tokio::spawn(async move {
            let mut pipeline = ReasoningPipeline::new(&reasoning_cfg);
            let mut coalescer = Coalescer::with_settings(CoalescerSettings {
                flush_ms: reasoning_cfg.flush_ms,
                flush_bytes: reasoning_cfg.flush_bytes,
            });
            let mut writer = SseWriter::new(format, reasoning_cfg.null_content);

            // Early role chunk lowers time-to-first-byte and lets UIs show a
            // typing/thinking indicator (spec_reasoning §6.4).
            if reasoning_cfg.early_role_chunk {
                let _ = send_sse(
                    &tx,
                    &ctx.chunk(serde_json::json!({ "role": "assistant" }), None),
                )
                .await;
                // A comment also keeps intermediaries from buffering the
                // response while the CLI is still starting.
                let _ = tx
                    .send(Ok(axum::response::sse::Event::default().comment("ping")))
                    .await;
            }

            for event in pipeline.start() {
                emit_event(&tx, &ctx, &mut writer, event, activity_via_reasoning).await;
            }

            let mut events = registry_clone
                .stream_run_events(&provider_id, request)
                .await;
            let mut ticker = tokio::time::interval(Duration::from_millis(250));
            let mut last_heartbeat = Instant::now();

            loop {
                tokio::select! {
                    event = events.recv() => {
                        match event {
                            Some(event) => {
                                for processed in pipeline.process(event) {
                                    for coalesced in coalescer.add_event(processed) {
                                        emit_event(&tx, &ctx, &mut writer, coalesced, activity_via_reasoning).await;
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                    _ = ticker.tick() => {
                        for event in pipeline.tick(Instant::now()) {
                            for coalesced in coalescer.add_event(event) {
                                emit_event(&tx, &ctx, &mut writer, coalesced, activity_via_reasoning).await;
                            }
                        }
                        // Keep-alive heartbeat to prevent intermediaries/clients
                        // from treating a long-running turn as a dead connection.
                        if last_heartbeat.elapsed() >= Duration::from_secs(10) {
                            let _ = tx
                                .send(Ok(axum::response::sse::Event::default().comment("keep-alive")))
                                .await;
                            last_heartbeat = Instant::now();
                        }
                    }
                }
            }

            for processed in pipeline.finish() {
                for coalesced in coalescer.add_event(processed) {
                    emit_event(&tx, &ctx, &mut writer, coalesced, activity_via_reasoning).await;
                }
            }
            for coalesced in coalescer.force_flush() {
                emit_event(&tx, &ctx, &mut writer, coalesced, activity_via_reasoning).await;
            }
            for delta in writer.close_think() {
                let _ = send_sse(&tx, &ctx.chunk(delta, None)).await;
            }

            let _ = send_sse(&tx, &ctx.chunk(serde_json::json!({}), Some("stop"))).await;
            let _ = tx
                .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                .await;
        });

        let stream = ReceiverStream::new(rx);
        let mut res = Sse::new(stream).into_response();
        res.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
        res.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        );
        add_reasoning_headers(&mut res, format, &response_cfg, "block");

        Ok(res)
    } else {
        let response = registry.chat_completion(request).await?;
        let value = apply_non_stream_format(response, format, &reasoning_cfg);
        let mut res = Json(value).into_response();
        add_reasoning_headers(&mut res, format, &reasoning_cfg, "final");
        Ok(res)
    }
}

fn add_reasoning_headers(
    res: &mut Response,
    format: ReasoningFormat,
    cfg: &ReasoningConfig,
    granularity: &str,
) {
    let kind = if format == ReasoningFormat::None {
        ReasoningKind::Raw
    } else if cfg.activity_via_reasoning && cfg.fallback != "none" {
        ReasoningKind::Activity
    } else {
        ReasoningKind::Raw
    };
    for (k, v) in reasoning_headers(format, kind, granularity) {
        if let (Ok(name), Ok(value)) = (
            axum::http::header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            res.headers_mut().insert(name, value);
        }
    }
}

async fn send_sse(
    tx: &mpsc::Sender<Result<axum::response::sse::Event, axum::Error>>,
    data: &str,
) -> bool {
    tx.send(Ok(axum::response::sse::Event::default().data(data)))
        .await
        .is_ok()
}

async fn send_delta(
    tx: &mpsc::Sender<Result<axum::response::sse::Event, axum::Error>>,
    ctx: &ChunkContext,
    delta: serde_json::Value,
) {
    let _ = send_sse(tx, &ctx.chunk(delta, None)).await;
}

async fn emit_event(
    tx: &mpsc::Sender<Result<axum::response::sse::Event, axum::Error>>,
    ctx: &ChunkContext,
    writer: &mut SseWriter,
    event: RunEvent,
    activity_via_reasoning: bool,
) {
    match event {
        RunEvent::Text { text } => {
            for delta in writer.text_deltas(&text) {
                send_delta(tx, ctx, delta).await;
            }
        }
        RunEvent::ReasoningDelta { text, .. } => {
            for delta in writer.reasoning_deltas(&text) {
                send_delta(tx, ctx, delta).await;
            }
        }
        RunEvent::ReasoningEnd { .. } => {
            for delta in writer.reasoning_end_deltas() {
                send_delta(tx, ctx, delta).await;
            }
        }
        RunEvent::Activity { label, .. } => {
            if activity_via_reasoning {
                for delta in writer.reasoning_deltas(&label) {
                    send_delta(tx, ctx, delta).await;
                }
                for delta in writer.reasoning_end_deltas() {
                    send_delta(tx, ctx, delta).await;
                }
            }
        }
        RunEvent::ToolCalls { calls } => {
            // `</think>` must be closed before tool_calls even without text.
            for delta in writer.close_think() {
                send_delta(tx, ctx, delta).await;
            }
            let tool_calls: Vec<serde_json::Value> = calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    serde_json::json!({
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": {
                            "name": call.function.name,
                            "arguments": call.function.arguments
                        }
                    })
                })
                .collect();
            send_delta(tx, ctx, serde_json::json!({ "tool_calls": tool_calls })).await;
            let _ = send_sse(tx, &ctx.chunk(serde_json::json!({}), Some("tool_calls"))).await;
        }
        RunEvent::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        } => {
            let chunk = serde_json::json!({
                "id": ctx.id,
                "object": "chat.completion.chunk",
                "created": ctx.created,
                "model": ctx.model,
                "choices": [],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": total_tokens
                }
            });
            let _ = send_sse(tx, &chunk.to_string()).await;
        }
        RunEvent::Error { message, code } => {
            let chunk = serde_json::json!({
                "id": ctx.id,
                "object": "chat.completion.chunk",
                "created": ctx.created,
                "model": ctx.model,
                "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
                "error": { "message": message, "code": code }
            });
            let _ = send_sse(tx, &chunk.to_string()).await;
        }
        // ReasoningStart carries no client-visible payload and Done is emitted
        // by the stream terminator.
        RunEvent::ReasoningStart { .. } | RunEvent::Done { .. } => {}
    }
}

/// Apply `reasoning.format` to a non-streaming OpenAI response.
fn apply_non_stream_format(
    response: crate::types::ChatCompletionResponse,
    format: ReasoningFormat,
    _cfg: &ReasoningConfig,
) -> serde_json::Value {
    let mut value = serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({}));
    let reasoning = value
        .get_mut("choices")
        .and_then(|c| c.get_mut(0))
        .and_then(|c| c.get_mut("message"))
        .and_then(|m| {
            let message = m.as_object_mut()?;
            let reasoning = message
                .remove("reasoning_content")
                .and_then(|r| r.as_str().map(|s| s.to_string()))
                .filter(|s| !s.is_empty());
            if let Some(reasoning) = reasoning.as_ref() {
                match format {
                    ReasoningFormat::ReasoningContent => {
                        message.insert(
                            "reasoning_content".to_string(),
                            serde_json::Value::String(reasoning.clone()),
                        );
                    }
                    ReasoningFormat::Reasoning => {
                        message.insert(
                            "reasoning".to_string(),
                            serde_json::Value::String(reasoning.clone()),
                        );
                    }
                    ReasoningFormat::Both => {
                        message.insert(
                            "reasoning_content".to_string(),
                            serde_json::Value::String(reasoning.clone()),
                        );
                        message.insert(
                            "reasoning".to_string(),
                            serde_json::Value::String(reasoning.clone()),
                        );
                    }
                    ReasoningFormat::ThinkTags => {
                        let content = message
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("")
                            .to_string();
                        let wrapped = crate::reasoning::format::format_reasoning(
                            reasoning,
                            ReasoningFormat::ThinkTags,
                            ReasoningKind::Raw,
                        );
                        message.insert(
                            "content".to_string(),
                            serde_json::Value::String(format!(
                                "{}{}",
                                wrapped.think_tags, content
                            )),
                        );
                    }
                    ReasoningFormat::None | ReasoningFormat::Passthrough => {}
                }
            }
            Some(())
        });
    let _ = reasoning;
    value
}

async fn health_check() -> Json<serde_json::Value> {
    info!("Health check called");
    Json(serde_json::json!({"status": "ok"}))
}

async fn add_provider(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddProviderRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add provider {}", request.id);
    Ok(Json(serde_json::json!({"status": "ok"})))
}

async fn add_key(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddKeyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add key for provider {:?}", request.provider);
    Ok(Json(serde_json::json!({"status": "ok", "provider": request.provider})))
}

async fn proxy(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<ProxyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: proxy to {}", request.target);
    Ok(Json(serde_json::json!({"status": "ok", "target": request.target})))
}

async fn add_skill(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddSkillRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add skill {}", request.name);
    Ok(Json(serde_json::json!({"status": "ok", "skill": request.name})))
}

async fn add_mcp(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddMcpRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add MCP {}", request.name);
    Ok(Json(serde_json::json!({"status": "ok", "mcp": request.name})))
}

async fn bridge_mcp(
    State(registry): State<Arc<ServiceRegistry>>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    // Reject browser Origin
    if headers.get("origin").is_some() {
        return (axum::http::StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"origin not allowed"}))).into_response();
    }
    let session = match registry.bridge.get_session(&session_id).await {
        Some(s) => s,
        None => return (axum::http::StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"session not found"}))).into_response(),
    };
    // Auth
    let token = headers.get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if !crate::bridge::BridgeServer::validate_token(&session, token) {
        return (axum::http::StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"invalid token"}))).into_response();
    }

    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = body.get("id").cloned();
    let params = body.get("params").cloned().unwrap_or(serde_json::json!({}));

    let result = match method {
        "initialize" => {
            let pv = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18");
            let supported = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
            let negotiated = if supported.contains(&pv) { pv } else { "2025-06-18" };
            serde_json::json!({
                "protocolVersion": negotiated,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "agentgate-bridge", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        "notifications/initialized" => {
            return (axum::http::StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response();
        }
        "notifications/cancelled" => {
            return (axum::http::StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response();
        }
        "ping" => serde_json::json!({}),
        "tools/list" => {
            let tools: Vec<serde_json::Value> = session.tools.iter().map(|t| serde_json::json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.parameters,
            })).collect();
            serde_json::json!({ "tools": tools })
        }
        "tools/call" => {
            let _name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let _args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
            let mcp_id = id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            match registry.bridge.hold_tool_call(mcp_id.clone(), registry.bridge_config.call_ttl_secs).await {
                Ok(res) => serde_json::json!({
                    "content": [{ "type": "text", "text": res.content.to_string() }],
                    "isError": res.is_error
                }),
                Err(e) => serde_json::json!({
                    "content": [{ "type": "text", "text": e.to_string() }],
                    "isError": true
                }),
            }
        }
        "resources/list" | "prompts/list" => serde_json::json!({ "items": [] }),
        _ => {
            let err = serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } });
            return Json(err).into_response();
        }
    };

    let resp = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });
    Json(resp).into_response()
}

async fn bridge_mcp_delete(
    State(registry): State<Arc<ServiceRegistry>>,
    Path(session_id): Path<String>,
) -> axum::http::StatusCode {
    // idempotent session teardown
    let _ = registry.bridge.get_session(&session_id).await;
    axum::http::StatusCode::OK
}

async fn bridge_resolve(
    State(registry): State<Arc<ServiceRegistry>>,
    Path(call_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let content = body.get("content").cloned().unwrap_or(serde_json::json!({}));
    let is_error = body.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let res = crate::bridge::ToolResult {
        tool_call_id: call_id.clone(),
        name: body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        content,
        is_error,
    };
    let ok = registry.bridge.resolve_tool_call(&call_id, res).await;
    if !ok {
        return Json(serde_json::json!({ "resolved": false, "error": { "code": "tool_already_resolved" } }));
    }
    Json(serde_json::json!({ "resolved": ok }))
}
