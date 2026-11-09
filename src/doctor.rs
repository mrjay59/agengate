//! `agentgate doctor` — client/9router compatibility matrix and synthetic
//! reasoning probes (RS7 / spec_reasoning §11.3).
//!
//! `doctor --probe-reasoning` serves three timestamped SSE streams without
//! calling any model. An operator points AionUi (directly or through 9router)
//! at each stream and records which format actually renders incrementally.

use std::time::Duration;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        Html, IntoResponse,
    },
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::reasoning::{
    format::SseWriter,
    ReasoningFormat, ReasoningKind,
};

/// Embedded client/9router matrix (`compat/clients.json`).
pub const CLIENT_MATRIX_JSON: &str = include_str!("../compat/clients.json");

pub fn client_matrix() -> Value {
    serde_json::from_str(CLIENT_MATRIX_JSON)
        .unwrap_or_else(|_| json!({ "error": "invalid embedded compat/clients.json" }))
}

/// One synthetic SSE frame with a relative delay.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeFrame {
    pub delay: Duration,
    pub data: String,
}

fn chunk(model: &str, delta: Value, finish: Option<&str>) -> String {
    json!({
        "id": "chatcmpl-probe",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }]
    })
    .to_string()
}

/// Build the deterministic synthetic stream used by the probe endpoint.
///
/// Two reasoning deltas arrive 400 ms apart, followed by one text delta. If a
/// client displays both deltas before the answer, the path is streaming.
pub fn probe_frames(format: ReasoningFormat, null_content: bool) -> Vec<ProbeFrame> {
    let model = format!("agentgate/probe-{}", format_name(format));
    let mut frames = Vec::new();
    let mut writer = SseWriter::new(format, null_content);

    frames.push(ProbeFrame {
        delay: Duration::ZERO,
        data: chunk(&model, json!({ "role": "assistant" }), None),
    });

    for (index, text) in [
        "probe reasoning #1 (t=0.0s)\n",
        "probe reasoning #2 (t=0.4s)\n",
    ]
    .into_iter()
    .enumerate()
    {
        let delay = if index == 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(400)
        };
        for delta in writer.reasoning_deltas(text) {
            frames.push(ProbeFrame {
                delay,
                data: chunk(&model, delta, None),
            });
        }
    }

    for delta in writer.reasoning_end_deltas() {
        frames.push(ProbeFrame {
            delay: Duration::ZERO,
            data: chunk(&model, delta, None),
        });
    }

    for delta in writer.text_deltas("probe answer: streaming OK (t=0.8s)\n") {
        frames.push(ProbeFrame {
            delay: Duration::from_millis(400),
            data: chunk(&model, delta, None),
        });
    }

    frames.push(ProbeFrame {
        delay: Duration::ZERO,
        data: chunk(&model, json!({}), Some("stop")),
    });
    frames.push(ProbeFrame {
        delay: Duration::ZERO,
        data: "[DONE]".to_string(),
    });
    frames
}

pub fn format_name(format: ReasoningFormat) -> &'static str {
    match format {
        ReasoningFormat::ReasoningContent => "reasoning_content",
        ReasoningFormat::Reasoning => "reasoning",
        ReasoningFormat::Both => "both",
        ReasoningFormat::ThinkTags => "think_tags",
        ReasoningFormat::None => "none",
        ReasoningFormat::Passthrough => "passthrough",
    }
}

/// Router for the probe server. Kept public so integration tests can drive it
/// without binding a TCP port.
pub fn probe_router() -> Router {
    Router::new()
        .route("/", get(probe_index))
        .route("/matrix", get(probe_matrix))
        .route("/probe/reasoning", get(probe_index))
        .route("/probe/reasoning/:format", get(probe_stream))
        .route("/health", get(|| async { Json(json!({ "status": "ok" })) }))
}

async fn probe_matrix() -> Json<Value> {
    Json(client_matrix())
}

