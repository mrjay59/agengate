//! Transport hardening helpers shared by HTTP upstreams and the CLI bridge.
//!
//! This module is intentionally dependency-light and side-effect-light: it
//! classifies transport failures, applies bounded exponential backoff with
//! jitter, redacts secrets from diagnostics, parses SSE frames, and provides a
//! generic endpoint-failover helper. Callers are responsible for recording
//! their own domain metrics (see `crate::metrics`).
//!
//! Covers the errors reported in the field:
//! `ECONNREFUSED`, `ECONNRESET`, `Broken pipe`, timeouts, and
//! `standalone server exited before reporting readiness`.

use std::{
    fmt,
    future::Future,
    sync::OnceLock,
    time::Duration,
};

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Coarse class of a transport failure. Used for metrics labels, retry
/// decisions, and failover policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportErrorClass {
    /// Connection refused / ECONNREFUSED / OS error 111.
    ConnectionRefused,
    /// Connection reset / ECONNRESET / OS error 104.
    ConnectionReset,
    /// Broken pipe / OS error 32 (upstream or child exited mid-write).
    BrokenPipe,
    /// DNS resolution failure.
    Dns,
    /// TLS handshake / certificate failure. Usually not retryable.
    Tls,
    /// Timeout (request, connect, or stream idle).
    Timeout,
    /// HTTP 429 / provider.free-limit / free usage limit.
    /// Handled by quota-aware failover rather than plain transport retry.
    Quota429,
    /// HTTP 5xx from upstream.
    Server5xx,
    /// "standalone server exited before reporting readiness" and friends.
    Readiness,
    /// Generic transient I/O (EAGAIN, resource temporarily unavailable, ...).
    Io,
    /// The client/stream was aborted; retrying is pointless.
    Aborted,
    /// Upstream emitted an invalid stream event (e.g. non-string delta.content).
    /// Transient: a retry often yields a valid frame when no output was sent yet.
    InvalidOutput,
    /// Unclassified.
    Other,
}

impl TransportErrorClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransportErrorClass::ConnectionRefused => "connection_refused",
            TransportErrorClass::ConnectionReset => "connection_reset",
            TransportErrorClass::BrokenPipe => "broken_pipe",
            TransportErrorClass::Dns => "dns",
            TransportErrorClass::Tls => "tls",
            TransportErrorClass::Timeout => "timeout",
            TransportErrorClass::Quota429 => "quota_429",
            TransportErrorClass::Server5xx => "server_5xx",
            TransportErrorClass::Readiness => "readiness",
            TransportErrorClass::Io => "io",
            TransportErrorClass::Aborted => "aborted",
            TransportErrorClass::InvalidOutput => "invalid_output",
            TransportErrorClass::Other => "other",
        }
    }

    /// Whether a plain retry (same endpoint, same request) can reasonably
    /// succeed. Quota and TLS/auth failures are excluded by design; those are
    /// routed through the quota/failover layer instead.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            TransportErrorClass::ConnectionRefused
                | TransportErrorClass::ConnectionReset
                | TransportErrorClass::BrokenPipe
                | TransportErrorClass::Dns
                | TransportErrorClass::Timeout
                | TransportErrorClass::Server5xx
                | TransportErrorClass::Readiness
                | TransportErrorClass::Io
                | TransportErrorClass::InvalidOutput
        )
    }

    pub fn is_quota(&self) -> bool {
        matches!(self, TransportErrorClass::Quota429)
    }
}

