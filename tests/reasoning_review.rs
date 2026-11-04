//! Additional unit tests for reasoning pipeline components (RS0-RS2 review).

use agentgate::config::ReasoningConfig;
use agentgate::reasoning::event::{FinishReason, ReasoningFormat, ReasoningKind, RunEvent};
use agentgate::reasoning::format::{SseWriter, parse_reasoning_format};
use agentgate::reasoning::parser::ReasoningParser;
use agentgate::reasoning::{Coalescer, CoalescerSettings, ReasoningPipeline};

#[test]
fn parser_normalizes_crlf_plain_text() {
    let mut p = ReasoningParser::new();
    let events = p.parse_line("hello\r\nworld\r");
    // trailing CR stripped; internal? line trimmed; at least no trailing CR in text
    for e in &events {
        if let RunEvent::Text { text } = e {
            assert!(!text.contains('\r'));
        }
    }
}

#[test]
fn parser_skips_malformed_json() {
    let mut p = ReasoningParser::new();
    assert!(p.parse_line("{not json").is_empty());
    assert!(p.parse_line("{\"type\":\"text\",\"delta\":\"ok\"}").len() == 1);
}

#[test]
fn parser_handles_gemini_thought_and_message() {
    let mut p = ReasoningParser::new();
    let t = p.parse_line(r#"{"type":"thought","thought":"hmm"}"#);
    assert!(matches!(t[0], RunEvent::ReasoningStart { .. }));
    assert!(matches!(t[1], RunEvent::ReasoningDelta { .. }));
    let m = p.parse_line(r#"{"type":"message","content":"answer"}"#);
    assert!(m.iter().any(|e| matches!(e, RunEvent::Text { .. })));
}

#[test]
fn parser_handles_done_and_finish() {
    let mut p = ReasoningParser::new();
    let out = p.parse_line(r#"{"type":"done"}"#);
    assert!(out.iter().any(|e| matches!(e, RunEvent::Done { reason: FinishReason::Stop })));
    // finish is idempotent
    let fin = p.finish();
    assert!(fin.is_empty() || fin.iter().all(|e| !matches!(e, RunEvent::Done { .. })));
}

#[test]
fn parser_usage_event() {
    let mut p = ReasoningParser::new();
    let out = p.parse_line(r#"{"type":"usage","prompt_tokens":5,"completion_tokens":7}"#);
    assert!(matches!(out[0], RunEvent::Usage { prompt_tokens: 5, completion_tokens: 7, total_tokens: 12 }));
}

#[test]
fn coalescer_flushes_reasoning_before_text() {
    let mut c = Coalescer::with_settings(CoalescerSettings {
        flush_ms: 60_000,
        flush_bytes: 1_000_000,
    });
    c.add_event(RunEvent::ReasoningStart { block: 0, kind: ReasoningKind::Raw });
    c.add_event(RunEvent::ReasoningDelta { block: 0, text: "think".into() });
    let mut out = c.add_event(RunEvent::Text { text: "answer".into() });
    out.extend(c.force_flush());
    assert!(matches!(out[0], RunEvent::ReasoningDelta { .. }));
    assert!(matches!(out[1], RunEvent::ReasoningEnd { .. }));
    assert!(matches!(out[2], RunEvent::Text { .. }));
}

#[test]
fn sse_writer_all_formats_parse() {
    for f in ["reasoning_content", "reasoning", "both", "think_tags", "none", "passthrough"] {
        assert!(parse_reasoning_format(f).is_some(), "format {f}");
    }
    assert!(parse_reasoning_format("bogus").is_none());
}

#[test]
fn sse_writer_think_tags_close_before_text() {
    let mut w = SseWriter::new(ReasoningFormat::ThinkTags, true);
    let a = w.reasoning_deltas("r");
    assert_eq!(a[0]["content"], "<think>\n");
    let c = w.text_deltas("answer");
    assert_eq!(c[0]["content"], "\n</think>\n\n");
    assert_eq!(c[1]["content"], "answer");
}

#[test]
fn pipeline_omitted_emits_activity() {
    let mut cfg = ReasoningConfig::default();
    cfg.activity_min_interval_ms = 0;
    let mut p = ReasoningPipeline::new(&cfg);
    let out = p.process(RunEvent::ReasoningStart {
        block: 1,
        kind: ReasoningKind::Omitted,
    });
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], RunEvent::Activity { .. }));
}

#[test]
fn pipeline_disabled_drops_reasoning() {
    let mut cfg = ReasoningConfig::default();
    cfg.enabled = false;
    let mut p = ReasoningPipeline::new(&cfg);
    p.process(RunEvent::ReasoningStart { block: 0, kind: ReasoningKind::Raw });
    let out = p.process(RunEvent::ReasoningDelta { block: 0, text: "x".into() });
    assert!(out.is_empty());
}

#[test]
fn activity_tracker_on_start_once() {
    let mut cfg = ReasoningConfig::default();
    cfg.activity_min_interval_ms = 0;
    let settings = agentgate::reasoning::fallback::FallbackSettings::from_config(&cfg);
    let mut t = agentgate::reasoning::fallback::ActivityTracker::new(settings);
    assert!(t.on_start().is_some());
    assert!(t.on_start().is_none());
}

#[test]
fn redactor_disabled_noop() {
    let r = agentgate::reasoning::Redactor::from_patterns(&[]);
    assert_eq!(r.redact("sk-abcdefghijklmnop12345"), "sk-abcdefghijklmnop12345");
}

#[test]
fn limiter_block_limit() {
    let mut l = agentgate::reasoning::ReasoningLimiter::with_default_marker(1000, 1);
    assert!(l.admit_block());
    assert!(!l.admit_block());
    assert!(l.is_truncated());
}
