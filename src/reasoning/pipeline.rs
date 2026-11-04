//! Reasoning event pipeline (RS6 + RS8).
//!
//! Combines:
//! * activity fallback + omitted marker (`fallback`),
//! * best-effort redaction (`redact`),
//! * hard per-turn limits (`limit`),
//! * ordering guarantees from spec_reasoning §4.2.
//!
//! The pipeline is transport-agnostic and synchronous; the HTTP layer decides
//! how formatted deltas become SSE chunks.

use std::collections::HashSet;
use std::time::Instant;

use crate::config::ReasoningConfig;

use super::event::{RunEvent, ReasoningKind};
use super::fallback::{ActivityTracker, FallbackSettings};
use super::limit::{ReasoningLimiter, TRUNCATION_MARKER_EN, TRUNCATION_MARKER_ID};
use super::redact::Redactor;

#[derive(Debug, Clone)]
pub struct PipelineSettings {
    pub enabled: bool,
    pub max_chars_per_turn: usize,
    pub max_blocks_per_turn: u32,
    pub activity_lang: String,
    pub redact_patterns: Vec<String>,
    pub fallback: FallbackSettings,
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
            fallback: FallbackSettings::from_config(cfg),
        }
    }
}

#[derive(Debug)]
pub struct ReasoningPipeline {
    settings: PipelineSettings,
    redactor: Redactor,
    limiter: ReasoningLimiter,
    tracker: ActivityTracker,
    admitted_blocks: HashSet<u32>,
    started: bool,
    finished: bool,
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
            tracker: ActivityTracker::new(settings.fallback.clone()),
            settings,
            admitted_blocks: HashSet::new(),
            started: false,
            finished: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.settings.enabled
    }

    pub fn saw_reasoning(&self) -> bool {
        self.tracker.saw_reasoning()
    }

    pub fn is_truncated(&self) -> bool {
        self.limiter.is_truncated()
    }

    /// Emit the run-start activity exactly once.
    pub fn start(&mut self) -> Vec<RunEvent> {
        if self.started || self.finished {
            return Vec::new();
        }
        self.started = true;
        self.tracker.on_start().into_iter().collect()
    }

    /// Time-based fallback; call periodically (e.g. every 250 ms).
    pub fn tick(&mut self, now: Instant) -> Vec<RunEvent> {
        if self.finished {
            return Vec::new();
        }
        self.tracker.tick(now).into_iter().collect()
    }

    /// Parked waiting for a client-held tool result.
    pub fn on_waiting_client(&mut self) -> Vec<RunEvent> {
        self.tracker.on_waiting_client().into_iter().collect()
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
                        self.admitted_blocks.remove(&block);
                        self.tracker.on_omitted().into_iter().collect()
                    }
                    _ => {
                        if !self.limiter.admit_block() {
                            return self.truncation_marker();
                        }
                        self.admitted_blocks.insert(block);
                        self.tracker.mark_reasoning();
                        vec![RunEvent::ReasoningStart { block, kind }]
                    }
                }
            }
            RunEvent::ReasoningDelta { block, text } => {
                if !self.settings.enabled || !self.admitted_blocks.contains(&block) {
                    return Vec::new();
                }
                let text = self.redactor.redact(&text);
                let accepted = self.limiter.admit_delta(&text);
                if accepted.is_empty() {
                    return self.truncation_marker();
                }
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
            RunEvent::Text { text } => {
                self.tracker.mark_text();
                vec![RunEvent::Text {
                    text: self.redactor.redact(&text),
                }]
            }
            RunEvent::Activity { kind, label } => {
                if !self.settings.enabled && !self.settings.fallback.emits_activity() {
                    return Vec::new();
                }
                vec![RunEvent::Activity {
                    kind,
                    label: self.redactor.redact(&label),
                }]
            }
            RunEvent::ToolCalls { calls } => {
                let mut out = Vec::new();
                // Activity is a fallback: do not interleave synthetic tool
                // labels into a stream that already carries real reasoning.
                if !self.tracker.saw_reasoning() {
                    if let Some(first) = calls.first() {
                        if let Some(activity) = self.tracker.on_tool_requested(Some(&first.function.name)) {
                            out.push(activity);
                        }
                    }
                }
                out.push(RunEvent::ToolCalls { calls });
                out
            }
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
                self.tracker.mark_text();
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
    use crate::reasoning::event::ActivityKind;

    fn pipeline() -> ReasoningPipeline {
        let mut cfg = ReasoningConfig::default();
        cfg.activity_min_interval_ms = 0;
        cfg.redact_patterns = vec![r"sk-[A-Za-z0-9_-]{8,}".to_string()];
        ReasoningPipeline::new(&cfg)
    }

    #[test]
    fn omitted_block_emits_marker_without_delta() {
        let mut p = pipeline();
        let out = p.process(RunEvent::ReasoningStart {
            block: 1,
            kind: ReasoningKind::Omitted,
        });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], RunEvent::Activity { .. }));
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
    fn tool_activity_is_fallback_only() {
        let mut p = pipeline();
        let calls = vec![crate::reasoning::event::ToolCall {
            id: "call_1".into(),
            r#type: "function".into(),
            function: crate::reasoning::event::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        }];
        let out = p.process(RunEvent::ToolCalls { calls: calls.clone() });
        assert!(matches!(out[0], RunEvent::Activity { kind: ActivityKind::ToolRequested, .. }));

        let mut p = pipeline();
        p.process(RunEvent::ReasoningStart {
            block: 0,
            kind: ReasoningKind::Raw,
        });
        let out = p.process(RunEvent::ToolCalls { calls });
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], RunEvent::ToolCalls { .. }));
    }
}