impl fmt::Display for TransportErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classify an error string (anyhow/reqwest/CLI stderr) into a transport class.
///
/// Order matters: quota and readiness strings are checked before generic
/// status/IO patterns so the most specific cause wins.
pub fn classify_message(message: &str) -> TransportErrorClass {
    let s = message.to_lowercase();

    if s.contains("provider.free-limit")
        || s.contains("free usage limit")
        || s.contains("http 429")
        || s.contains("status 429")
        || s.contains("\"status\":429")
        || s.contains("quota_exceeded")
        || s.contains("rate limit reached")
    {
        return TransportErrorClass::Quota429;
    }

    if s.contains("standalone server exited before reporting readiness")
        || s.contains("before reporting readiness")
        || (s.contains("server") && s.contains("not ready"))
    {
        return TransportErrorClass::Readiness;
    }

    if s.contains("connection refused") || s.contains("econnrefused") || s.contains("os error 111") {
        return TransportErrorClass::ConnectionRefused;
    }
    if s.contains("connection reset") || s.contains("econnreset") || s.contains("os error 104") {
        return TransportErrorClass::ConnectionReset;
    }
    if s.contains("broken pipe")
        || s.contains("os error 32")
        // Windows: ERROR_BROKEN_PIPE (109) / ERROR_NO_DATA (232)
        || s.contains("os error 109")
        || s.contains("os error 232")
        || s.contains("pipe has been ended")
        || s.contains("pipe is being closed")
    {
        return TransportErrorClass::BrokenPipe;
    }
    // Upstream stream-frame shape error (e.g. 9router sending non-string
    // `choices[0].delta.content`) surfacing through the opencode AI SDK.
    // Transient: one retry when no meaningful output has been emitted yet.
    if s.contains("provider.invalid-output")
        || (s.contains("invalid") && s.contains("stream event"))
        || (s.contains("delta.content") && s.contains("expected string"))
    {
        return TransportErrorClass::InvalidOutput;
    }
    if s.contains("timed out") || s.contains("timeout") || s.contains("elapsed") {
        return TransportErrorClass::Timeout;
    }
    if s.contains("certificate") || s.contains("handshake") || s.contains("invalid tls") {
        return TransportErrorClass::Tls;
    }
    if s.contains("dns") || s.contains("name or service not known") || s.contains("lookup") {
        return TransportErrorClass::Dns;
    }
    if s.contains("client hung up") || s.contains("aborted") || s.contains("client closed") {
        return TransportErrorClass::Aborted;
    }
    if s.contains("resource temporarily unavailable")
        || s.contains("os error 11")
        || s.contains("would block")
        || s.contains("unexpected eof")
    {
        return TransportErrorClass::Io;
    }
    if s.contains("http 500")
        || s.contains("http 502")
        || s.contains("http 503")
        || s.contains("http 504")
        || s.contains("status 500")
        || s.contains("status 502")
        || s.contains("status 503")
        || s.contains("status 504")
        || s.contains(" 5xx")
    {
        return TransportErrorClass::Server5xx;
    }

    TransportErrorClass::Other
}

/// Classify an HTTP status code returned by an upstream.
pub fn classify_status(status: u16) -> TransportErrorClass {
    match status {
        429 => TransportErrorClass::Quota429,
        408 | 425 => TransportErrorClass::Timeout,
        500..=599 => TransportErrorClass::Server5xx,
        _ => TransportErrorClass::Other,
    }
}

/// Classify a `reqwest::Error`, walking its source chain for nested IO causes.
pub fn classify_reqwest(error: &reqwest::Error) -> TransportErrorClass {
    if let Some(status) = error.status() {
        return classify_status(status.as_u16());
    }
    if error.is_timeout() {
        return TransportErrorClass::Timeout;
    }
    if error.is_connect() {
        let s = error.to_string().to_lowercase();
        if s.contains("refused") {
            return TransportErrorClass::ConnectionRefused;
        }
        if s.contains("reset") {
            return TransportErrorClass::ConnectionReset;
        }
        if s.contains("name or service") || s.contains("lookup") || s.contains("dns") {
            return TransportErrorClass::Dns;
        }
        if s.contains("certificate") || s.contains("tls") || s.contains("handshake") {
            return TransportErrorClass::Tls;
        }
    }

    let direct = classify_message(&error.to_string());
    if direct != TransportErrorClass::Other {
        return direct;
    }
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        let c = classify_message(&cause.to_string());
        if c != TransportErrorClass::Other {
            return c;
        }
        source = cause.source();
    }
    if error.is_request() {
        TransportErrorClass::Io
    } else {
        TransportErrorClass::Other
    }
}

