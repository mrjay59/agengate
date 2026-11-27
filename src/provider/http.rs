//! HTTP OpenAI-compatible upstream provider.
//!
//! This provider performs plain `POST {base_url}/chat/completions` requests and
//! never spawns a CLI process. It supports JSON and SSE responses, bounded
//! retries, per-provider proxy selection, response normalization and structured
//! error classification so the router can failover on quota/rate-limit/5xx.

use crate::config::ProviderConfig;
use crate::provider::mappings::{is_nvidia_api_key, OPENAI_COMPATIBLE_ADAPTER};
use crate::reasoning::{FinishReason, ReasoningKind, RunEvent};
use crate::transport::{
    classify_status, retry_after_from_headers, retry_with_backoff, SseParser, TransportContext,
    TransportErrorClass, TransportFailure, RetryPolicy,
};
use crate::types::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Choice, FunctionCall, MessageRole,
    ToolCall, Usage,
};
use anyhow::anyhow;
use futures::StreamExt;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Classified upstream failure. Implements `std::error::Error` so callers can
/// use `?` in `anyhow::Result` contexts without losing the error class.
#[derive(Debug, Clone)]
pub struct HttpError {
    pub class: TransportErrorClass,
    pub status: Option<u16>,
    pub message: String,
}

impl HttpError {
    pub fn new(class: TransportErrorClass, message: impl Into<String>) -> Self {
        Self { class, status: None, message: message.into() }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn is_quota(&self) -> bool {
        self.class.is_quota()
    }

    pub fn is_retryable(&self) -> bool {
        self.class.is_retryable()
    }

    pub fn is_rate_limited(&self) -> bool {
        self.status == Some(429) || self.is_quota()
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "http_provider[{}] HTTP {}: {}", self.class, status, self.message),
            None => write!(f, "http_provider[{}]: {}", self.class, self.message),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<TransportFailure> for HttpError {
    fn from(failure: TransportFailure) -> Self {
        Self::new(failure.class, failure.message)
    }
}

/// Resolve the upstream key from `api_key_env` (preferred) or inline `api_key`.
/// Provider-specific prefix validation (e.g. NVIDIA `nvapi-`) fails closed.
pub fn resolve_api_key(config: &ProviderConfig) -> anyhow::Result<String> {
    let mut key = None;
    if let Some(env) = config.api_key_env.as_deref().filter(|s| !s.trim().is_empty()) {
        if let Ok(value) = std::env::var(env) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                key = Some(value);
            }
        }
    }
    let key = key.or_else(|| {
        config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });

    let key = key.ok_or_else(|| {
        anyhow!(
            "HTTP provider '{}' has no API key: set {} (or api_key)",
            config.id,
            config.api_key_env.as_deref().unwrap_or("<unset>")
        )
    })?;

    if let Some(prefix) = config.api_key_prefix.as_deref().filter(|s| !s.is_empty()) {
        if !key.starts_with(prefix) {
            return Err(anyhow!(
                "HTTP provider '{}' API key must start with '{}'",
                config.id,
                prefix
            ));
        }
    }
    if key.starts_with("nvapi-") && !is_nvidia_api_key(&key) {
        return Err(anyhow!("invalid NVIDIA NIM API key"));
    }
    Ok(key)
}

/// Build the outbound OpenAI-compatible JSON body.
///
/// Internal `agentgate_*` fields are never forwarded (they are `#[serde(skip)]`
/// on `ChatCompletionRequest`, and are not read here). Provider-specific fields
/// are gated by the `forward_*` flags — NIM rejects undeclared fields with 422.
pub fn build_request_body(
    config: &ProviderConfig,
    upstream_model: &str,
    request: &ChatCompletionRequest,
    stream: bool,
) -> anyhow::Result<Value> {
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(upstream_model.to_string()));
    body.insert(
        "messages".to_string(),
        serde_json::to_value(&request.messages).map_err(|e| anyhow!("serialize messages: {}", e))?,
    );
    body.insert("stream".to_string(), Value::Bool(stream));

    if let Some(value) = request.temperature {
        body.insert("temperature".to_string(), serde_json::json!(value));
    }
    if let Some(value) = request.top_p {
        body.insert("top_p".to_string(), serde_json::json!(value));
    }
    if let Some(value) = request.max_tokens {
        body.insert("max_tokens".to_string(), serde_json::json!(value));
    }
    if config.forward_tools {
        if let Some(tools) = &request.tools {
            body.insert("tools".to_string(), serde_json::to_value(tools)?);
        }
    }
    if config.forward_tool_choice {
        if let Some(choice) = &request.tool_choice {
            body.insert("tool_choice".to_string(), choice.clone());
        }
    }
    if config.forward_response_format {
        if let Some(format) = &request.response_format {
            body.insert("response_format".to_string(), format.clone());
        }
    }
    Ok(Value::Object(body))
}

