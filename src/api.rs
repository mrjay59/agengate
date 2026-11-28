use crate::config::ReasoningConfig;
use crate::error::AppError;
use crate::gateway::admin as admin_api;
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
    routing::{get, patch, post, put},
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
        // Admin management plane (spec §8.2). All routes require admin scope
        // and return redacted payloads; see gateway::admin::authorize.
        .route("/admin/providers", get(admin_api::list_providers).post(admin_api::add_provider))
        .route("/admin/providers/*id", get(admin_api::get_provider).patch(admin_api::update_provider).delete(admin_api::delete_provider).post(admin_api::test_provider))
        .route("/admin/models", get(admin_api::list_models).post(admin_api::add_model))
        .route("/admin/models/sync", post(admin_api::sync_models))
        .route("/admin/models/*id", get(admin_api::get_model).patch(admin_api::update_model).delete(admin_api::delete_model))
        .route("/admin/combos", get(admin_api::list_combos).post(admin_api::add_combo))
        .route("/admin/combos/*id", get(admin_api::get_combo).patch(admin_api::update_combo).delete(admin_api::delete_combo))
        .route("/admin/proxies", get(admin_api::list_proxies).post(admin_api::add_proxy))
        .route("/admin/proxies/*id", get(admin_api::get_proxy).patch(admin_api::update_proxy).delete(admin_api::delete_proxy).post(admin_api::test_proxy))
        .route("/admin/pools", get(admin_api::list_pools).put(admin_api::put_pool))
        .route("/admin/pools/:name", put(admin_api::put_pool_named))
        .route("/admin/status", get(admin_api::gateway_status))
        // FIX-KEYSTORE / FIX-QUOTA-ACCOUNTING: key lifecycle + usage/quota.
        // Owned by Quota & Key Store; kept in its own block to avoid clashing
        // with the provider/model/proxy routes above.
        .route("/admin/keys", post(crate::admin::create_key).get(crate::admin::list_keys))
        .route("/admin/keys/:id", get(crate::admin::get_key).patch(crate::admin::patch_key_handler))
        .route("/admin/keys/:id/revoke", post(crate::admin::revoke_key_handler))
        .route("/admin/keys/:id/rotate", post(crate::admin::rotate_key_handler))
        .route("/admin/usage", get(crate::admin::usage))
        .route("/admin/quotas", get(crate::admin::quotas_get).patch(crate::admin::quotas_patch_defaults))
        .route("/admin/quotas/keys/:id", get(crate::admin::key_quota_get).patch(crate::admin::key_quota_patch))
        // Deprecated aliases kept for backward compatibility (spec §8.3).
        .route("/admin/provider", post(add_provider))
        .route("/admin/key", post(legacy_add_key))
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
        crate::metrics::init_metrics().record_guard_violation();
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
        crate::metrics::init_metrics().record_guard_violation();
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
        crate::metrics::init_metrics().render_prometheus_full(),
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
    /// FIX-TOOL-EXEC: names the client registered in this request. Used to
    /// fail-closed at SSE emit so a raw built-in `execute` never leaks to
    /// AionUI as an unknown tool.
    tool_names: Vec<String>,
    /// When bridge.emit_unregistered_tools is explicitly enabled, allow emitting
    /// tool calls not declared by the client nor known in the bridge session.
    /// Decoupled from passthrough_host_mcp: host MCP tools are executed locally
    /// by the CLI and must never be forwarded to a client that cannot run them.
    allow_unregistered_tools: bool,
    /// Tool names registered in the bridge session for this run (client tools + builtins).
    /// Used as an additional allowlist source for emit filtering.
    session_tool_names: Vec<String>,
}