/// True when the message is the specific "server exited before readiness"
/// family reported on node-ai.
pub fn is_readiness_error(message: &str) -> bool {
    classify_message(message) == TransportErrorClass::Readiness
}

// ---------------------------------------------------------------------------
// Structured failure + retry policy
// ---------------------------------------------------------------------------

/// A classifiable failure carrying an optional `Retry-After` hint.
#[derive(Debug, Clone)]
pub struct TransportFailure {
    pub class: TransportErrorClass,
    pub message: String,
    pub retry_after: Option<Duration>,
    /// Upstream HTTP status when the failure came from an HTTP response.
    pub status: Option<u16>,
}

/// Lets `from_reqwest` accept both `reqwest::Error` and `&reqwest::Error`, so
/// callers can use it directly as a `map_err` function in either style.
pub trait AsReqwestError {
    fn as_reqwest_error(&self) -> &reqwest::Error;
}

impl AsReqwestError for reqwest::Error {
    fn as_reqwest_error(&self) -> &reqwest::Error {
        self
    }
}

impl AsReqwestError for &reqwest::Error {
    fn as_reqwest_error(&self) -> &reqwest::Error {
        self
    }
}

impl TransportFailure {
    pub fn new(class: TransportErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            retry_after: None,
            status: None,
        }
    }

    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Upstream status code, when known (`None` for connection-level errors).
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    pub fn from_reqwest<E: AsReqwestError>(error: E) -> Self {
        let error = error.as_reqwest_error();
        let mut failure = Self::new(classify_reqwest(error), redact_diagnostics(&error.to_string()));
        failure.status = error.status().map(|status| status.as_u16());
        failure
    }

    pub fn from_message(message: &str) -> Self {
        Self::new(classify_message(message), redact_diagnostics(message))
    }
}

impl fmt::Display for TransportFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class.as_str(), self.message)
    }
}

impl std::error::Error for TransportFailure {}

/// Bounded exponential-backoff policy with optional jitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Number of retries *after* the initial attempt.
    pub max_retries: usize,
    pub base_delay: Duration,
    pub max_delay: Duration,
    /// Apply +/- 25% jitter to computed delays (production only).
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(4),
            jitter: true,
        }
    }
}

impl RetryPolicy {
    /// Deterministic exponential delay for a 0-based retry attempt.
    pub fn delay_for_attempt(&self, attempt: usize) -> Duration {
        let shift = attempt.min(20) as u32;
        let base_ms = self.base_delay.as_millis().saturating_mul(1u128 << shift);
        let cap_ms = self.max_delay.as_millis() as u128;
        Duration::from_millis(base_ms.min(cap_ms) as u64)
    }

    /// Delay with bounded jitter; never longer than `max_delay`.
    pub fn jittered_delay_for_attempt(&self, attempt: usize) -> Duration {
        let delay = self.delay_for_attempt(attempt);
        if !self.jitter || delay.is_zero() {
            return delay;
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u128)
            .unwrap_or(0);
        // 75%..100% of the nominal delay keeps retries snappy without a stampede.
        let scaled = delay.as_millis() * (75 + (nanos % 26)) / 100;
        Duration::from_millis(scaled.min(self.max_delay.as_millis() as u128) as u64)
    }
}

/// Context fields attached to retry/failover logs (all optional/empty by default).
#[derive(Debug, Default, Clone, Copy)]
pub struct TransportContext<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub target: &'a str,
    pub mode: &'a str,
}