fn join_path(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.is_empty() {
        return base.to_string();
    }
    if path.starts_with('/') {
        format!("{}{}", base, path)
    } else {
        format!("{}/{}", base, path)
    }
}

fn build_client(config: &ProviderConfig, proxy_url: Option<&str>) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs.max(1)));
    if !config.verify_tls {
        builder = builder.danger_accept_invalid_certs(true);
    }
    if let Some(url) = proxy_url.filter(|s| !s.trim().is_empty()) {
        let proxy = reqwest::Proxy::all(url)
            .map_err(|e| anyhow!("invalid proxy url for '{}': {}", config.id, e))?;
        builder = builder.proxy(proxy);
    }
    Ok(builder.build()?)
}

fn retry_policy_from(config: &ProviderConfig) -> RetryPolicy {
    let base_delay = config
        .retry_backoff_ms
        .first()
        .copied()
        .unwrap_or(250);
    let max_delay = config
        .retry_backoff_ms
        .iter()
        .copied()
        .max()
        .unwrap_or(4000)
        .max(base_delay);
    RetryPolicy {
        max_retries: config.max_retries as usize,
        base_delay: Duration::from_millis(base_delay),
        max_delay: Duration::from_millis(max_delay),
        jitter: false,
    }
}

/// A direct HTTP OpenAI-compatible upstream.
pub struct HttpOpenAiCompatibleProvider {
    config: ProviderConfig,
    client: reqwest::Client,
    chat_url: String,
    models_url: String,
    api_key: String,
    retry_policy: RetryPolicy,
}

