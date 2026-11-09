//! RS8 hardening tests: limits, redaction, Windows line endings, ordering,
//! and parallel streams (soak).

use std::time::{Duration, Instant};

use agentgate::config::ReasoningConfig;
use agentgate::reasoning::{
    format::SseWriter,
    Coalescer, CoalescerSettings, ReasoningFormat, ReasoningKind, ReasoningPipeline, RunEvent,
};

fn event_text(event: &RunEvent) -> Option<&str> {
    match event {
        RunEvent::Text { text } | RunEvent::ReasoningDelta { text, .. } => Some(text),
        _ => None,
    }
}

#[test]
fn max_chars_truncates_and_marker_emitted_once() {
    let mut cfg = ReasoningConfig::default();
    cfg.max_chars_per_turn = 10;
    cfg.activity_min_interval_ms = 0;
    let mut p = ReasoningPipeline::new(&cfg);

    p.process(RunEvent::ReasoningStart {
        block: 0,
        kind: ReasoningKind::Raw,
    });
    let first = p.process(RunEvent::ReasoningDelta {
        block: 0,
        text: "0123456789ABCDEF".to_string(),
    });
    assert_eq!(event_text(&first[0]), Some("0123456789"));
    assert!(p.is_truncated());

    let second = p.process(RunEvent::ReasoningDelta {
        block: 0,
        text: "more".to_string(),
    });
    assert_eq!(second.len(), 1);
    assert!(event_text(&second[0]).unwrap().contains("dipotong"));

    let third = p.process(RunEvent::ReasoningDelta {
        block: 0,
        text: "again".to_string(),
    });
    assert!(third.is_empty(), "marker must only be sent once");
}

#[test]
fn multibyte_truncation_is_utf8_safe() {
    let mut cfg = ReasoningConfig::default();
    cfg.max_chars_per_turn = 3;
    let mut p = ReasoningPipeline::new(&cfg);
    p.process(RunEvent::ReasoningStart {
        block: 0,
        kind: ReasoningKind::Raw,
    });
    let out = p.process(RunEvent::ReasoningDelta {
        block: 0,
        text: "日本語テキスト".to_string(),
    });
    let text = event_text(&out[0]).unwrap();
    assert_eq!(text, "日本語");
    // Valid UTF-8 by construction; round-trip through bytes.
    assert_eq!(std::str::from_utf8(text.as_bytes()).unwrap(), "日本語");
}

#[test]
fn secrets_are_redacted_before_clients_see_them() {
    let mut cfg = ReasoningConfig::default();
    cfg.redact_patterns = vec![
        r"(?i)api[_-]?key\s*[:=]\s*\S+".to_string(),
        r"sk-[A-Za-z0-9_-]{16,}".to_string(),
    ];
    let mut p = ReasoningPipeline::new(&cfg);
    p.process(RunEvent::ReasoningStart {
        block: 0,
        kind: ReasoningKind::Raw,
    });
    let out = p.process(RunEvent::ReasoningDelta {
        block: 0,
        text: "api_key=super-secret sk-abcdefghijklmnop12345".to_string(),
    });
    let text = event_text(&out[0]).unwrap();
    assert!(!text.contains("super-secret"));
    assert!(!text.contains("sk-abcdefghijklmnop12345"));
    assert!(text.contains("[redacted]"));
}

#[test]
fn windows_crlf_is_normalized_by_parser() {
    let mut parser = agentgate::reasoning::parser::ReasoningParser::new();
    let events = parser.parse_line("{\"type\":\"text\",\"delta\":\"line\\r\\n\"}\r\n");
    let text = events.iter().find_map(event_text).unwrap();
    assert!(!text.contains('\r'), "CR must not leak to clients: {text:?}");
}

#[test]
fn think_tags_are_never_split_and_close_before_text() {
    let mut writer = SseWriter::new(ReasoningFormat::ThinkTags, true);
    let deltas = writer
        .reasoning_deltas("reasoning with literal </think> inside")
        .into_iter()
        .chain(writer.reasoning_end_deltas())
        .chain(writer.text_deltas("answer"))
        .collect::<Vec<_>>();

    let content = deltas
        .iter()
        .map(|d| d.get("content").and_then(|c| c.as_str()).unwrap_or(""))
        .collect::<Vec<_>>();

    assert_eq!(content[0], "<think>\n");
    assert_eq!(content.iter().filter(|c| **c == "<think>\n").count(), 1);
    assert_eq!(content.iter().filter(|c| **c == "\n</think>\n\n").count(), 1);
    let close_idx = content.iter().position(|c| *c == "\n</think>\n\n").unwrap();
    let answer_idx = content.iter().position(|c| *c == "answer").unwrap();
    assert!(close_idx < answer_idx);
    assert!(content.iter().any(|c| c.contains('\u{200b}')));
}

#[test]
fn coalescer_keeps_reasoning_before_text_under_load() {
    let mut c = Coalescer::with_settings(CoalescerSettings {
        flush_ms: 0,
        flush_bytes: 64,
    });
    let mut seen_reasoning_end = false;
    let mut seen_text = false;

    c.add_event(RunEvent::ReasoningStart {
        block: 0,
        kind: ReasoningKind::Raw,
    });
    for i in 0..10_000 {
        let out = c.add_event(RunEvent::ReasoningDelta {
            block: 0,
            text: format!("r{i}"),
        });
        for ev in out {
            if matches!(ev, RunEvent::ReasoningEnd { .. }) {
                seen_reasoning_end = true;
            }
        }
    }
    let out = c.add_event(RunEvent::Text {
        text: "answer".to_string(),
    });
    for ev in out {
        if matches!(ev, RunEvent::Text { .. }) {
            assert!(seen_reasoning_end, "text must not precede ReasoningEnd");
            seen_text = true;
        }
        if matches!(ev, RunEvent::ReasoningEnd { .. }) {
            seen_reasoning_end = true;
        }
    }
    // Text is buffered until ReasoningEnd or force_flush. Call force_flush to emit it.
    let out = c.force_flush();
    for ev in out {
        if matches!(ev, RunEvent::Text { .. }) {
            assert!(seen_reasoning_end, "text must not precede ReasoningEnd");
            seen_text = true;
        }
        if matches!(ev, RunEvent::ReasoningEnd { .. }) {
            seen_reasoning_end = true;
        }
    }
    assert!(seen_text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_parallel_pipelines_do_not_leak_state() {
    let mut handles = Vec::new();
    for id in 0..50u32 {
        handles.push(tokio::spawn(async move {
            let mut cfg = ReasoningConfig::default();
            cfg.activity_min_interval_ms = 0;
            cfg.redact_patterns = vec![r"sk-[A-Za-z0-9_-]{8,}".to_string()];
            let mut p = ReasoningPipeline::new(&cfg);
            p.start();
            p.process(RunEvent::ReasoningStart {
                block: 0,
                kind: ReasoningKind::Raw,
            });
            let out = p.process(RunEvent::ReasoningDelta {
                block: 0,
                text: format!("stream-{id} sk-secret{id:08}"),
            });
            let text = match &out[0] {
                RunEvent::ReasoningDelta { text, .. } => text.clone(),
                _ => panic!("expected reasoning delta"),
            };
            assert!(text.contains(&format!("stream-{id}")));
            assert!(!text.contains(&format!("sk-secret{id:08}")));
            text
        }));
    }

    let started = Instant::now();
    for handle in handles {
        handle.await.unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "soak test should complete quickly"
    );
}