/// Run an operation with bounded retry for retryable transport classes.
///
/// The operation receives a 0-based attempt number and must return a
/// `TransportFailure` so the classifier can decide whether to retry. A
/// `Retry-After` hint on the failure takes precedence over computed backoff
/// (capped at 8x `max_delay` so a malicious header cannot pin the request).
pub async fn retry_with_backoff<F, Fut, T>(
    policy: &RetryPolicy,
    ctx: TransportContext<'_>,
    mut operation: F,
) -> Result<T, TransportFailure>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<T, TransportFailure>>,
{
    let mut last_error: Option<TransportFailure> = None;

    for attempt in 0..=policy.max_retries {
        match operation(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= policy.max_retries || !err.class.is_retryable() {
                    return Err(err);
                }
                let header_delay = err.retry_after.unwrap_or_default();
                let backoff = policy.jittered_delay_for_attempt(attempt);
                let delay = if header_delay > backoff {
                    header_delay.min(policy.max_delay.saturating_mul(8))
                } else {
                    backoff
                };
                tracing::warn!(
                    target: "agentgate.transport",
                    provider = ctx.provider,
                    model = ctx.model,
                    target_endpoint = ctx.target,
                    mode = ctx.mode,
                    class = err.class.as_str(),
                    attempt,
                    delay_ms = delay.as_millis(),
                    error = %redact_diagnostics(&err.message),
                    "transport.retry"
                );
                tokio::time::sleep(delay).await;
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        TransportFailure::new(TransportErrorClass::Other, "retry loop exhausted")
    }))
}

/// Try a list of endpoints in order, retrying each per `policy`. Used by the
/// HTTP upstream (proxy pool) and free-model load balancer. Returns the index
/// of the endpoint that succeeded together with its value.
pub async fn failover<F, Fut, T>(
    policy: &RetryPolicy,
    ctx: TransportContext<'_>,
    endpoints: &[&str],
    mut operation: F,
) -> Result<(usize, T), TransportFailure>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, TransportFailure>>,
{
    let mut last_error: Option<TransportFailure> = None;

    for (index, endpoint) in endpoints.iter().enumerate() {
        let endpoint_ctx = TransportContext {
            target: endpoint,
            ..ctx
        };
        match retry_with_backoff(policy, endpoint_ctx, |_| operation(endpoint)).await {
            Ok(value) => {
                if index > 0 {
                    tracing::warn!(
                        target: "agentgate.transport",
                        provider = ctx.provider,
                        model = ctx.model,
                        from_endpoint = endpoints[0],
                        to_endpoint = endpoint,
                        "transport.failover_succeeded"
                    );
                }
                return Ok((index, value));
            }
            Err(err) => {
                tracing::warn!(
                    target: "agentgate.transport",
                    provider = ctx.provider,
                    model = ctx.model,
                    target_endpoint = endpoint,
                    class = err.class.as_str(),
                    "transport.endpoint_exhausted"
                );
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        TransportFailure::new(TransportErrorClass::Other, "no endpoints configured")
    }))
}

/// Emit a structured diagnostic log for a terminal transport failure.
pub fn log_transport_failure(ctx: TransportContext<'_>, err: &TransportFailure, attempts: usize) {
    tracing::error!(
        target: "agentgate.transport",
        provider = ctx.provider,
        model = ctx.model,
        target_endpoint = ctx.target,
        mode = ctx.mode,
        class = err.class.as_str(),
        attempts,
        error = %redact_diagnostics(&err.message),
        "transport.failure"
    );
}

// ---------------------------------------------------------------------------
// Retry-After parsing
// ---------------------------------------------------------------------------

/// Parse an HTTP `Retry-After` header (delta-seconds or RFC1123/HTTP-date).
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(value) {
        let now = chrono::Utc::now();
        let delta = date.with_timezone(&chrono::Utc).signed_duration_since(now);
        if delta > chrono::Duration::zero() {
            return Some(Duration::from_millis(delta.num_milliseconds() as u64));
        }
    }
    None
}

/// Read `Retry-After` from response headers.
pub fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after)
}

// ---------------------------------------------------------------------------
// Secret redaction for logs/metrics/admin output
// ---------------------------------------------------------------------------

/// Redact URL userinfo (`http://user:pass@host`) and sensitive query values.
pub fn redact_url(url: &str) -> String {
    static USERINFO: OnceLock<regex::Regex> = OnceLock::new();
    static QUERY_SECRET: OnceLock<regex::Regex> = OnceLock::new();

    let userinfo = USERINFO.get_or_init(|| {
        regex::Regex::new(r"(?i)(https?://)[^/@\s]+@").expect("valid userinfo regex")
    });
    let query_secret = QUERY_SECRET.get_or_init(|| {
        regex::Regex::new(r"(?i)([?&](?:api[_-]?key|key|token|access_token|secret|password)=)[^&\s]+")
            .expect("valid query secret regex")
    });

    let redacted = userinfo.replace_all(url, "$1***@");
    query_secret.replace_all(&redacted, "$1***").into_owned()
}