async fn probe_index() -> Html<String> {
    Html(format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>agentgate reasoning probe</title></head>
<body style="font-family:system-ui;max-width:56rem;margin:2rem auto;line-height:1.5">
<h1>agentgate reasoning probe</h1>
<p>Open one stream below directly or through 9router / AionUi. Reasoning deltas arrive
~400&nbsp;ms apart; the answer arrives last. Non-streaming is a failure.</p>
<ul>
  <li><a href="/probe/reasoning/reasoning_content"><code>reasoning_content</code></a></li>
  <li><a href="/probe/reasoning/reasoning"><code>reasoning</code></a></li>
  <li><a href="/probe/reasoning/both"><code>both</code></a></li>
  <li><a href="/probe/reasoning/think_tags"><code>think_tags</code></a></li>
  <li><a href="/probe/reasoning/none"><code>none</code> (no reasoning expected)</a></li>
</ul>
<p>Matrix: <a href="/matrix"><code>/matrix</code></a></p>
<p>Raw check: <code>curl -N http://127.0.0.1:8799/probe/reasoning/reasoning_content</code></p>
</body></html>"#
    ))
}

async fn probe_stream(Path(format): Path<String>) -> Result<impl IntoResponse, StatusCode> {
    let format = crate::reasoning::format::parse_reasoning_format(&format)
        .ok_or(StatusCode::NOT_FOUND)?;
    let frames = probe_frames(format, true);

    let stream = async_stream::stream! {
        let mut previous = Duration::ZERO;
        for frame in frames {
            let wait = frame.delay.saturating_sub(previous);
            previous = frame.delay;
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            yield Ok::<_, std::convert::Infallible>(Event::default().data(frame.data));
        }
    };

    Ok(Sse::new(stream))
}

/// Print the compatibility matrix in a human-friendly form.
pub fn print_matrix() {
    let matrix = client_matrix();
    println!("Client / 9router reasoning matrix (compat/clients.json)");
    println!();
    println!("Formats:");
    if let Some(formats) = matrix.get("formats").and_then(|v| v.as_array()) {
        for f in formats {
            println!(
                "  {:<18} {:<40} status={}",
                f.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                f.get("sse_field").and_then(|v| v.as_str()).unwrap_or("?"),
                f.get("status").and_then(|v| v.as_str()).unwrap_or("known"),
            );
        }
    }
    println!();
    println!("Clients:");
    if let Some(clients) = matrix.get("clients").and_then(|v| v.as_array()) {
        for c in clients {
            println!(
                "  {:<20} recommended={:<18} fallback={:<12} {}",
                c.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                c.get("recommended_format").and_then(|v| v.as_str()).unwrap_or("?"),
                c.get("fallback_format").and_then(|v| v.as_str()).unwrap_or("?"),
                c.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
            );
        }
    }
    println!();
    println!("Intermediaries:");
    if let Some(items) = matrix.get("intermediaries").and_then(|v| v.as_array()) {
        for i in items {
            println!(
                "  {:<20} preserves_reasoning={:<10} buffers_sse={:<10} {}",
                i.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                i.get("preserves_reasoning_fields").and_then(|v| v.as_str()).unwrap_or("?"),
                i.get("buffers_sse").and_then(|v| v.as_str()).unwrap_or("?"),
                i.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
            );
        }
    }
}

/// Run the synthetic probe server.
pub async fn run_probe(
    host: &str,
    port: u16,
    seconds: Option<u64>,
) -> anyhow::Result<()> {
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("agentgate reasoning probe listening on http://{}", addr);
    println!();
    print_matrix();
    println!();
    println!("Open these direct or through 9router, then through AionUi:");
    for format in [
        "reasoning_content",
        "reasoning",
        "both",
        "think_tags",
        "none",
    ] {
        println!("  http://{}/probe/reasoning/{}", addr, format);
    }
    println!();
    println!("Raw curl check:");
    println!(
        "  curl -N http://{}/probe/reasoning/reasoning_content",
        addr
    );
    if let Some(secs) = seconds {
        println!("Server stops automatically after {} seconds (Ctrl+C also works).", secs);
        let server = axum::serve(listener, probe_router());
        match tokio::time::timeout(Duration::from_secs(secs), server).await {
            Ok(result) => result?,
            Err(_) => println!("probe window elapsed"),
        }
    } else {
        axum::serve(listener, probe_router()).await?;
    }
    Ok(())
}

