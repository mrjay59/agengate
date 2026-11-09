//! Event coalescer for streaming (spec_reasoning §6.5).
//!
//! Goal: avoid thousands of 1–3 character SSE chunks while staying real-time.
//! Ordering is more important than latency: reasoning always flushes before
//! text/tool calls, and a reasoning block is closed before the first text chunk.

use super::event::{RunEvent, ReasoningKind, ToolCall};
use std::collections::VecDeque;
use std::time::Instant;

/// Coalescer settings.
#[derive(Debug, Clone)]
pub struct CoalescerSettings {
    pub flush_ms: u64,
    pub flush_bytes: usize,
}

impl Default for CoalescerSettings {
    fn default() -> Self {
        Self { flush_ms: 40, flush_bytes: 256 }
    }
}

/// Event coalescer for batched streaming.
pub struct Coalescer {
    pending_reasoning: String,
    pending_text: String,
    pending_activity: VecDeque<(super::event::ActivityKind, String)>,
    pending_tool_calls: Vec<ToolCall>,
    pending_usage: Option<super::event::Usage>,
    last_flush: Instant,
    settings: CoalescerSettings,
    current_block: u32,
    reasoning_kind: ReasoningKind,
    in_reasoning: bool,
}

impl Coalescer {
    pub fn new() -> Self {
        Self::with_settings(CoalescerSettings::default())
    }

    pub fn with_settings(settings: CoalescerSettings) -> Self {
        Self {
            pending_reasoning: String::new(),
            pending_text: String::new(),
            pending_activity: VecDeque::new(),
            pending_tool_calls: Vec::new(),
            pending_usage: None,
            last_flush: Instant::now(),
            settings,
            current_block: 0,
            reasoning_kind: ReasoningKind::Raw,
            in_reasoning: false,
        }
    }

    pub fn add_event(&mut self, event: RunEvent) -> Vec<RunEvent> {
        let mut out = Vec::new();

        match event {
            RunEvent::ReasoningStart { block, kind } => {
                // A new block cannot begin while text is pending.
                self.flush_text(&mut out);
                if self.in_reasoning {
                    self.emit_reasoning_end(&mut out);
                }
                self.current_block = block;
                self.reasoning_kind = kind;
                self.in_reasoning = true;
                out.push(RunEvent::ReasoningStart { block, kind });
            }
            RunEvent::ReasoningDelta { block, text } => {
                if !self.in_reasoning {
                    self.flush_text(&mut out);
                    self.current_block = block;
                    self.in_reasoning = true;
                    out.push(RunEvent::ReasoningStart {
                        block,
                        kind: self.reasoning_kind,
                    });
                }
                self.pending_reasoning.push_str(&text);
                if self.pending_reasoning.len() >= self.settings.flush_bytes {
                    self.flush_reasoning(&mut out);
                }
            }
            RunEvent::ReasoningEnd { block } => {
                self.flush_reasoning(&mut out);
                self.current_block = block;
                self.emit_reasoning_end(&mut out);
                // Now flush any buffered text that was waiting for reasoning to end
                self.flush_text(&mut out);
            }
            RunEvent::Text { text } => {
                // Buffer text until reasoning ends (ReasoningEnd received).
                // This ensures ALL reasoning is emitted before ANY text.
                self.pending_text.push_str(&text);
                if self.pending_text.len() >= self.settings.flush_bytes {
                    // Still respect flush_bytes for very long text, but don't flush reasoning
                    if !self.in_reasoning {
                        self.flush_text(&mut out);
                    }
                }
            }
            RunEvent::Activity { kind, label } => {
                self.flush_reasoning(&mut out);
                self.pending_activity.push_back((kind, label));
            }
            RunEvent::ToolCalls { calls } => {
                // Do NOT flush/end reasoning here — let the reasoning block (with
                // accumulated view steps) persist until a proper boundary
                // (Done/Error/text delta after tools). This groups view steps
                // into a single block instead of fragmenting per tool_call batch.
                self.flush_text(&mut out);
                self.flush_activity(&mut out);
                self.pending_tool_calls.extend(calls);
                self.flush_tools(&mut out);
            }
            RunEvent::Usage { prompt_tokens, completion_tokens, total_tokens } => {
                self.pending_usage = Some(super::event::Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                });
            }
            RunEvent::Done { reason } => {
                self.flush_reasoning(&mut out);
                if self.in_reasoning {
                    self.emit_reasoning_end(&mut out);
                }
                self.flush_text(&mut out);
                self.flush_activity(&mut out);
                self.flush_tools(&mut out);
                self.flush_usage(&mut out);
                out.push(RunEvent::Done { reason });
            }
            RunEvent::Error { message, code } => {
                self.flush_reasoning(&mut out);
                if self.in_reasoning {
                    self.emit_reasoning_end(&mut out);
                }
                self.flush_text(&mut out);
                self.flush_activity(&mut out);
                out.push(RunEvent::Error { message, code });
            }
        }

