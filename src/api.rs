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
        .route("/_metrics", get(metrics_endpoint))
        .route("/_bridge/mcp/:session_id", post(bridge_mcp).delete(bridge_mcp_delete))
        .route("/_bridge/resolve/:call_id", post(bridge_resolve))
        .route("/_bridge/preflight/:session_id", get(preflight_isolation_check))
        .route("/admin/provider", post(add_provider))
        .route("/admin/key", post(add_key))
        .route("/admin/proxy", post(proxy))
        .route("/admin/skill", post(add_skill))
        .route("/admin/mcp", post(add_mcp))
}

/// Preflight isolation check before a tool-capable turn is admitted (spec §13.2).
/// Verifies that isolation is active and tool is bridge-compatible.
async fn preflight_isolation_check(
    State(registry): State<Arc<ServiceRegistry>>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
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

    // Verify isolation is active for this session
    if !session.isolation_active {
        info!(event = "guard.violation", session = %session.session_id, reason = "isolation_not_active");
        crate::metrics::METRICS.record_guard_violation();
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "message": "tool_isolation_unavailable",
                    "type": "invalid_request_error",
                    "code": "tool_isolation_unavailable",
                }
            })),
        ).into_response();
    }

    // Verify the tool name is bridge-compatible
    let tool_name = headers.get("x-agentgate-tool-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !registry.bridge.is_bridge_tool(tool_name) {
        info!(event = "guard.violation", session = %session.session_id, observed_tool = %tool_name, reason = "not_bridge_tool");
        crate::metrics::METRICS.record_guard_violation();
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": {
                    "message": format!("tool_isolation_unavailable: {}", tool_name),
                    "type": "invalid_request_error",
                    "code": "tool_isolation_unavailable",
                }
            })),
        ).into_response();
    }

    // Normalize tool name for bridge lookup
    let normalized_name = crate::bridge::BridgeServer::normalize_tool_name(tool_name, &session.tools);
    if normalized_name != tool_name {
        info!(event = "tool.normalized", session = %session.session_id, original = %tool_name, normalized = %normalized_name);
    }

    // All checks passed
    Json(serde_json::json!({
        "status": "ok",
        "isolation_active": true,
        "tool_name": normalized_name,
    })).into_response()
}