/// Redact credentials, API keys and secret-bearing key/value pairs from a
/// diagnostic string before it reaches logs or metrics.
pub fn redact_diagnostics(input: &str) -> String {
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static AGK: OnceLock<regex::Regex> = OnceLock::new();
    static NVAPI: OnceLock<regex::Regex> = OnceLock::new();
    static KV_SECRET: OnceLock<regex::Regex> = OnceLock::new();

    let bearer =
        BEARER.get_or_init(|| regex::Regex::new(r"(?i)(bearer\s+)[a-z0-9._\-]+").expect("valid bearer regex"));
    let agk = AGK.get_or_init(|| regex::Regex::new(r"agk_[A-Za-z0-9_\-]+").expect("valid agk regex"));
    let nvapi =
        NVAPI.get_or_init(|| regex::Regex::new(r"nvapi-[A-Za-z0-9_\-]+").expect("valid nvapi regex"));
    let kv_secret = KV_SECRET.get_or_init(|| {
        regex::Regex::new(r#"(?i)("?(?:api[_-]?key|access_token|authorization|refresh_token|token|secret|password|key)"?\s*[:=]\s*"?)([^\s",;}]+)"#)
            .expect("valid kv secret regex")
    });

    let mut out = redact_url(input);
    out = bearer.replace_all(&out, "$1***").into_owned();
    out = agk.replace_all(&out, "agk_***").into_owned();
    out = nvapi.replace_all(&out, "nvapi-***").into_owned();
    kv_secret.replace_all(&out, "$1***").into_owned()
}

// ---------------------------------------------------------------------------
// SSE parsing (OpenAI-compatible `text/event-stream`)
// ---------------------------------------------------------------------------

/// One decoded Server-Sent Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub name: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Partial event fields carried across `push()` calls. Without this, an event
/// split across two network chunks loses the fields seen in the first chunk.
#[derive(Debug, Default)]
struct PendingSseEvent {
    name: Option<String>,
    data: Option<String>,
    id: Option<String>,
}

/// Incremental SSE decoder. Handles chunk boundaries, CRLF, comments,
/// multi-line `data:` fields and the terminal `[DONE]` sentinel.
#[derive(Debug, Default)]
pub struct SseParser {
    buffer: String,
    pending: PendingSseEvent,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw chunk and return all complete events decoded so far.
    pub fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buffer.push_str(chunk);
        self.drain(false)
    }

    /// Flush any trailing event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        self.drain(true)
    }

    fn take_pending(&mut self) -> Option<SseEvent> {
        let payload = self.pending.data.take()?;
        let event = SseEvent {
            name: self.pending.name.take(),
            data: payload,
            id: self.pending.id.take(),
        };
        Some(event)
    }

    fn drain(&mut self, final_flush: bool) -> Vec<SseEvent> {
        let mut events = Vec::new();

        loop {
            let line = match self.buffer.find('\n') {
                Some(index) => {
                    let line = self.buffer[..index].trim_end_matches('\r').to_string();
                    self.buffer.drain(..=index);
                    line
                }
                None => {
                    if !final_flush || self.buffer.is_empty() {
                        break;
                    }
                    let line = self.buffer.trim_end_matches('\r').to_string();
                    self.buffer.clear();
                    line
                }
            };

            if line.is_empty() {
                if let Some(event) = self.take_pending() {
                    events.push(event);
                } else {
                    self.pending.name = None;
                    self.pending.id = None;
                }
                continue;
            }

            if let Some(rest) = line.strip_prefix(':') {
                // Comment / keep-alive line — ignore.
                let _ = rest;
                continue;
            }

            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_str(), ""),
            };

            match field {
                "data" => {
                    let slot = self.pending.data.get_or_insert_with(String::new);
                    if !slot.is_empty() {
                        slot.push('\n');
                    }
                    slot.push_str(value);
                }
                "event" => self.pending.name = Some(value.to_string()),
                "id" => self.pending.id = Some(value.to_string()),
                // "retry" and unknown fields are intentionally ignored.
                _ => {}
            }
        }

        if final_flush {
            if let Some(event) = self.take_pending() {
                events.push(event);
            }
        }
        events
    }
}