        if self.should_flush() {
            out.extend(self.flush());
        }

        out
    }

    fn should_flush(&self) -> bool {
        let elapsed = self.last_flush.elapsed().as_millis() as u64;
        let bytes = self.pending_reasoning.len() + self.pending_text.len();
        elapsed >= self.settings.flush_ms || bytes >= self.settings.flush_bytes
    }

    /// Flush buffered deltas without closing an open reasoning block.
    pub fn flush(&mut self) -> Vec<RunEvent> {
        let mut out = Vec::new();
        self.flush_reasoning(&mut out);
        // Don't flush text if reasoning is still ongoing (wait for ReasoningEnd)
        if !self.in_reasoning {
            self.flush_text(&mut out);
        }
        self.flush_activity(&mut out);
        self.flush_tools(&mut out);
        self.flush_usage(&mut out);
        self.last_flush = Instant::now();
        out
    }

    /// Flush everything and close an open reasoning block (turn boundary).
    pub fn force_flush(&mut self) -> Vec<RunEvent> {
        let mut out = self.flush();
        if self.in_reasoning {
            self.emit_reasoning_end(&mut out);
            // Flush text after reasoning ends
            self.flush_text(&mut out);
        } else if !self.pending_text.is_empty() {
            self.flush_text(&mut out);
        }
        out
    }

    fn flush_reasoning(&mut self, out: &mut Vec<RunEvent>) {
        if !self.pending_reasoning.is_empty() {
            out.push(RunEvent::ReasoningDelta {
                block: self.current_block,
                text: std::mem::take(&mut self.pending_reasoning),
            });
        }
    }

    fn emit_reasoning_end(&mut self, out: &mut Vec<RunEvent>) {
        if self.in_reasoning {
            out.push(RunEvent::ReasoningEnd {
                block: self.current_block,
            });
            self.in_reasoning = false;
        }
    }

    fn flush_text(&mut self, out: &mut Vec<RunEvent>) {
        if !self.pending_text.is_empty() {
            out.push(RunEvent::Text {
                text: std::mem::take(&mut self.pending_text),
            });
        }
    }

    fn flush_activity(&mut self, out: &mut Vec<RunEvent>) {
        while let Some((kind, label)) = self.pending_activity.pop_front() {
            out.push(RunEvent::Activity { kind, label });
        }
    }

    fn flush_tools(&mut self, out: &mut Vec<RunEvent>) {
        if !self.pending_tool_calls.is_empty() {
            out.push(RunEvent::ToolCalls {
                calls: std::mem::take(&mut self.pending_tool_calls),
            });
        }
    }

    fn flush_usage(&mut self, out: &mut Vec<RunEvent>) {
        if let Some(usage) = self.pending_usage.take() {
            out.push(RunEvent::Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
            });
        }
    }

    pub fn set_reasoning_kind(&mut self, kind: ReasoningKind) {
        self.reasoning_kind = kind;
    }
}

impl Default for Coalescer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reasoning::event::ReasoningKind;

    #[test]
    fn reasoning_is_flushed_before_first_text() {
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
    fn force_flush_closes_reasoning() {
        let mut c = Coalescer::with_settings(CoalescerSettings {
            flush_ms: 60_000,
            flush_bytes: 1_000_000,
        });
        c.add_event(RunEvent::ReasoningStart { block: 0, kind: ReasoningKind::Summary });
        c.add_event(RunEvent::ReasoningDelta { block: 0, text: "x".into() });
        let out = c.force_flush();
        assert!(out.iter().any(|e| matches!(e, RunEvent::ReasoningDelta { .. })));
        assert!(out.iter().any(|e| matches!(e, RunEvent::ReasoningEnd { .. })));
    }
}