async fn metrics_endpoint() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        crate::metrics::METRICS.render_prometheus(),
    )
        .into_response()
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

    // Fail fast: reject unavailable providers (missing binary, broken install)
    // before opening a stream that would only send keep-alives. Ollama served
    // over HTTP (`ollama_mode = "native"`) is exempt: its availability is the
    // Ollama server's responsibility, not the CLI's.
    let is_http_native = !registry.ollama_cli_mode
        && (provider_id == "ollama" || request.model.starts_with("ollama/"));
    if !is_http_native && !registry.as_ref().is_provider_available(&provider_id).await {
        return Err(AppError::ProviderUnavailable(provider_id));
    }

    let (reasoning_cfg, format, _source) = resolve_reasoning(&registry, &headers, &request);
    let is_stream = request.stream.unwrap_or(false);

    // Observability (spec §15): run id, mode, counters, headers.
    let run_id = uuid::Uuid::new_v4().to_string();
    let is_native = is_http_native;
    let known_ids: std::collections::HashSet<String> = registry
        .run_registry
        .get_known_tool_call_ids()
        .await;
    let classified = crate::run::classify(&request, &[], is_native, &known_ids);
    let mode = match &classified {
        crate::run::Classified::Native => "native",
        crate::run::Classified::Resume { .. } => "resume",
        crate::run::Classified::Replay { .. } => "replay",
        crate::run::Classified::NewBridge { .. } => "bridge",
        crate::run::Classified::NewChatOnly => "chat_only",
    };
    crate::metrics::METRICS.mark_run_started();
    info!(
        event = "run.start",
        run_id = %run_id,
        provider = %provider_id,
        model = %request.model,
        mode = %mode,
        tools = request.tools.as_ref().map(|t| t.len()).unwrap_or(0)
    );
    let mut run_id_for_stream = Some(run_id.clone());
    let resume_mode = match mode {
        "resume" | "replay" => Some(mode),
        _ => None,
    };

    if is_stream {
        let (tx, rx) = mpsc::channel::<Result<axum::response::sse::Event, axum::Error>>(128);
        let ctx = ChunkContext::new(&request.model);
        let registry_clone = registry.clone();
        let activity_via_reasoning = reasoning_cfg.activity_via_reasoning;
        let response_cfg = reasoning_cfg.clone();
        let stream_run_id = run_id_for_stream.take().unwrap_or_default();

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
            let ttft_start = Instant::now();
            let mut ttft_recorded = false;
            let mut ticker = tokio::time::interval(Duration::from_millis(250));
            let mut last_heartbeat = Instant::now();
            let mut tool_calls_finished = false;
            // Fail fast on silence. A provider that never produces an event
            // (missing CLI, an upstream error the CLI never terminates after,
            // stuck process) used to keep this loop alive forever: the client
            // only ever saw `: keep-alive` comments and finally gave up with
            // "no response". Bound the silence and end the stream with an
            // explicit error chunk followed by `[DONE]`.
            //
            //  - before the first event: `defaults.first_token_timeout_secs`
            //    (agentgate.toml, default 120 s so cold CPU model loads fit)
            //  - after the first event: max silence, also 120 s by default
            //    (`AGENTGATE_STREAM_IDLE_TIMEOUT_SECS`)
            let first_token_timeout = registry_clone.first_token_timeout;
            let mut first_token_timer = Box::pin(tokio::time::sleep(first_token_timeout));
            let idle_timeout = stream_idle_timeout();
            let mut last_provider_activity = Instant::now();
            let mut terminated_by_error = false;

            loop {
                tokio::select! {
                    // The client hung up (browser closed, curl killed, proxy
                    // dropped the body). Stop draining provider events and drop
                    // the receiver when this task returns: the provider-side
                    // stream then sees a closed channel and kills its CLI child
                    // instead of orphaning `ollama run` / `opencode` processes.
                    _ = tx.closed() => {
                        info!(
                            event = "stream.client_disconnected",
                            run_id = %stream_run_id
                        );
                        break;
                    }
                    event = events.recv() => {
                        match event {
                            Some(event) => {
                                last_provider_activity = Instant::now();
                                if !ttft_recorded {
                                    crate::metrics::METRICS
                                        .observe_ttft(ttft_start.elapsed().as_millis() as u64);
                                    ttft_recorded = true;
                                }
                                let is_tool_calls = matches!(event, RunEvent::ToolCalls { .. });
                                if is_tool_calls {
                                    tool_calls_finished = true;
                                }
                                // An explicit provider error is terminal for the
                                // client: forward the chunk, then stop waiting
                                // for a CLI that may never exit (this is exactly
                                // the opencode quota-error case, which used to
                                // hang for ~2 minutes).
                                let is_error = matches!(event, RunEvent::Error { .. });
                                for processed in pipeline.process(event) {
                                    for coalesced in coalescer.add_event(processed) {
                                        emit_event(&tx, &ctx, &mut writer, coalesced, activity_via_reasoning).await;
                                    }
                                }
                                if is_error {
                                    terminated_by_error = true;
                                    let _ = tx
                                        .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                                        .await;
                                    break;
                                }
                                // A tool-calling turn is complete once tool_calls are
                                // emitted (client executes tools and sends results in a
                                // follow-up request). Stop the stream to avoid trailing
                                // events confusing OpenAI-compatible clients/UIs.
                                if is_tool_calls {
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = &mut first_token_timer, if !ttft_recorded => {
                        // First token timeout fired before any event received
                        let error_msg = format!(
                            "provider '{}' did not produce first token within {} seconds",
                            provider_id, first_token_timeout.as_secs()
                        );
                        let chunk = serde_json::json!({
                            "id": ctx.id,
                            "object": "chat.completion.chunk",
                            "created": ctx.created,
                            "model": ctx.model,
                            "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
                            "error": { "message": error_msg, "code": "first_token_timeout" }
                        });
                        let _ = send_sse(&tx, &chunk.to_string()).await;
                        // Emit [DONE] to properly terminate SSE stream
                        let _ = tx
                            .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                            .await;
                        terminated_by_error = true;
                        break;
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
                        // Ongoing idle timeout after first token
                        if ttft_recorded && last_provider_activity.elapsed() >= idle_timeout {
                            let error_msg = format!(
                                "provider '{}' produced no output for {}s; stream aborted",
                                provider_id, idle_timeout.as_secs()
                            );
                            let chunk = serde_json::json!({
                                "id": ctx.id,
                                "object": "chat.completion.chunk",
                                "created": ctx.created,
                                "model": ctx.model,
                                "choices": [{ "index": 0, "delta": {}, "finish_reason": "error" }],
                                "error": { "message": error_msg, "code": "provider_timeout" }
                            });
                            let _ = send_sse(&tx, &chunk.to_string()).await;
                            let _ = tx
                                .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                                .await;
                            terminated_by_error = true;
                            break;
                        }
                    }
                }
            }

            // Error/timeout paths already emitted their terminal chunk(s) and [DONE].
            if terminated_by_error {
                crate::metrics::METRICS.mark_run_ended();
                info!(event = "run.end", run_id = %stream_run_id, tool_calls = tool_calls_finished, terminated_by_error = true);
                return;
            }

            // For a completed tool-calling turn, avoid flushing buffered
            // reasoning/activity after the tool_calls boundary; emit only the
            // terminal finish_reason and [DONE].
            if tool_calls_finished {
                let _ = send_sse(&tx, &ctx.chunk(serde_json::json!({}), Some("tool_calls"))).await;
            } else {
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
            }
            let _ = tx
                .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                .await;
            crate::metrics::METRICS.mark_run_ended();
            info!(event = "run.end", run_id = %stream_run_id, tool_calls = tool_calls_finished);
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
        res.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-agentgate-run"),
            HeaderValue::from_str(&run_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
        );
        res.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-agentgate-mode"),
            HeaderValue::from_static(mode),
        );
        if mode == "replay" {
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-agentgate-replay"),
                HeaderValue::from_static("1"),
            );
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-agentgate-replay-reason"),
                HeaderValue::from_static("replay_fallback"),
            );
        }
        add_reasoning_headers(&mut res, format, &response_cfg, "block");

        Ok(res)
    } else {
        let response = match registry.chat_completion(request).await {
            Ok(resp) => {
                if let Some(mode_label) = resume_mode {
                    crate::metrics::METRICS.record_resume(if mode_label == "replay" {
                        "replay"
                    } else {
                        "ok"
                    });
                }
                resp
            }
            Err(e) => {
                if resume_mode.is_some() && e.code() == "tool_already_resolved" {
                    crate::metrics::METRICS.record_resume("conflict");
                }
                crate::metrics::METRICS.mark_run_ended();
                return Err(e);
            }
        };
        let value = apply_non_stream_format(response, format, &reasoning_cfg);
        crate::metrics::METRICS.mark_run_ended();
        let mut res = Json(value).into_response();
        res.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-agentgate-run"),
            HeaderValue::from_str(&run_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
        );
        res.headers_mut().insert(
            axum::http::header::HeaderName::from_static("x-agentgate-mode"),
            HeaderValue::from_static(mode),
        );
        if mode == "replay" {
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-agentgate-replay"),
                HeaderValue::from_static("1"),
            );
            res.headers_mut().insert(
                axum::http::header::HeaderName::from_static("x-agentgate-replay-reason"),
                HeaderValue::from_static("replay_fallback"),
            );
        }
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
            // SSE §7.4: emit per-tool-call chunks — Chunk 1 with empty arguments,
            // then Chunk 2+ with 512-byte argument slices. The terminal
            // `finish_reason:"tool_calls"` chunk is emitted by the stream
            // terminator to guarantee it is the last choice chunk before [DONE].
            for (index, call) in calls.iter().enumerate() {
                let name = call.function.name.clone();
                // Chunk 1: empty arguments
                send_delta(tx, ctx, serde_json::json!({
                    "tool_calls": [{
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": {
                            "name": name,
                            "arguments": ""
                        }
                    }]
                })).await;
                // Chunk 2+: 512-byte argument slices (continuation: no id/type/name)
                let args = call.function.arguments.as_str();
                let args_bytes = args.as_bytes();
                for chunk in args_bytes.chunks(512) {
                    send_delta(tx, ctx, serde_json::json!({
                        "tool_calls": [{
                            "index": index,
                            "function": {
                                "arguments": std::str::from_utf8(chunk).unwrap_or("")
                            }
                        }]
                    })).await;
                }
            }
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
            // P0-AUDIT seq1: cancel pending calls, stop run, kill CLI child/process tree,
            // cleanup temp/agentgate/sessions/{id}. Best-effort; always ACK.
            let _ = registry.bridge.cancel_session(&session_id).await;
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
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let _args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
            // Fail-closed (spec §13.1): tool harus ada di whitelist session ini.
            // Bila tidak -> 503 `tool_isolation_unavailable` (tidak pernah hold/execute).
            if !session.tools.iter().any(|t| t.name == name) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name);
                crate::metrics::METRICS.record_guard_violation();
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("tool_isolation_unavailable: {}", name),
                            "type": "invalid_request_error",
                            "code": "tool_isolation_unavailable",
                        }
                    })),
                ).into_response();
            }
            // Preflight isolation check (spec §13.2): verify guard is active for this bridge provider.
            if let Err(e) = registry.bridge.preflight_isolation_check(&name, &session.tools) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name, reason = %e);
                crate::metrics::METRICS.record_guard_violation();
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("tool_isolation_unavailable: {}", e),
                            "type": "invalid_request_error",
                            "code": "tool_isolation_unavailable",
                        }
                    })),
                ).into_response();
            }
            // Normalize tool name for bridge lookup (handles Read/read case-insensitivity).
            let normalized_name = crate::bridge::BridgeServer::normalize_tool_name(&name, &session.tools);
            if normalized_name != name {
                info!(event = "tool.normalized", session = %session.session_id, original = %name, normalized = %normalized_name);
            }
            let mcp_id = id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            info!(event = "bridge.call_held", mcp_req_id = %mcp_id, session = %session.session_id, tool = %normalized_name);
            let hold_start = Instant::now();
            // P0-AUDIT seq2: Pass run_id/tool_call_id when available (None for MCP hold path).
            let run_id: Option<String> = None;
            let tool_call_id: Option<String> = None;
            match registry.bridge.hold_tool_call(mcp_id.clone(), session.session_id.clone(), run_id, tool_call_id, normalized_name.clone(), registry.bridge_config.call_ttl_secs).await {
                Ok(res) => {
                    crate::metrics::METRICS.observe_tool_wait(hold_start.elapsed().as_millis() as u64);
                    serde_json::json!({
                        "content": [{ "type": "text", "text": res.content.to_string() }],
                        "isError": res.is_error
                    })
                }
                Err(e) => {
                // Handle Windows os error 5 / PermissionDenied with clear JSON instead of raw crash.
                let e_str = e.to_string();
                let is_permission_denied = e_str.contains("PermissionDenied")
                    || e_str.contains("os error 5")
                    || e_str.contains("access denied");
                let error_msg = if is_permission_denied {
                    format!("Read-folder access denied: {}", e_str)
                } else {
                    e_str
                };
                serde_json::json!({
                    "content": [{ "type": "text", "text": error_msg }],
                    "isError": true
                })
            }
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
    // Idempotent teardown: drop the session and wake any held tools/call.
    registry.bridge.remove_session(&session_id).await;
    axum::http::StatusCode::OK
}

