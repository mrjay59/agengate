//! Phase 5 hardening (spec §14/§17): transport classification, bounded
//! retry/backoff, endpoint/proxy failover, SSE parsing, and metrics exposure.
//!
//! These tests use `wiremock` mock upstreams so no real provider or network
//! egress is required.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use agentgate::transport::{
    classify_status, failover, retry_after_from_headers, retry_with_backoff,
    is_sse_done, parse_retry_after, redact_diagnostics,
    SseParser, TransportContext, TransportErrorClass, TransportFailure, RetryPolicy,
};
use axum::{body::Body, http::{Method, Request}, response::Response};
use reqwest::Client;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn fast_policy(max_retries: usize) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        jitter: false,
    }
}

fn classify_response(response: &reqwest::Response) -> TransportFailure {
    let status = response.status().as_u16();
    let mut failure = TransportFailure::new(classify_status(status), format!("HTTP {status}"));
    if let Some(retry_after) = retry_after_from_headers(response.headers()) {
        failure = failure.with_retry_after(retry_after);
    }
    failure
}

#[tokio::test]
async fn transient_503_then_success_against_mock_upstream() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/chat"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("Retry-After", "0")
                .set_body_string("upstream temporarily unavailable"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok-after-retry"))
        .mount(&server)
        .await;

    let client = Client::new();
    let url = format!("{}/chat", server.uri());
    let attempts = std::sync::Arc::new(AtomicUsize::new(0));

    let body = retry_with_backoff(
        &fast_policy(2),
        TransportContext {
            provider: "mock",
            model: "mock-chat",
            target: &url,
            mode: "http",
        },
        |_| {
            let client = client.clone();
            let url = url.clone();
            let attempts = attempts.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                let response = client.get(&url).send().await.map_err(|error| TransportFailure::from_reqwest(&error))?;
                if response.status().is_success() {
                    Ok(response.text().await.unwrap_or_default())
                } else {
                    Err(classify_response(&response))
                }
            }
        },
    )
    .await
    .expect("503 should be retried and then succeed");

    assert_eq!(body, "ok-after-retry");
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "expected exactly one retry");
}

#[tokio::test]
async fn non_retryable_429_without_retry_policy_fails_fast() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/chat"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_string("free usage limit reached"),
        )
        .mount(&server)
        .await;

    let client = Client::new();
    let url = format!("{}/chat", server.uri());
    let attempts = std::sync::Arc::new(AtomicUsize::new(0));

    let result = retry_with_backoff(
        &fast_policy(3),
        TransportContext::default(),
        |_| {
            let client = client.clone();
            let url = url.clone();
            let attempts = attempts.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                let response = client.get(&url).send().await.map_err(|error| TransportFailure::from_reqwest(&error))?;
                if response.status().is_success() {
                    Ok(())
                } else {
                    Err(classify_response(&response))
                }
            }
        },
    )
    .await;

    let error = result.expect_err("quota must not be retried by the transport layer");
    assert_eq!(error.class, TransportErrorClass::Quota429);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn quota_429_fails_over_to_fallback_upstream() {
    let primary = MockServer::start().await;
    let fallback = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/chat"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "86400")
                .set_body_string("free usage limit reached"),
        )
        .mount(&primary)
        .await;
    Mock::given(method("GET"))
        .and(path("/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string("fallback-answer"))
        .mount(&fallback)
        .await;

    let primary_url = format!("{}/chat", primary.uri());
    let fallback_url = format!("{}/chat", fallback.uri());
    let client = Client::new();

    let (index, body) = failover(
        &fast_policy(0),
        TransportContext {
            provider: "mock-http",
            model: "quota-failover",
            target: "",
            mode: "http",
        },
        &[primary_url.as_str(), fallback_url.as_str()],
        |endpoint| {
            let client = client.clone();
            let endpoint = endpoint.to_string();
            async move {
                let response = client
                    .get(&endpoint)
                    .send()
                    .await
                    .map_err(|error| TransportFailure::from_reqwest(&error))?;
                if response.status().is_success() {
                    Ok(response.text().await.unwrap_or_default())
                } else {
                    Err(classify_response(&response))
                }
            }
        },
    )
    .await
    .expect("quota-disabled primary should fail over to the fallback");

    assert_eq!(index, 1, "fallback endpoint should be selected");
    assert_eq!(body, "fallback-answer");
}

#[tokio::test]
async fn sse_stream_from_mock_upstream_decodes_to_done() {
    let server = MockServer::start().await;
    let sse_body = "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\"}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n: keepalive\ndata: [DONE]\n\n";

    Mock::given(method("GET"))
        .and(path("/stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let response = Client::new()
        .get(format!("{}/stream", server.uri()))
        .send()
        .await
        .expect("mock upstream responds");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let body = response.text().await.expect("stream body");
    let mut parser = SseParser::new();
    let mut events = parser.push(&body);
    events.extend(parser.finish());

    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0].data, "{\"id\":\"1\",\"object\":\"chat.completion.chunk\"}");
    assert_eq!(events[1].data, "{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}");
    assert!(is_sse_done(&events[2]));
}

#[tokio::test]
async fn connection_refused_fails_over_to_healthy_mock_upstream() {
    // Bind then drop to obtain a port that is almost certainly closed.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let dead_port = listener.local_addr().expect("local addr").port();
    drop(listener);

    let healthy = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_string("fallback-ok"))
        .mount(&healthy)
        .await;

    let dead = format!("http://127.0.0.1:{dead_port}");
    let live = format!("{}/chat", healthy.uri());
    let endpoints = [dead.as_str(), live.as_str()];
    let client = Client::new();

    let (index, body) = failover(
        &fast_policy(0),
        TransportContext {
            provider: "mock-http",
            model: "failover-test",
            target: "",
            mode: "http",
        },
        &endpoints,
        |endpoint| {
            let client = client.clone();
            // Own the endpoint before entering `async move`: a borrowed &str
            // from the FnMut argument cannot escape into the future.
            let endpoint = endpoint.to_string();
            async move {
                let response = client
                    .get(&endpoint)
                    .timeout(Duration::from_millis(500))
                    .send()
                    .await
                    .map_err(|error| TransportFailure::from_reqwest(&error))?;
                if response.status().is_success() {
                    Ok(response.text().await.unwrap_or_default())
                } else {
                    Err(classify_response(&response))
                }
            }
        },
    )
    .await
    .expect("second endpoint should be healthy");

    assert_eq!(index, 1);
    assert_eq!(body, "fallback-ok");
}