/// Print provider availability plus the matrix.
pub async fn run_report(registry: &crate::service::ServiceRegistry) {
    println!("agentgate doctor");
    println!();
    match registry.list_providers().await {
        Ok(providers) => {
            if providers.is_empty() {
                println!("Providers: none configured");
            } else {
                println!("Providers:");
                for p in providers {
                    println!(
                        "  {:<16} {:<8} bin={:<20} models={}",
                        p.id,
                        if p.is_available { "OK" } else { "MISSING" },
                        p.bin,
                        p.models.len()
                    );
                }
            }
        }
        Err(err) => println!("Providers: error: {}", err),
    }

    let cfg = registry.reasoning_config();
    println!();
    println!("Reasoning:");
    println!("  enabled              = {}", cfg.enabled);
    println!("  format               = {}", cfg.format);
    println!("  fallback             = {}", cfg.fallback);
    println!("  activity_lang        = {}", cfg.activity_lang);
    println!("  omitted_marker       = {}", cfg.omitted_marker);
    println!("  max_chars_per_turn   = {}", cfg.max_chars_per_turn);
    println!("  max_blocks_per_turn  = {}", cfg.max_blocks_per_turn);
    println!("  redact_patterns      = {}", cfg.redact_patterns.len());
    println!();
    print_matrix();
    println!();
    println!("Run `agentgate doctor --probe-reasoning` to verify the client chain.");
}

/// Map a successful probe format back to the config value.
pub fn recommended_format(format: ReasoningFormat) -> &'static str {
    match format {
        ReasoningFormat::ReasoningContent => "reasoning_content",
        ReasoningFormat::Reasoning => "reasoning",
        ReasoningFormat::Both => "both",
        ReasoningFormat::ThinkTags => "think_tags",
        ReasoningFormat::None => "none",
        ReasoningFormat::Passthrough => "passthrough",
    }
}

/// Config hint printed by the probe instructions.
pub fn probe_config_hint(format: ReasoningFormat) -> String {
    format!(
        "[reasoning]\nformat = \"{}\"",
        recommended_format(format)
    )
}

// Keep `ReasoningKind` referenced in this module's public docs/tests even when
// the format module changes.
#[allow(dead_code)]
const _KIND_RAW: ReasoningKind = ReasoningKind::Raw;

/// Run the full provider probe chain (D1-D4) per spec §16/§17.
/// 
/// D1: CLI direct - spawn CLI with streaming and verify reasoning output
/// D2: Gateway direct - call agentgate /v1/chat/completions directly
/// D3: Through 9router - proxy through 9router (if configured)
/// D4: Through AionUi - manual verification (prints instructions)
pub async fn run_probe_chain(
    _registry: &crate::service::ServiceRegistry,
    _host: &str,
    _port: u16,
    _seconds: u64,
) -> anyhow::Result<()> {
    eprintln!("⚠️  doctor --probe-chain is not yet implemented (P3 task)");
    eprintln!("    This will run the full D1-D4 probe chain per spec §16/§17.");
    eprintln!("    For now, use 'agentgate doctor --probe-reasoning' for synthetic format testing.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_matrix_is_valid_json() {
        let matrix = client_matrix();
        assert!(matrix.get("clients").is_some());
        assert!(matrix.get("intermediaries").is_some());
        assert_eq!(matrix["intermediaries"][0]["id"], "9router");
    }

    #[test]
    fn each_format_has_a_probe_stream() {
        for format in [
            ReasoningFormat::ReasoningContent,
            ReasoningFormat::Reasoning,
            ReasoningFormat::Both,
            ReasoningFormat::ThinkTags,
            ReasoningFormat::None,
        ] {
            let frames = probe_frames(format, true);
            assert!(frames.len() >= 4, "{format:?}");
            assert_eq!(frames.last().unwrap().data, "[DONE]");
        }
    }

    #[test]
    fn reasoning_content_probe_uses_reasoning_field() {
        let frames = probe_frames(ReasoningFormat::ReasoningContent, true);
        let joined = frames
            .iter()
            .map(|f| f.data.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("reasoning_content"));
        assert!(joined.contains("probe reasoning #1"));
        assert!(joined.contains("\"content\":null"));
    }

    #[test]
    fn think_tags_probe_opens_and_closes_once() {
        let frames = probe_frames(ReasoningFormat::ThinkTags, true);
        let joined = frames
            .iter()
            .map(|f| f.data.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(joined.matches("<think>").count(), 1);
        assert_eq!(joined.matches("</think>").count(), 1);
    }

    #[test]
    fn none_probe_has_no_reasoning_fields() {
        let frames = probe_frames(ReasoningFormat::None, true);
        let joined = frames
            .iter()
            .map(|f| f.data.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains("reasoning_content"));
        assert!(!joined.contains("reasoning\":"));
        assert!(joined.contains("probe answer"));
    }
}