async fn bridge_resolve(
    State(registry): State<Arc<ServiceRegistry>>,
    Path(call_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::http::StatusCode;

    // Same browser-origin hardening as the MCP endpoint.
    if headers.get("origin").is_some() {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"origin not allowed"}))).into_response();
    }

    // A held call is bound to the session that issued it; only that session's
    // bearer token may resolve it.
    let session = match registry.bridge.pending_session_id(&call_id).await {
        Some(session_id) => registry.bridge.get_session(&session_id).await,
        None => None,
    };
    let session = match session {
        Some(session) => session,
        None => {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "resolved": false,
                "error": { "code": "tool_already_resolved" }
            }))).into_response()
        }
    };

    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if !crate::bridge::BridgeServer::validate_token(&session, token) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"invalid token"}))).into_response();
    }

    let content = body.get("content").cloned().unwrap_or(serde_json::json!({}));
    let is_error = body.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let res = crate::bridge::ToolResult {
        tool_call_id: call_id.clone(),
        name: body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        content,
        is_error,
    };
    let run_id_opt = registry.bridge.resolve_tool_call(&call_id, res).await;
    if run_id_opt.is_none() {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({
            "resolved": false,
            "error": { "code": "tool_already_resolved" }
        }))).into_response();
    }
    Json(serde_json::json!({ "resolved": true, "run_id": run_id_opt.flatten() })).into_response()
}





/// How long an SSE stream may stay silent before it is terminated with an
/// explicit error chunk (default 120 s). Override with
/// `AGENTGATE_STREAM_IDLE_TIMEOUT_SECS`.
fn stream_idle_timeout() -> Duration {
    std::env::var("AGENTGATE_STREAM_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(120))
}
