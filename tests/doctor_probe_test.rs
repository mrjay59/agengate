//! RS7: synthetic reasoning probe endpoints.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::util::ServiceExt;

#[tokio::test]
async fn matrix_endpoint_returns_client_and_intermediary_data() {
    let app = agentgate::doctor::probe_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/matrix")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["clients"].as_array().unwrap().len() >= 3);
    assert_eq!(json["intermediaries"][0]["id"], "9router");
}

#[tokio::test]
async fn probe_stream_is_sse_and_incremental_for_every_format() {
    for (format, expected) in [
        ("reasoning_content", "reasoning_content"),
        ("reasoning", "\"reasoning\""),
        ("both", "reasoning_content"),
        ("think_tags", "<think>"),
        ("none", "probe answer"),
    ] {
        let app = agentgate::doctor::probe_router();
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/probe/reasoning/{format}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "format {format}");
        assert_eq!(
            response.headers()["content-type"],
            "text/event-stream",
            "format {format}"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains(expected), "format {format}: {body}");
        assert!(body.contains("[DONE]"), "format {format}");
        if format != "none" {
            assert!(
                body.contains("probe reasoning #1") && body.contains("probe reasoning #2"),
                "format {format} must contain both reasoning deltas"
            );
        }
    }
}

#[tokio::test]
async fn unknown_probe_format_is_rejected() {
    let app = agentgate::doctor::probe_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/probe/reasoning/bogus")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