// Minimal normalizers for upstream responses that omit optional OpenAI fields.
#[derive(Debug, Deserialize)]
struct UpstreamResponse {
    #[serde(default)]
    id: String,
    #[serde(default = "default_chat_object")]
    object: String,
    #[serde(default)]
    created: u64,
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<UpstreamChoice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct UpstreamChoice {
    #[serde(default)]
    index: u32,
    message: UpstreamMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpstreamMessage {
    #[serde(default = "default_assistant")]
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    tool_call_id: Option<String>,
}

fn default_chat_object() -> String { "chat.completion".to_string() }
fn default_assistant() -> String { "assistant".to_string() }

/// Some upstreams (e.g. NVIDIA NIM) only require `id` and `choices` on stream
/// chunks, so optional OpenAI fields get defaults during normalization.
#[derive(Debug, Deserialize)]
struct UpstreamChunk {
    #[serde(default)]
    id: String,
    #[serde(default = "default_chat_object")]
    object: String,
    #[serde(default)]
    created: u64,
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<crate::types::ChatCompletionChunkChoice>,
    usage: Option<Usage>,
}

#[derive(Debug, Default)]
struct ToolCallAccum {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

impl HttpOpenAiCompatibleProvider {
    /// Construct a provider from config. Returns an error for malformed URLs,
    /// missing keys, or unsupported adapters — no CLI process is involved.
    pub fn new(config: ProviderConfig) -> anyhow::Result<Self> {
        let adapter = if config.adapter.is_empty() {
            OPENAI_COMPATIBLE_ADAPTER.to_string()
        } else {
            config.adapter.clone()
        };
        if adapter != OPENAI_COMPATIBLE_ADAPTER {
            return Err(anyhow!("unsupported HTTP adapter '{}'", adapter));
        }

        let base_url = config
            .base_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("HTTP provider '{}' requires base_url", config.id))?;
        let parsed = reqwest::Url::parse(base_url)
            .map_err(|e| anyhow!("invalid base_url for '{}': {}", config.id, e))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(anyhow!(
                "base_url for '{}' must use http/https (got '{}')",
                config.id,
                parsed.scheme()
            ));
        }
        if config.mapping_status == "unverified" {
            tracing::warn!(event = "http_provider.unverified_enabled", provider_id = %config.id,
                "Provider mapping is unverified; enabling it is an explicit operator decision");
        }

        let chat_url = join_path(base_url, &config.chat_completions_path);
        let models_url = join_path(base_url, &config.models_path);
        let api_key = resolve_api_key(&config)?;

        let retry_policy = retry_policy_from(&config);
        let client = build_client(&config, None)?;

        Ok(Self {
            config,
            client,
            chat_url,
            models_url,
            api_key,
            retry_policy,
        })
    }

    pub fn id(&self) -> &str { &self.config.id }
    pub fn base_url(&self) -> &str { &self.chat_url }
    /// True: this provider never spawns a CLI/bridge process.
    pub fn is_http_only(&self) -> bool { true }
    pub fn forward_tools(&self) -> bool { self.config.forward_tools }

    /// Pull the upstream catalog (`GET /models`). Returns upstream model ids.
    pub async fn fetch_models(&self) -> Result<Vec<String>, HttpError> {
        let provider = self.config.id.as_str();
        let started = Instant::now();
        let metrics = crate::metrics::init_metrics();
        let ctx = TransportContext { provider, model: "*", target: &self.models_url, mode: "http" };

        let builder = self
            .client
            .get(&self.models_url)
            .bearer_auth(&self.api_key)
            .header(CONTENT_TYPE, "application/json");
        let template = builder.try_clone().ok_or_else(|| {
            HttpError::new(TransportErrorClass::Other, "model-list request is not cloneable")
        })?;

        let response = retry_with_backoff(&self.retry_policy, ctx, |_| {
            let clone_opt = template.try_clone();
            async move {
                let clone = match clone_opt {
                    Some(clone) => clone,
                    None => {
                        return Err(TransportFailure::new(
                            TransportErrorClass::Other,
                            "request is not cloneable",
                        ));
                    }
                };
                let response = clone.send().await.map_err(|e| TransportFailure::from_reqwest(&e))?;
                let status = response.status();
                if status.is_success() {
                    Ok(response)
                } else {
                    let mut failure = TransportFailure::new(
                        classify_status(status.as_u16()),
                        format!("upstream returned HTTP {}", status.as_u16()),
                    );
                    if let Some(delay) = retry_after_from_headers(response.headers()) {
                        failure = failure.with_retry_after(delay);
                    }
                    Err(failure)
                }
            }
        })
        .await;

        metrics.observe_http_upstream_latency(provider, "*", started.elapsed().as_millis() as u64);
        let response = match response {
            Ok(response) => {
                metrics.record_http_upstream_request(provider, "*", &self.models_url, "ok", "http");
                response
            }
            Err(err) => {
                metrics.record_http_upstream_request(provider, "*", &self.models_url, "error", "http");
                metrics.record_http_upstream_error(provider, "*", err.class.as_str());
                return Err(HttpError::from(err));
            }
        };

        let payload: Value = response.json().await.map_err(|e| {
            HttpError::new(TransportErrorClass::Other, format!("invalid /models response: {}", e))
        })?;
        let models = payload
            .get("data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(models)
    }

    /// Non-streaming chat completion.
    pub async fn chat(
        &self,
        upstream_model: &str,
        request: &ChatCompletionRequest,
        sticky_key: Option<&str>,
    ) -> Result<ChatCompletionResponse, HttpError> {
        let body = build_request_body(&self.config, upstream_model, request, false)
            .map_err(|e| HttpError::new(TransportErrorClass::Other, e.to_string()))?;
        let response = self.send(body, upstream_model, sticky_key).await?;
        let value: Value = response.json().await.map_err(|e| {
            HttpError::new(TransportErrorClass::Other, format!("invalid chat response: {}", e))
        })?;
        self.normalize_response(value, upstream_model)
    }

    /// Streaming chat completion: decode SSE chunks into [`RunEvent`]s.
    /// Never emits a `Done` twice, and preserves tool-call deltas.
    pub async fn stream_events(
        &self,
        upstream_model: &str,
        request: &ChatCompletionRequest,
        tx: mpsc::Sender<RunEvent>,
        sticky_key: Option<&str>,
    ) -> Result<(), HttpError> {
        let body = build_request_body(&self.config, upstream_model, request, true)
            .map_err(|e| HttpError::new(TransportErrorClass::Other, e.to_string()))?;
        let response = self.send(body, upstream_model, sticky_key).await?;
        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();
        let mut buffer = String::new();
        let mut done_emitted = false;
        let mut reasoning_block = 0u32;
        let mut reasoning_open = false;
        let mut tool_calls: BTreeMap<u32, ToolCallAccum> = BTreeMap::new();
        let idle_timeout = Duration::from_secs(self.config.stream_idle_timeout_secs.max(1));

        let handle_event = |event: crate::transport::SseEvent,
                            tx: &mpsc::Sender<RunEvent>,
                            done_emitted: &mut bool,
                            reasoning_open: &mut bool,
                            reasoning_block: &mut u32,
                            tool_calls: &mut BTreeMap<u32, ToolCallAccum>| {
            let data = event.data;
            if data.trim() == "[DONE]" {
                if !*done_emitted {
                    let _ = tx.try_send(RunEvent::Done { reason: FinishReason::Stop });
                    *done_emitted = true;
                }
                return;
            }
            let chunk: UpstreamChunk = match serde_json::from_str(&data) {
                Ok(chunk) => chunk,
                Err(_) => return, // ignore keep-alives / non-chunk events
            };
            if let Some(usage) = chunk.usage {
                let _ = tx.try_send(RunEvent::Usage {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    total_tokens: usage.total_tokens,
                });
            }
            for choice in chunk.choices {
                if let Some(reasoning) = choice.delta.reasoning_content.filter(|s| !s.is_empty()) {
                    if !*reasoning_open {
                        let _ = tx.try_send(RunEvent::ReasoningStart {
                            block: *reasoning_block,
                            kind: ReasoningKind::Raw,
                        });
                        *reasoning_open = true;
                    }
                    let _ = tx.try_send(RunEvent::ReasoningDelta {
                        block: *reasoning_block,
                        text: reasoning,
                    });
                }
                if let Some(content) = choice.delta.content.filter(|s| !s.is_empty()) {
                    if *reasoning_open {
                        let _ = tx.try_send(RunEvent::ReasoningEnd { block: *reasoning_block });
                        *reasoning_open = false;
                        *reasoning_block += 1;
                    }
                    let _ = tx.try_send(RunEvent::Text { text: content });
                }
                if let Some(deltas) = choice.delta.tool_calls {
                    for delta in deltas {
                        let entry = tool_calls.entry(delta.index).or_insert_with(|| ToolCallAccum {
                            kind: "function".to_string(),
                            ..Default::default()
                        });
                        if let Some(id) = delta.id { entry.id = id; }
                        if let Some(kind) = delta.r#type { entry.kind = kind; }
                        if let Some(function) = delta.function {
                            if let Some(name) = function.name { entry.name = name; }
                            if let Some(args) = function.arguments { entry.arguments.push_str(&args); }
                        }
                    }
                }
                if let Some(reason) = choice.finish_reason {
                    let finish = match reason.as_str() {
                        "length" => FinishReason::Length,
                        "tool_calls" => FinishReason::ToolCalls,
                        "content_filter" => FinishReason::ContentFilter,
                        _ => FinishReason::Stop,
                    };
                    if !tool_calls.is_empty() {
                        let calls = tool_calls.values().map(|entry| ToolCall {
                            id: entry.id.clone(),
                            r#type: entry.kind.clone(),
                            function: FunctionCall {
                                name: entry.name.clone(),
                                arguments: entry.arguments.clone(),
                            },
                        }).collect::<Vec<_>>();
                        let _ = tx.try_send(RunEvent::ToolCalls { calls });
                        tool_calls.clear();
                    }
                    if !*done_emitted {
                        let _ = tx.try_send(RunEvent::Done { reason: finish });
                        *done_emitted = true;
                    }
                }
            }
        };

        loop {
            let next = tokio::time::timeout(idle_timeout, stream.next()).await;
            let item = match next {
                Ok(item) => item,
                Err(_) => {
                    return Err(HttpError::new(
                        TransportErrorClass::Timeout,
                        format!("upstream stream idle for >{}s", idle_timeout.as_secs()),
                    ));
                }
            };
            match item {
                Some(Ok(bytes)) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    for event in parser.push(&buffer) {
                        handle_event(event, &tx, &mut done_emitted, &mut reasoning_open,
                            &mut reasoning_block, &mut tool_calls);
                    }
                    buffer.clear();
                }
                Some(Err(error)) => {
                    return Err(HttpError::new(
                        TransportFailure::from_reqwest(&error).class,
                        format!("upstream stream failed: {}", error),
                    ));
                }
                None => break,
            }
        }
        for event in parser.finish() {
            handle_event(event, &tx, &mut done_emitted, &mut reasoning_open,
                &mut reasoning_block, &mut tool_calls);
        }
        if !done_emitted {
            let _ = tx.try_send(RunEvent::Done { reason: FinishReason::Stop });
        }
        Ok(())
    }

    /// Send a JSON body with bounded retries and the configured proxy hook.
    async fn send(
        &self,
        body: Value,
        model: &str,
        sticky_key: Option<&str>,
    ) -> Result<reqwest::Response, HttpError> {
        let provider = self.config.id.as_str();
        let endpoint = crate::gateway::proxy::select_for_provider(provider, sticky_key)
            .await
            .map_err(|e| HttpError::new(TransportErrorClass::Other, e.to_string()))?;
        // reqwest proxies are client-scoped, so build a client carrying the
        // selected proxy (or the shared one when no proxy is configured).
        let client = match &endpoint {
            Some(endpoint) => build_client(&self.config, Some(&endpoint.url))
                .map_err(|e| HttpError::new(TransportErrorClass::Other, e.to_string()))?,
            None => self.client.clone(),
        };
        let builder = client
            .post(&self.chat_url)
            .bearer_auth(&self.api_key)
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        let template = builder.try_clone().ok_or_else(|| {
            HttpError::new(TransportErrorClass::Other, "request body is not cloneable")
        })?;

        let last_status = std::sync::Arc::new(std::sync::Mutex::new(None::<u16>));
        let status_slot = last_status.clone();
        let ctx = TransportContext { provider, model, target: &self.chat_url, mode: "http" };
        let metrics = crate::metrics::init_metrics();
        let started = Instant::now();
        let result = retry_with_backoff(&self.retry_policy, ctx, |_| {
            let clone_opt = template.try_clone();
            let status_slot = status_slot.clone();
            async move {
                let clone = match clone_opt {
                    Some(clone) => clone,
                    None => {
                        return Err(TransportFailure::new(
                            TransportErrorClass::Other,
                            "request is not cloneable",
                        ));
                    }
                };
                let response = clone.send().await.map_err(|e| TransportFailure::from_reqwest(&e))?;
                let status = response.status();
                if status.is_success() {
                    Ok(response)
                } else {
                    if let Ok(mut slot) = status_slot.lock() {
                        *slot = Some(status.as_u16());
                    }
                    let mut failure = TransportFailure::new(
                        classify_status(status.as_u16()),
                        format!("upstream returned HTTP {}", status.as_u16()),
                    );
                    if let Some(delay) = retry_after_from_headers(response.headers()) {
                        failure = failure.with_retry_after(delay);
                    }
                    Err(failure)
                }
            }
        })
        .await;

        let elapsed = started.elapsed().as_millis() as u64;
        metrics.observe_http_upstream_latency(provider, model, elapsed);
        let result = match result {
            Ok(response) => {
                metrics.record_http_upstream_request(provider, model, &self.chat_url, "ok", "http");
                if let Some(endpoint) = &endpoint {
                    crate::gateway::proxy::report_result(
                        provider,
                        endpoint,
                        crate::gateway::proxy::ProxyOutcome::Success {
                            latency: Duration::from_millis(elapsed),
                        },
                    );
                }
                Ok(response)
            }
            Err(err) => {
                metrics.record_http_upstream_request(provider, model, &self.chat_url, "error", "http");
                metrics.record_http_upstream_error(provider, model, err.class.as_str());
                if let Some(endpoint) = &endpoint {
                    let outcome = if err.class.is_quota() {
                        crate::gateway::proxy::ProxyOutcome::Quota
                    } else if err.class == TransportErrorClass::Timeout {
                        crate::gateway::proxy::ProxyOutcome::Timeout
                    } else {
                        crate::gateway::proxy::ProxyOutcome::ServerError
                    };
                    crate::gateway::proxy::report_result(provider, endpoint, outcome);
                }
                let status = last_status.lock().ok().and_then(|slot| *slot).unwrap_or(0);
                Err(HttpError::from(err).with_status(status))
            }
        }?;
        Ok(result)
    }

    fn normalize_response(
        &self,
        value: Value,
        upstream_model: &str,
    ) -> Result<ChatCompletionResponse, HttpError> {
        let parsed: UpstreamResponse = serde_json::from_value(value).map_err(|e| {
            HttpError::new(TransportErrorClass::Other, format!("invalid chat response: {}", e))
        })?;
        let choices = parsed
            .choices
            .into_iter()
            .map(|choice| Choice {
                index: choice.index,
                message: ChatMessage {
                    role: MessageRole::from_string(&choice.message.role),
                    content: choice.message.content,
                    reasoning_content: choice.message.reasoning_content,
                    tool_calls: choice.message.tool_calls,
                    tool_call_id: choice.message.tool_call_id,
                    name: None,
                },
                finish_reason: choice.finish_reason,
            })
            .collect();
        Ok(ChatCompletionResponse {
            id: parsed.id,
            object: parsed.object,
            created: parsed.created,
            model: if parsed.model.is_empty() { upstream_model.to_string() } else { parsed.model },
            choices,
            usage: parsed.usage,
            session_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;

    fn config() -> ProviderConfig {
        let mut cfg = ProviderConfig::default();
        cfg.id = "nvidia".to_string();
        cfg.provider_type = "http_openai_compatible".to_string();
        cfg.base_url = Some("https://integrate.api.nvidia.com/v1/".to_string());
        cfg.chat_completions_path = "/chat/completions".to_string();
        cfg.models_path = "/models".to_string();
        cfg.api_key_env = None;
        cfg.api_key = Some("nvapi-test-key".to_string());
        cfg
    }

    #[test]
    fn join_path_handles_trailing_slash() {
        assert_eq!(
            join_path("https://integrate.api.nvidia.com/v1/", "/chat/completions"),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
    }

    #[test]
    fn nvidia_fields_are_not_forwarded_by_default() {
        let cfg = config();
        // ProviderConfig::default() does not apply serde defaults; set explicitly.
        let body = build_request_body(
            &ProviderConfig {
                forward_tools: true,
                forward_tool_choice: false,
                forward_response_format: false,
                ..cfg.clone()
            },
            "meta/llama-3.3-70b-instruct",
            &ChatCompletionRequest {
                model: "agg/nvidia/meta/llama-3.3-70b-instruct".to_string(),
                messages: vec![ChatMessage::text("user", "hello")],
                tool_choice: Some(serde_json::json!("auto")),
                response_format: Some(serde_json::json!({"type": "json_object"})),
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(body["model"], "meta/llama-3.3-70b-instruct");
        assert_eq!(body["stream"], true);
        assert!(body.get("tool_choice").is_none());
        assert!(body.get("response_format").is_none());
        assert!(body.get("agentgate_run_id").is_none());
    }

    #[test]
    fn key_prefix_validation_fails_closed() {
        let mut cfg = config();
        cfg.api_key = Some("sk-wrong".to_string());
        cfg.api_key_prefix = Some("nvapi-".to_string());
        assert!(resolve_api_key(&cfg).is_err());
    }
}
