//! Reasoning event pipeline (RS6 + RS8).
//!
//! * best-effort redaction (`redact`),
//! * hard per-turn limits (`limit`),
//! * ordering guarantees from spec_reasoning §4.2.
//!
//! The pipeline is transport-agnostic and synchronous; the HTTP layer decides
//! how formatted deltas become SSE chunks.
//!
//! **No synthetic reasoning/activity is emitted here.** Reasoning must come only
//! from the agent CLI (`ReasoningStart`/`ReasoningDelta`/`ReasoningEnd`).
//! Gateway-side activity labels (e.g. "Menyiapkan agen…") were removed.

use std::collections::HashSet;
use std::time::Instant;

use crate::config::ReasoningConfig;

use super::event::{ReasoningKind, RunEvent};
use super::limit::{ReasoningLimiter, TRUNCATION_MARKER_EN, TRUNCATION_MARKER_ID};
use super::redact::Redactor;

#[derive(Debug, Clone)]
pub struct PipelineSettings {
    pub enabled: bool,
    pub max_chars_per_turn: usize,
    pub max_blocks_per_turn: u32,
    pub activity_lang: String,
    pub redact_patterns: Vec<String>,
}

impl Default for PipelineSettings {
    fn default() -> Self {
        Self::from_config(&ReasoningConfig::default())
    }
}

impl PipelineSettings {
    pub fn from_config(cfg: &ReasoningConfig) -> Self {
        Self {
            enabled: cfg.enabled,
            max_chars_per_turn: cfg.max_chars_per_turn,
            max_blocks_per_turn: cfg.max_blocks_per_turn,
            activity_lang: cfg.activity_lang.clone(),
            redact_patterns: cfg.redact_patterns.clone(),
        }
    }
}

#[derive(Debug)]
pub struct ReasoningPipeline {
    settings: PipelineSettings,
    redactor: Redactor,
    limiter: ReasoningLimiter,
    admitted_blocks: HashSet<u32>,
    started: bool,
    finished: bool,
    saw_reasoning: bool,
}

impl ReasoningPipeline {
    pub fn new(cfg: &ReasoningConfig) -> Self {
        Self::with_settings(PipelineSettings::from_config(cfg))
    }