impl ChunkContext {
    fn new(model: &str, tool_names: Vec<String>, allow_unregistered_tools: bool, session_tool_names: Vec<String>) -> Self {
        Self {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            tool_names,
            allow_unregistered_tools,
            session_tool_names,
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

/// Validate the requested tool count against the configured ceiling.
/// Returns the error message to surface when the count exceeds the limit.
fn validate_tool_count(count: usize, max_tools: usize) -> Result<(), String> {
    if count > max_tools {
        Err(format!(
            "too_many_tools ({} > {}; set [bridge.limits] max_tools to raise)",
            count, max_tools
        ))
    } else {
        Ok(())
    }
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

    // FIX-QUOTA-ACCOUNTING: capture raw request size before any mutation and
    // resolve the authenticated key identity (also works when middleware is
    // bypassed in unit tests).
    let content_in_bytes = serde_json::to_string(&request)
        .map(|s| s.len() as u64).unwrap_or(0);
    let mut key_identity = crate::keystore::identity_from_headers(&headers);
    if key_identity.is_none() {
        if let Some(store) = crate::keystore::keystore() {
            if store.quota_config().enforce_anonymous {
                key_identity = Some(crate::keystore::KeyStore::anonymous_identity());
            }
        }
    }
    let request_started = Instant::now();

    // Spec validation (spec_tooling §7.1)
    if request.n.unwrap_or(1) > 1 {
        return Err(AppError::ToolError("n > 1 is not supported".to_string()));
    }
    if let Some(tools) = &request.tools {
        // Use the configurable `[bridge.limits] max_tools` ceiling instead of
        // a hard-coded 128. Operators with tool-heavy clients (AionUI MCP
        // catalogs) can raise it without a code change; fail-closed remains.
        let max_tools = registry.bridge_config.limits.max_tools;
        if let Err(message) = validate_tool_count(tools.len(), max_tools) {
            return Err(AppError::ToolError(message));
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

    // Resolve model alias (e.g., "gpt-4o" → "agg/opencode/beam-free")
    let resolved_model = registry.resolve_model_alias(&request.model);
    if resolved_model != request.model {
        info!("Model alias resolved: {} -> {}", request.model, resolved_model);
    }

    let provider_id = match request.provider.clone() {
        Some(provider) => provider,
        None => registry
            .resolve_provider(&resolved_model)
            .await
            .ok_or_else(|| AppError::ModelNotFound(request.model.clone()))?,
    };

    // Update request.model to resolved model for downstream processing
    request.model = resolved_model.clone();

    // FIX-QUOTA-ACCOUNTING: enforce per-key/provider/model limits before any
    // provider work starts. The permit is released when this request (or its
    // spawned stream task) finishes, so concurrency is bounded in-process.
    let mut quota_permit = None;
    if let (Some(store), Some(identity)) = (crate::keystore::keystore(), key_identity.as_ref()) {
        if !identity.allows_provider(&provider_id) || !identity.allows_model(&resolved_model) {
            return Err(AppError::ScopeDenied(format!("provider={} model={}", provider_id, resolved_model)));
        }
        match store.check_quota(identity, &provider_id, &resolved_model, content_in_bytes) {
            Ok(permit) => quota_permit = permit,
            Err(d) => return Err(AppError::QuotaExceeded {
                reason: d.reason, scope: d.scope, reset_at: d.reset_at,
            }),
        }
    }

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
    crate::metrics::init_metrics().mark_run_started();
    info!(
        event = "run.start",
        run_id = %run_id,
        provider = %provider_id,
        model = %resolved_model,
        original_model = %request.model,
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
        let client_tool_names: Vec<String> = request
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(|t| t.function.name.clone()).collect())
            .unwrap_or_default();
        let allow_unregistered_tools = registry.bridge_config.emit_unregistered_tools;
        let registry_clone = registry.clone();
        let (mut events, session_tool_names) = registry_clone
            .stream_run_events(&provider_id, request)
            .await;
        let ctx = ChunkContext::new(&resolved_model, client_tool_names, allow_unregistered_tools, session_tool_names);
        let activity_via_reasoning = reasoning_cfg.activity_via_reasoning;
        let response_cfg = reasoning_cfg.clone();
        let stream_run_id = run_id_for_stream.take().unwrap_or_default();
        // FIX-QUOTA-ACCOUNTING: state carried into the stream task so usage is
        // recorded even when the client disconnects or the provider errors.
        let usage_provider = provider_id.clone();
        let usage_model = resolved_model.clone();
        let usage_mode = mode.to_string();
        let usage_identity = key_identity.clone();
        let usage_in_bytes = content_in_bytes;
        let usage_started = request_started;
        // Hold the concurrency slot for the whole stream, not just until the
        // SSE response headers are returned.
        let stream_quota_permit = quota_permit;

        tokio::spawn(async move {
            let _quota_permit = stream_quota_permit;
            let mut usage_in = 0u64;
            let mut usage_out = 0u64;
            let mut usage_out_bytes = 0u64;
            let mut usage_error: Option<String> = None;
            // FIX-TRANSPORT-BUILD: this was a closure that immutably borrowed
            // usage_in/usage_out/usage_out_bytes while the stream loop mutates
            // them (E0506). A local macro expands at the call site and reads
            // the current values without holding a borrow across mutations.
            macro_rules! record_stream_usage {
                ($error:expr) => {{
                    let error: Option<String> = $error;
                    let estimated = usage_in == 0 && usage_out == 0;
                    let (in_tokens, out_tokens) = if estimated {
                        ((usage_out_bytes / 4) as u64, 0u64)
                    } else {
                        (usage_in, usage_out)
                    };
                    record_request_usage(
                        usage_identity.as_ref(), &usage_provider, &usage_model, &usage_mode,
                        &stream_run_id, if error.is_some() { "error" } else { "ok" }, error,
                        usage_started.elapsed().as_millis() as u64,
                        in_tokens, out_tokens, estimated,
                        usage_in_bytes, usage_out_bytes, true,
                    );
                }};
            }
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
            //  - after the first event: max silence, default 120 s
            //    (`defaults.stream_idle_timeout_secs`, env AGENTGATE_STREAM_IDLE_TIMEOUT_SECS overrides)
            let first_token_timeout = registry_clone.first_token_timeout;
            let mut first_token_timer = Box::pin(tokio::time::sleep(first_token_timeout));
            let idle_timeout = registry_clone.stream_idle_timeout;
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
                                    crate::metrics::init_metrics().observe_ttft(ttft_start.elapsed().as_millis() as u64);
                                    ttft_recorded = true;
                                }
                                let is_tool_calls = matches!(event, RunEvent::ToolCalls { .. });
                                let is_error = matches!(event, RunEvent::Error { .. });
                                // FIX-QUOTA-ACCOUNTING: token usage comes from the
                                // upstream usage event; bytes are accumulated from
                                // the produced content as a always-available proxy.
                                match &event {
                                    RunEvent::Usage { prompt_tokens, completion_tokens, .. } => {
                                        usage_in = *prompt_tokens as u64;
                                        usage_out = *completion_tokens as u64;
                                    }
                                    RunEvent::Text { text } | RunEvent::ReasoningDelta { text, .. } => {
                                        usage_out_bytes += text.len() as u64;
                                    }
                                    RunEvent::ToolCalls { calls } => {
                                        usage_out_bytes += serde_json::to_string(calls)
                                            .map(|s| s.len() as u64).unwrap_or(0);
                                    }
                                    RunEvent::Error { message, .. } => {
                                        usage_error = Some(message.clone());
                                    }
                                    _ => {}
                                }
                                let mut emitted_tool_calls: usize = 0;
                                for processed in pipeline.process(event) {
                                    for coalesced in coalescer.add_event(processed) {
                                        emitted_tool_calls += emit_event(&tx, &ctx, &mut writer, coalesced, activity_via_reasoning).await;
                                    }
                                }
                                if is_error {
                                    terminated_by_error = true;
                                    let _ = tx
                                        .send(Ok(axum::response::sse::Event::default().data("[DONE]")))
                                        .await;
                                    break;
                                }
                                if is_tool_calls {
                                    if emitted_tool_calls > 0 {
                                        tool_calls_finished = true;
                                        break;
                                    } else {
                                        // All tool calls were skipped (host/local MCP,
                                        // raw builtin, or unregistered). The CLI may
                                        // have executed them locally and will keep
                                        // emitting. Do NOT send stop/[DONE] or break —
                                        // that is what made prompting stop.
                                        tracing::warn!(
                                            "all tool_calls skipped/not forwarded; continuing stream"
                                        );
                                    }
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
                        // Emit as normal content warning, NOT a top-level JSON "error"
                        // object — some clients (Aionrs) misparse that as
                        // UNKNOWN_UPSTREAM_ERROR "without an HTTP status".
                        let _ = send_sse(&tx, &ctx.chunk(serde_json::json!({
                            "content": format!("\n\n⚠️ Warning: {}", error_msg)
                        }), None)).await;
                        let _ = send_sse(&tx, &ctx.chunk(serde_json::json!({}), Some("stop"))).await;
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
                                "provider '{}' produced no output for {}s; stream idle timeout",
                                provider_id, idle_timeout.as_secs()
                            );
                            // Emit as normal content warning, NOT a top-level JSON "error"
                            // object — some clients (Aionrs) misparse that as
                            // UNKNOWN_UPSTREAM_ERROR "without an HTTP status".
                            let _ = send_sse(&tx, &ctx.chunk(serde_json::json!({
                                "content": format!("\n\n⚠️ Warning: {}", error_msg)
                            }), None)).await;
                            let _ = send_sse(&tx, &ctx.chunk(serde_json::json!({}), Some("stop"))).await;
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
                record_stream_usage!(usage_error.take().or_else(|| Some("stream_timeout".to_string())));
                crate::metrics::init_metrics().mark_run_ended();
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
            record_stream_usage!(usage_error.take());
            crate::metrics::init_metrics().mark_run_ended();
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
        let response = match tokio::time::timeout(registry.request_timeout, registry.chat_completion(request)).await {
            Ok(Ok(resp)) => {
                if let Some(mode_label) = resume_mode {
                    crate::metrics::init_metrics().record_resume(if mode_label == "replay" {
                        "replay"
                    } else {
                        "ok"
                    });
                }
                resp
            }
            Ok(Err(e)) => {
                if resume_mode.is_some() && e.code() == "tool_already_resolved" {
                    crate::metrics::init_metrics().record_resume("conflict");
                }
                // FIX-QUOTA-ACCOUNTING: failed requests still count toward RPD.
                record_request_usage(
                    key_identity.as_ref(), &provider_id, &resolved_model, mode, &run_id,
                    "error", Some(e.code().to_string()),
                    request_started.elapsed().as_millis() as u64,
                    0, 0, false, content_in_bytes, 0, false,
                );
                crate::metrics::init_metrics().mark_run_ended();
                return Err(e);
            }
            Err(_) => {
                record_request_usage(
                    key_identity.as_ref(), &provider_id, &resolved_model, mode, &run_id,
                    "error", Some("request_timeout".to_string()),
                    request_started.elapsed().as_millis() as u64,
                    0, 0, false, content_in_bytes, 0, false,
                );
                crate::metrics::init_metrics().mark_run_ended();
                return Err(AppError::Timeout);
            }
        };
        let upstream_usage = response.usage.clone().unwrap_or_default();
        let value = apply_non_stream_format(response, format, &reasoning_cfg);
        // FIX-QUOTA-ACCOUNTING: upstream usage wins; fall back to a chars/4
        // estimator and flag it so reports can distinguish the two.
        let content_out_bytes = serde_json::to_string(&value)
            .map(|s| s.len() as u64).unwrap_or(0);
        let estimated = upstream_usage.total_tokens == 0;
        let (input_tokens, output_tokens) = if estimated {
            ((content_out_bytes / 4) as u64, 0u64)
        } else {
            (upstream_usage.prompt_tokens as u64, upstream_usage.completion_tokens as u64)
        };
        record_request_usage(
            key_identity.as_ref(), &provider_id, &resolved_model, mode, &run_id,
            "ok", None, request_started.elapsed().as_millis() as u64,
            input_tokens, output_tokens, estimated,
            content_in_bytes, content_out_bytes, false,
        );
        crate::metrics::init_metrics().mark_run_ended();
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

/// FIX-QUOTA-ACCOUNTING: persist one usage row + update metrics. Called from
/// both the stream and non-stream paths; a no-op when no key store is active.
fn record_request_usage(
    identity: Option<&crate::keystore::KeyIdentity>,
    provider: &str,
    model: &str,
    mode: &str,
    run_id: &str,
    status: &str,
    error_class: Option<String>,
    latency_ms: u64,
    input_tokens: u64,
    output_tokens: u64,
    tokens_estimated: bool,
    content_in_bytes: u64,
    content_out_bytes: u64,
    stream: bool,
) {
    let store = match crate::keystore::keystore() {
        Some(s) => s,
        None => return,
    };
    let total_tokens = input_tokens.saturating_add(output_tokens);
    store.record_usage(crate::keystore::UsageRecord {
        request_id: run_id.to_string(),
        ts: chrono::Utc::now().timestamp(),
        key_id: identity.map(|i| i.id.clone()).unwrap_or_else(|| "anonymous".to_string()),
        provider_id: provider.to_string(),
        model: model.to_string(),
        upstream_model: None,
        mode: mode.to_string(),
        status: status.to_string(),
        error_class,
        latency_ms,
        input_tokens,
        output_tokens,
        total_tokens,
        tokens_estimated,
        content_in_bytes,
        content_out_bytes,
        stream,
    });
}

fn add_reasoning_headers(
    res: &mut Response,
    format: ReasoningFormat,
    _cfg: &ReasoningConfig,
    granularity: &str,
) {
    let kind = if format == ReasoningFormat::None {
        ReasoningKind::Omitted
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

/// Split a string into chunks of at most `max_bytes` bytes, never splitting a
/// UTF-8 code point. Returns string slices in original order.
fn utf8_chunks(s: &str, max_bytes: usize) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let len = s.len();
    while start < len {
        let mut end = (start + max_bytes).min(len);
        while end > start && !s.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            // Should not happen when max_bytes >= 4, but advance by one char.
            if let Some(ch) = s[start..].chars().next() {
                end = start + ch.len_utf8();
            } else {
                break;
            }
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}

/// Emits a single `RunEvent` as SSE chunks. Returns the number of tool calls
/// actually emitted to the client (0 for non-ToolCalls events). Callers use
/// this to avoid terminating a stream with `finish_reason:"tool_calls"` when
/// every tool call was dropped by fail-closed filtering.
async fn emit_event(
    tx: &mpsc::Sender<Result<axum::response::sse::Event, axum::Error>>,
    ctx: &ChunkContext,
    writer: &mut SseWriter,
    event: RunEvent,
    _activity_via_reasoning: bool,
) -> usize {
    match event {
        RunEvent::Text { text } => {
            for delta in writer.text_deltas(&text) {
                send_delta(tx, ctx, delta).await;
            }
            0
        }
        RunEvent::ReasoningDelta { text, .. } => {
            for delta in writer.reasoning_deltas(&text) {
                send_delta(tx, ctx, delta).await;
            }
            0
        }
        RunEvent::ReasoningEnd { .. } => {
            for delta in writer.reasoning_end_deltas() {
                send_delta(tx, ctx, delta).await;
            }
            0
        }
        RunEvent::Activity { label, .. } => {
            send_delta(tx, ctx, serde_json::json!({ "activity": label })).await;
            0
        }
        RunEvent::ToolCalls { calls } => {
            for delta in writer.close_think() {
                send_delta(tx, ctx, delta).await;
            }
            let mut emitted_index = 0usize;
            let mut dropped: Vec<String> = Vec::new();
            for call in calls.iter() {
                let name = call.function.name.clone();
                // Normalize once: stacked MCP prefixes (agentgate_agentgate_...)
                // and casing must not hide a legitimately registered tool.
                let normalized =
                    crate::bridge::BridgeServer::strip_mcp_tool_prefix(&name).to_ascii_lowercase();
                let all_names = ctx.tool_names.iter().chain(ctx.session_tool_names.iter());
                let registered = all_names.clone().any(|n| {
                    crate::bridge::BridgeServer::strip_mcp_tool_prefix(n)
                        .eq_ignore_ascii_case(&normalized)
                });
                // A CLI built-in (e.g. opencode's `Bash`) may be the local
                // equivalent of a client tool with a different name (AionUI
                // registers `ExecCommand`, GitHub Copilot registers `execute`).
                // Map it to the client-registered name instead of dropping it,
                // so the client can execute the call under a tool it knows.
                let mut emitted_name = name.clone();
                if !registered {
                    let aliases: &[&str] = match normalized.as_str() {
                        "bash" | "shell" | "execute" | "exec" =>
                            &["execcommand", "exec_command", "execute", "bash", "shell"],
                        "read" => &["read_file", "read"],
                        "write" => &["write_file", "write"],
                        "edit" => &["edit_file", "edit"],
                        "list" | "glob" => &["list_dir", "list", "glob"],
                        _ => &[],
                    };
                    if let Some(client_name) = aliases.iter().find_map(|alias| {
                        all_names.clone().find(|n| n.eq_ignore_ascii_case(alias))
                    }) {
                        emitted_name = client_name.clone();
                        tracing::info!(tool = %name, mapped_to = %emitted_name,
                            "mapped CLI built-in tool to client-registered name");
                    }
                }
                let registered = registered || emitted_name != name;
                let blocked_builtin = matches!(normalized.as_str(),
                    "execute" | "bash" | "shell" | "write" | "read" | "edit" | "patch" | "list" | "glob"
                );
                if !registered {
                    if ctx.allow_unregistered_tools && !blocked_builtin {
                        tracing::warn!(
                            tool = %name,
                            blocked_builtin,
                            passthrough = ctx.allow_unregistered_tools,
                            "Emitting unregistered tool_call with warning (not in client tools[])"
                        );
                    } else {
                        // Not client-executable: host MCP tools (vps_*, ...)
                        // are executed locally by opencode via passthrough, and
                        // raw builtins are never safe to forward. Skip silently
                        // (no tool_call delta) but DO NOT push to `dropped`:
                        // treating this as fatal terminates the turn even though
                        // the CLI already executed the tool and will continue.
                        tracing::warn!(
                            tool = %name,
                            blocked_builtin,
                            "Skipping unregistered tool_call (not forwarded; likely host/local MCP); stream continues"
                        );
                        crate::metrics::init_metrics().record_guard_violation();
                        continue;
                    }
                }
                let index = emitted_index;
                emitted_index += 1;
                send_delta(tx, ctx, serde_json::json!({
                    "tool_calls": [{
                        "index": index,
                        "id": call.id,
                        "type": call.r#type,
                        "function": { "name": emitted_name, "arguments": "" }
                    }]
                })).await;
                let args = call.function.arguments.as_str();
                for chunk in utf8_chunks(args, 512) {
                    send_delta(tx, ctx, serde_json::json!({
                        "tool_calls": [{
                            "index": index,
                            "function": { "arguments": chunk }
                        }]
                    })).await;
                }
            }
            if emitted_index == 0 && !dropped.is_empty() {
                // Non-fatal: never terminate a turn because a tool call was
                // skipped. The CLI may have executed it locally; stopping here
                // is what caused "prompting stops" for host MCP tools.
                let msg = format!(
                    "Agent requested tool(s) not registered by client: {}. Skipped (not forwarded).",
                    dropped.join(", ")
                );
                tracing::warn!(dropped_tools = %dropped.join(","), "{}", msg);
                let _ = send_sse(tx, &ctx.chunk(serde_json::json!({
                    "content": format!("\n\n⚠️ Warning: {}", msg)
                }), None)).await;
            }
            emitted_index
        }
        RunEvent::Usage { prompt_tokens, completion_tokens, total_tokens, .. } => {
            let chunk = serde_json::json!({
                "id": ctx.id,
                "object": "chat.completion.chunk",
                "created": ctx.created,
                "model": ctx.model,
                "choices": [],
                "usage": { "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens, "total_tokens": total_tokens }
            });
            let _ = send_sse(tx, &chunk.to_string()).await;
            0
        }
        RunEvent::Error { message, .. } => {
            let _ = send_sse(tx, &ctx.chunk(serde_json::json!({
                "content": format!("\n\n⚠️ Error: {}", message)
            }), None)).await;
            let _ = send_sse(tx, &ctx.chunk(serde_json::json!({}), Some("error"))).await;
            0
        }
        RunEvent::ReasoningStart { .. } | RunEvent::Done { .. } => 0,
    }
}

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

/// Deprecated alias: POST /admin/provider -> POST /admin/providers.
async fn add_provider(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(request): Json<AddProviderRequest>,
) -> axum::response::Response {
    let id = request.id.clone();
    let payload = serde_json::json!({
        "id": id,
        "name": id,
        "base_url": request.base_url,
        "provider_kind": request.provider_kind,
        "mode": "auto",
    });
    let mut response = admin_api::add_provider(headers, Json(payload)).await;
    admin_api::deprecation_headers(&mut response, "/admin/providers");
    response
}

async fn add_key(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddKeyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add key for provider {:?}", request.provider);
    Ok(Json(serde_json::json!({"status": "ok", "provider": request.provider})))
}

/// Deprecated alias: POST /admin/key -> POST /admin/keys.
async fn legacy_add_key(
    State(registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(request): Json<AddKeyRequest>,
) -> axum::response::Response {
    // FIX-KEYSTORE: delegate to the real lifecycle-backed create handler.
    let mut response = match crate::admin::legacy_add_key(State(registry), headers, Json(request)).await {
        Ok(v) => v.into_response(),
        Err(e) => e.into_response(),
    };
    admin_api::deprecation_headers(&mut response, "/admin/keys");
    response
}

/// Deprecated alias: POST /admin/proxy -> POST /admin/proxies.
/// The legacy shape is a URL probe, not a proxy config; keep it side-effect
/// free and redact credentials from the echoed target.
async fn proxy(
    State(_registry): State<Arc<ServiceRegistry>>,
    headers: HeaderMap,
    Json(request): Json<ProxyRequest>,
) -> axum::response::Response {
    if let Err(response) = admin_api::authorize(&headers) { return response; }
    let mut response = if crate::gateway::store::validate_http_url(&request.target, true).is_ok() {
        (
            axum::http::StatusCode::OK,
            Json(serde_json::json!({
                "status": "ok",
                "target": crate::gateway::store::redact_url(&request.target),
                "method": request.method,
            })),
        ).into_response()
    } else {
        admin_api::error_response(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_request",
            "target must be an http(s) URL",
        )
    };
    admin_api::deprecation_headers(&mut response, "/admin/proxies");
    response
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

            // Normalize MCP-namespaced tool names (e.g. agentgate_glob / agentgate__glob)
            // back to the canonical tool registered in this session before whitelist/guard checks.
            let normalized_name = crate::bridge::BridgeServer::normalize_tool_name(&name, &session.tools);
            if normalized_name != name {
                info!(event = "tool.normalized", session = %session.session_id, original = %name, normalized = %normalized_name);
            }

            // Fail-closed (spec §13.1): tool harus ada di whitelist session ini.
            // Bila tidak -> 503 `tool_isolation_unavailable` (tidak pernah hold/execute).
            if !session.tools.iter().any(|t| t.name == normalized_name) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name, normalized = %normalized_name);
                crate::metrics::init_metrics().record_guard_violation();
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
            if let Err(e) = registry.bridge.preflight_isolation_check(&normalized_name, &session.tools) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name, normalized = %normalized_name, reason = %e);
                crate::metrics::init_metrics().record_guard_violation();
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

            // Preflight active probe (L3): validate against isolation config allowed_builtins
            let isolation = registry.bridge_config.isolation.clone();
            if let Err(e) = registry.bridge.preflight_client_tools(&session.tools, &isolation) {
                info!(event = "guard.violation", session = %session.session_id, observed_tool = %name, normalized = %normalized_name, reason = %e);
                crate::metrics::init_metrics().record_guard_violation();
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

            // FIX-BUILTIN-TOOLS: agentgate built-in discovery tools (check_mcp_local,
            // skill_local) are executed locally inside the gateway and never forwarded
            // to the client. Return their result directly without holding/parking.
            if normalized_name == "check_mcp_local" || normalized_name == "skill_local" {
                let args = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                let result = crate::bridge::execute_builtin_tool(&normalized_name, &args).await;
                let resp = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });
                return Json(resp).into_response();
            }

            let mcp_id = id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            info!(event = "bridge.call_held", mcp_req_id = %mcp_id, session = %session.session_id, tool = %normalized_name);
            let hold_start = Instant::now();
            // FIX-TOOL-ROUNDTRIP: surface the held MCP tool call to the client
            // stream. Previously this path only held the call and waited for
            // /_bridge/resolve, but the client never received a tool_calls
            // chunk (and had no way to learn the mcp_id), so the turn hung.
            let tool_call_id = crate::openai::TOOL_CALL_ID_GENERATOR.lock().unwrap_or_else(|e| e.into_inner()).generate();
            registry.bridge.emit_tool_call_event(
                &session.session_id,
                serde_json::json!({
                    "type": "tool_calls",
                    "calls": [{
                        "id": tool_call_id.clone(),
                        "type": "function",
                        "function": {
                            "name": normalized_name.clone(),
                            "arguments": _args.to_string()
                        }
                    }]
                }),
            );
            match registry.bridge.hold_tool_call(
                mcp_id.clone(),
                session.session_id.clone(),
                None,
                Some(tool_call_id),
                normalized_name.clone(),
                registry.bridge_config.call_ttl_secs,
            ).await {
                Ok(res) => {
                    crate::metrics::init_metrics().observe_tool_wait(hold_start.elapsed().as_millis() as u64);
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





// stream idle timeout is plumbed from DefaultsConfig.stream_idle_timeout_secs
// (env AGENTGATE_STREAM_IDLE_TIMEOUT_SECS overrides) via ServiceRegistry.stream_idle_timeout.

#[cfg(test)]
mod tool_limit_tests {
    use super::validate_tool_count;

    #[test]
    fn tool_count_uses_configured_ceiling() {
        // 129 tools must pass when the operator raises max_tools above 128.
        assert!(validate_tool_count(129, 256).is_ok());
        assert!(validate_tool_count(256, 256).is_ok());
    }

    #[test]
    fn tool_count_fail_closed_with_actionable_message() {
        let err = validate_tool_count(129, 128).unwrap_err();
        assert!(err.starts_with("too_many_tools (129 > 128"));
        assert!(err.contains("max_tools"));
        assert!(validate_tool_count(128, 128).is_ok());
    }
}

#[cfg(test)]
mod sse_tool_arg_chunking_tests {
    use super::utf8_chunks;

    #[test]
    fn utf8_chunks_never_split_multibyte_char_at_boundary() {
        // Regression for ExecCommand/code truncation: a 512-byte raw chunk
        // boundary landed inside '€' (3 bytes), and from_utf8(...).unwrap_or("")
        // dropped that chunk silently.
        let prefix = "a".repeat(511);
        let suffix = "b".repeat(20);
        let args = format!("{prefix}€{suffix}");
        assert_eq!("€".len(), 3);
        assert!(args.as_bytes().len() > 512);

        let chunks = utf8_chunks(&args, 512);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(c.len() <= 512, "chunk exceeds max bytes: {}", c.len());
            assert!(std::str::from_utf8(c.as_bytes()).is_ok());
        }
        let joined: String = chunks.concat();
        assert_eq!(joined, args, "arguments must round-trip without loss");
        assert!(joined.contains('€'));
    }

    #[test]
    fn utf8_chunks_handles_exact_boundary_and_empty() {
        assert!(utf8_chunks("", 512).is_empty());
        let s = "abc";
        assert_eq!(utf8_chunks(s, 512), vec!["abc"]);
        let s = "a".repeat(512);
        let chunks = utf8_chunks(&s, 512);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 512);
        let s = "a".repeat(513);
        let chunks = utf8_chunks(&s, 512);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 512);
        assert_eq!(chunks[1].len(), 1);
    }
}