/// True when an SSE event is the OpenAI-compatible stream terminator.
pub fn is_sse_done(event: &SseEvent) -> bool {
    event.data.trim() == "[DONE]"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_reported_transport_errors() {
        let cases = [
            ("Connection refused (os error 111)", TransportErrorClass::ConnectionRefused),
            ("tcp connect ECONNREFUSED 127.0.0.1:11434", TransportErrorClass::ConnectionRefused),
            ("Connection reset by peer (os error 104)", TransportErrorClass::ConnectionReset),
            ("Broken pipe (os error 32)", TransportErrorClass::BrokenPipe),
            (
                "provider_error: Invalid 9router/openai-compatible-chat stream event",
                TransportErrorClass::InvalidOutput,
            ),
            (
                "Expected string | null | undefined at choices[0].delta.content",
                TransportErrorClass::InvalidOutput,
            ),
            ("provider.invalid-output", TransportErrorClass::InvalidOutput),
            ("standalone server exited before reporting readiness", TransportErrorClass::Readiness),
            ("request timed out after 30s", TransportErrorClass::Timeout),
            ("error sending request: provider.free-limit reached", TransportErrorClass::Quota429),
            ("HTTP 503 from upstream", TransportErrorClass::Server5xx),
            ("client hung up", TransportErrorClass::Aborted),
            ("certificate verify failed", TransportErrorClass::Tls),
            ("dns lookup failed", TransportErrorClass::Dns),
            ("resource temporarily unavailable (os error 11)", TransportErrorClass::Io),
            ("something unexpected", TransportErrorClass::Other),
        ];
        for (message, expected) in cases {
            assert_eq!(classify_message(message), expected, "message: {message}");
        }
    }

    #[test]
    fn classifies_http_statuses() {
        assert_eq!(classify_status(429), TransportErrorClass::Quota429);
        assert_eq!(classify_status(408), TransportErrorClass::Timeout);
        assert_eq!(classify_status(503), TransportErrorClass::Server5xx);
        assert_eq!(classify_status(200), TransportErrorClass::Other);
    }

    #[test]
    fn retryable_classes_exclude_quota_tls_and_abort() {
        assert!(TransportErrorClass::ConnectionRefused.is_retryable());
        assert!(TransportErrorClass::BrokenPipe.is_retryable());
        assert!(TransportErrorClass::InvalidOutput.is_retryable());
        assert!(TransportErrorClass::Readiness.is_retryable());
        assert!(TransportErrorClass::Server5xx.is_retryable());
        assert!(!TransportErrorClass::Quota429.is_retryable());
        assert!(!TransportErrorClass::Tls.is_retryable());
        assert!(!TransportErrorClass::Aborted.is_retryable());
        assert!(TransportErrorClass::Quota429.is_quota());
    }

    #[test]
    fn readiness_helper_detects_only_readiness_family() {
        assert!(is_readiness_error("standalone server exited before reporting readiness"));
        assert!(!is_readiness_error("connection refused"));
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(400),
            jitter: false,
        };
        assert_eq!(policy.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(400));
    }

    #[test]
    fn jittered_delay_never_exceeds_cap() {
        let policy = RetryPolicy::default();
        for attempt in 0..8 {
            assert!(policy.jittered_delay_for_attempt(attempt) <= policy.max_delay);
        }
    }

    #[test]
    fn redaction_removes_proxy_credentials_and_api_keys() {
        let input = "proxy http://admin:s3cret@proxy.internal:8080 failed; Authorization: Bearer abc123XYZ; key=supersecretvalue; use agk_live_abcdef123456 and nvapi-abc123";
        let redacted = redact_diagnostics(input);
        assert!(!redacted.contains("s3cret"), "{redacted}");
        assert!(!redacted.contains("abc123XYZ"), "{redacted}");
        assert!(!redacted.contains("supersecretvalue"), "{redacted}");
        assert!(!redacted.contains("agk_live_abcdef123456"), "{redacted}");
        assert!(!redacted.contains("nvapi-abc123"), "{redacted}");
        assert!(redacted.contains("***"), "{redacted}");
    }

    #[test]
    fn redact_url_keeps_host_but_hides_userinfo() {
        let redacted = redact_url("https://user:pass@example.com/v1?api_key=secret123&model=x");
        assert!(redacted.contains("example.com"), "{redacted}");
        assert!(!redacted.contains("pass"), "{redacted}");
        assert!(!redacted.contains("secret123"), "{redacted}");
    }

    #[test]
    fn parses_retry_after_seconds_and_http_date() {
        assert_eq!(parse_retry_after("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after(" 5 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("nonsense"), None);

        let future = chrono::Utc::now() + chrono::Duration::seconds(30);
        let http_date = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let parsed = parse_retry_after(&http_date).expect("valid HTTP-date");
        assert!(parsed.as_secs() >= 28 && parsed.as_secs() <= 30, "parsed: {:?}", parsed);
    }

    #[test]
    fn sse_parser_handles_split_frames_and_done() {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        events.extend(parser.push("data: {\"a\":1}\r\n\r\nevent: ping\ndata: hello\n\n"));
        events.extend(parser.push(": keepalive\ndata: [DO"));
        events.extend(parser.push("NE]\n\n"));
        assert_eq!(events.len(), 3, "{events:?}");
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].name.as_deref(), Some("ping"));
        assert_eq!(events[1].data, "hello");
        assert_eq!(events[2].data, "[DONE]");
        assert!(is_sse_done(&events[2]));
    }

    #[test]
    fn sse_parser_flushes_trailing_event_without_blank_line() {
        let mut parser = SseParser::new();
        assert!(parser.push("data: line1\ndata: line2").is_empty());
        let events = parser.finish();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            jitter: false,
        };
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let value = retry_with_backoff(&policy, TransportContext::default(), |attempt| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    Err(TransportFailure::from_message("connection refused"))
                } else {
                    Ok(42)
                }
            }
        })
        .await
        .expect("eventually succeeds");
        assert_eq!(value, 42);
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_does_not_retry_quota_or_non_retryable_errors() {
        let policy = RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            jitter: false,
        };
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let result = retry_with_backoff(&policy, TransportContext::default(), |_| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Err::<(), _>(TransportFailure::from_message("free usage limit reached")) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_honors_retry_after_hint() {
        let policy = RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_millis(1),
            // Retry-After is capped at 8x max_delay; keep the cap above the
            // 30 ms hint so the header is actually honoured in this test.
            max_delay: Duration::from_millis(100),
            jitter: false,
        };
        let started = std::time::Instant::now();
        let result = retry_with_backoff(&policy, TransportContext::default(), |attempt| async move {
            if attempt == 0 {
                // Use a retryable transport class; a 429/quota failure is
                // deliberately excluded from plain transport retries.
                Err(TransportFailure::from_message("connection refused").with_retry_after(Duration::from_millis(30)))
            } else {
                Ok(())
            }
        })
        .await;
        assert!(result.is_ok());
        assert!(started.elapsed() >= Duration::from_millis(25), "elapsed: {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn failover_moves_to_healthy_endpoint() {
        let policy = RetryPolicy {
            max_retries: 0,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
            jitter: false,
        };
        let endpoints = ["http://127.0.0.1:1", "http://127.0.0.1:2"];
        let (index, value) = failover(&policy, TransportContext::default(), &endpoints, |endpoint| {
            // Compute the predicate before the async block: `async move` cannot
            // capture the `endpoints` array out of an `FnMut` closure.
            let is_first = endpoint == endpoints[0];
            async move {
                if is_first {
                    Err::<String, _>(TransportFailure::from_message("connection refused"))
                } else {
                    Ok("healthy".to_string())
                }
            }
        })
        .await
        .expect("second endpoint succeeds");
        assert_eq!(index, 1);
        assert_eq!(value, "healthy");
    }
}