    pub fn with_settings(settings: PipelineSettings) -> Self {
        let marker = if settings.activity_lang.eq_ignore_ascii_case("en") {
            TRUNCATION_MARKER_EN
        } else {
            TRUNCATION_MARKER_ID
        };
        Self {
            redactor: Redactor::from_patterns(&settings.redact_patterns),
            limiter: ReasoningLimiter::new(
                settings.max_chars_per_turn,
                settings.max_blocks_per_turn,
                marker,
            ),
            settings,
            admitted_blocks: HashSet::new(),
            started: false,
            finished: false,
            saw_reasoning: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    pub fn saw_reasoning(&self) -> bool {
        self.saw_reasoning
    }

    pub fn is_truncated(&self) -> bool {
        self.limiter.is_truncated()
    }

    /// Run start hook — no synthetic activity emitted.
    pub fn start(&mut self) -> Vec<RunEvent> {
        if self.started || self.finished {
            return Vec::new();
        }
        self.started = true;
        Vec::new()
    }

    /// Time-based tick — no synthetic activity emitted.
    pub fn tick(&mut self, _now: Instant) -> Vec<RunEvent> {
        Vec::new()
    }

    /// Parked waiting for a client-held tool result — no synthetic activity.
    pub fn on_waiting_client(&mut self) -> Vec<RunEvent> {
        Vec::new()
    }

    /// Process one run event.
    pub fn process(&mut self, event: RunEvent) -> Vec<RunEvent> {
        if self.finished {
            return Vec::new();
        }

        match event {
            RunEvent::ReasoningStart { block, kind } => {
                if !self.settings.enabled {
                    return Vec::new();
                }
                match kind {
                    ReasoningKind::Omitted => {
                        // Never fabricate reasoning/markers for omitted blocks.
                        self.admitted_blocks.remove(&block);
                        Vec::new()
                    }
                    _ => {
                        if !self.limiter.admit_block() {
                            return self.truncation_marker();
                        }
                        self.admitted_blocks.insert(block);
                        self.saw_reasoning = true;
                        vec![RunEvent::ReasoningStart { block, kind }]
                    }
                }
            }
            RunEvent::ReasoningDelta { block, text } => {
                if !self.settings.enabled {
                    return Vec::new();
                }
                // Auto-admit a block on first delta when no explicit
                // ReasoningStart arrived (some CLIs stream reasoning without one).
                if !self.admitted_blocks.contains(&block) {
                    if !self.limiter.admit_block() {
                        return self.truncation_marker();
                    }
                    self.admitted_blocks.insert(block);
                }
                let text = self.redactor.redact(&text);
                let accepted = self.limiter.admit_delta(&text);
                if accepted.is_empty() {
                    return self.truncation_marker();
                }
                self.saw_reasoning = true;
                vec![RunEvent::ReasoningDelta {
                    block,
                    text: accepted,
                }]
            }
            RunEvent::ReasoningEnd { block } => {
                if !self.settings.enabled {
                    return Vec::new();
                }
                if self.admitted_blocks.remove(&block) {
                    vec![RunEvent::ReasoningEnd { block }]
                } else {
                    Vec::new()
                }
            }
            RunEvent::Text { text } => vec![RunEvent::Text {
                text: self.redactor.redact(&text),
            }],
            RunEvent::Activity { .. } => {
                // Drop any synthetic/upstream activity; only CLI reasoning flows.
                Vec::new()
            }
            RunEvent::ToolCalls { calls } => vec![RunEvent::ToolCalls { calls }],
            RunEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => vec![RunEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            }],
            RunEvent::Done { reason } => {
                let mut out = self.truncation_marker();
                out.extend(self.close_blocks());
                self.finished = true;
                out.push(RunEvent::Done { reason });
                out
            }
            RunEvent::Error { message, code } => {
                let mut out = self.close_blocks();
                self.finished = true;
                out.push(RunEvent::Error { message, code });
                out
            }
        }
    }

    /// Final flush: close reasoning blocks that were never explicitly ended so
    /// transports can emit `ReasoningEnd` before `[DONE]`.
    pub fn finish(&mut self) -> Vec<RunEvent> {
        if self.finished {
            return Vec::new();
        }
        let mut out = self.close_blocks();
        out.extend(self.truncation_marker());
        self.finished = true;
        out
    }

    fn close_blocks(&mut self) -> Vec<RunEvent> {
        let mut blocks: Vec<u32> = self.admitted_blocks.drain().collect();
        blocks.sort_unstable();
        blocks
            .into_iter()
            .map(|block| RunEvent::ReasoningEnd { block })
            .collect()
    }

    fn truncation_marker(&mut self) -> Vec<RunEvent> {
        match self.limiter.truncation_marker() {
            Some(marker) => vec![RunEvent::ReasoningDelta {
                block: 0,
                text: marker,
            }],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline() -> ReasoningPipeline {
        let mut cfg = ReasoningConfig::default();
        cfg.activity_min_interval_ms = 0;
        cfg.redact_patterns = vec![r"sk-[A-Za-z0-9_-]{8,}".to_string()];
        ReasoningPipeline::new(&cfg)
    }

    #[test]
    fn omitted_block_emits_nothing_synthetic() {
        let mut p = pipeline();
        let out = p.process(RunEvent::ReasoningStart {
            block: 1,
            kind: ReasoningKind::Omitted,
        });
        assert!(out.is_empty(), "no synthetic marker for omitted blocks");
        let end = p.process(RunEvent::ReasoningEnd { block: 1 });
        assert!(end.is_empty());
    }

    #[test]
    fn redacts_reasoning_and_text() {
        let mut p = pipeline();
        p.process(RunEvent::ReasoningStart {
            block: 0,
            kind: ReasoningKind::Raw,
        });
        let out = p.process(RunEvent::ReasoningDelta {
            block: 0,
            text: "key sk-abcdefgh12345678".to_string(),
        });
        let text = match &out[0] {
            RunEvent::ReasoningDelta { text, .. } => text.clone(),
            _ => panic!("expected delta"),
        };
        assert!(!text.contains("sk-abcdefgh12345678"));
    }

    #[test]
    fn tool_calls_pass_through_no_synthetic_activity() {
        use crate::reasoning::event::{FunctionCall, ToolCall};
        let calls = vec![ToolCall {
            id: "call_1".into(),
            r#type: "function".into(),
            function: FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        }];
        let mut p = pipeline();
        let out = p.process(RunEvent::ToolCalls { calls: calls.clone() });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], RunEvent::ToolCalls { .. }));

        // Even without prior reasoning, never inject synthetic activity.
        let mut p = pipeline();
        p.start();
        let out = p.tick(std::time::Instant::now());
        assert!(out.is_empty());
        let out = p.process(RunEvent::ToolCalls { calls });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], RunEvent::ToolCalls { .. }));
    }
}