#[tokio::test]
async fn proxy_pool_failover_skips_503_endpoint() {
    let primary = MockServer::start().await;
    let secondary = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream unavailable"))
        .mount(&primary)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"data\":[]}"))
        .mount(&secondary)
        .await;

    let proxy_a = format!("{}/v1/models", primary.uri());
    let proxy_b = format!("{}/v1/models", secondary.uri());
    let client = Client::new();
    let metrics = agentgate::metrics::init_metrics();

    let (index, _) = failover(
        &fast_policy(0),
        TransportContext {
            provider: "mock-http",
            model: "proxy-pool",
            target: "",
            mode: "http",
        },
        &[proxy_a.as_str(), proxy_b.as_str()],
        |endpoint| {
            let client = client.clone();
            let endpoint = endpoint.to_string();
            async move {
                let response = client.get(&endpoint).send().await.map_err(|error| TransportFailure::from_reqwest(&error))?;
                if response.status().is_success() {
                    metrics.record_proxy_selected("mock-http", &endpoint, "round_robin");
                    Ok(response.text().await.unwrap_or_default())
                } else {
                    let failure = classify_response(&response);
                    metrics.record_proxy_error("mock-http", &endpoint, failure.class.as_str());
                    Err(failure)
                }
            }
        },
    )
    .await
    .expect("second proxy should serve the request");

    assert_eq!(index, 1);
}

#[tokio::test]
async fn metrics_endpoint_exposes_http_quota_lb_and_proxy_series() {
    let metrics = agentgate::metrics::init_metrics();
    metrics.record_http_upstream_request("mock", "m", "http://mock/v1", "ok", "http");
    metrics.record_http_upstream_error("mock", "m", "connection_refused");
    metrics.observe_http_upstream_latency("mock", "m", 42);
    metrics.record_quota_tokens("in", "mock", "m", "key_1", 10);
    metrics.record_quota_bytes("out", "mock", "m", "key_1", 20);
    metrics.record_quota_denied("rpm", "limit_exceeded");
    metrics.record_key_usage("key_1");
    metrics.record_proxy_selected("mock", "http://proxy-a", "round_robin");
    metrics.record_proxy_error("mock", "http://proxy-a", "timeout");
    metrics.record_lb_selection("free", "from", "to", "quota");
    metrics.record_lb_fallback("free", "fallback", "pool_exhausted");

    let app = agentgate::api::api().with_state(std::sync::Arc::new(
        agentgate::service::ServiceRegistry::new(),
    ));
    let request = Request::builder()
        .uri("/_metrics")
        .method(Method::GET)
        .body(Body::empty())
        .unwrap();
    let response: Response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();
    for expected in [
        "agentgate_http_upstream_requests_total",
        "agentgate_http_upstream_errors_total",
        "agentgate_http_upstream_latency_ms_sum",
        "agentgate_http_upstream_latency_ms{provider=\"mock\",model=\"m\"}",
        "agentgate_quota_tokens_total",
        "agentgate_quota_bytes_total",
        "agentgate_quota_denied_total",
        "agentgate_key_usage_total",
        "agentgate_proxy_selected_total",
        "agentgate_proxy_errors_total",
        "agentgate_lb_selection_total",
        "agentgate_lb_fallback_total",
    ] {
        assert!(text.contains(expected), "missing metric {expected} in:\n{text}");
    }
}

#[test]
fn diagnostics_are_redacted_and_retry_after_parsed() {
    let redacted = redact_diagnostics(
        "connect http://user:pw@proxy:8080 failed with Bearer secret123 and agk_live_key456",
    );
    assert!(!redacted.contains("pw@"), "{redacted}");
    assert!(!redacted.contains("secret123"), "{redacted}");
    assert!(!redacted.contains("agk_live_key456"), "{redacted}");

    assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
    assert_eq!(parse_retry_after("not-a-date"), None);
}
