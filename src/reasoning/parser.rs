//! Reasoning parser for CLI output.
//!
//! Tolerant by design: unknown events are ignored, malformed JSON lines are
//! skipped (RS16), CRLF is normalized (RS22), and oversized lines are dropped
//! before parsing so a hostile child cannot exhaust memory.

use super::event::{ActivityKind, FinishReason, RunEvent, ReasoningKind};
use super::limit::normalize_line_endings;

/// Maximum accepted stdout line (4 MiB).
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Parser for reasoning from CLI output.
pub struct ReasoningParser {
    current_block: u32,
    in_reasoning: bool,
    reasoning_kind: ReasoningKind,
    done: bool,
}

impl ReasoningParser {
    pub fn new() -> Self {
        Self {
            current_block: 0,
            in_reasoning: false,
            reasoning_kind: ReasoningKind::Raw,
            done: false,
        }
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<RunEvent> {
        if line.len() > MAX_LINE_BYTES {
            return Vec::new();
        }
        let line = normalize_line_endings(line);
        let line = line.trim_end_matches(['\r', '\n']).trim();
        if line.is_empty() {
            return Vec::new();
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            return self.parse_json(&value);
        }
        // A line that looks like JSON but failed to parse is corrupt (RS16):
        // skip it instead of forwarding it as assistant text.
        if line.starts_with('{') {
            return Vec::new();
        }

        // Plain text fallback (crosses line boundaries).
        if line.starts_with('[') && line.contains(']') {
            return self.parse_activity(line).into_iter().collect();
        }
        if self.in_reasoning || line.starts_with("<think>") || line.starts_with("Thinking:") {
            let mut events = self.ensure_reasoning_start(ReasoningKind::Raw);
            events.push(RunEvent::ReasoningDelta {
                block: self.current_block,
                text: line.to_string(),
            });
            return events;
        }

        let mut events = self.end_reasoning();
        events.push(RunEvent::Text { text: line.to_string() });
        events
    }

    fn parse_json(&mut self, value: &serde_json::Value) -> Vec<RunEvent> {
        // Claude stream-json wraps raw API events in `stream_event.event`.
        if value.get("type").and_then(|v| v.as_str()) == Some("stream_event") {
            if let Some(inner) = value.get("event") {
                return self.parse_claude_event(inner);
            }
            return Vec::new();
        }
        self.parse_claude_event(value)
    }

    fn parse_claude_event(&mut self, value: &serde_json::Value) -> Vec<RunEvent> {
        let event_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let block = value
            .get("index")
            .or_else(|| value.get("block"))
            .and_then(|v| v.as_u64())
            .unwrap_or(self.current_block as u64) as u32;

        match event_type {
            "content_block_start" => {
                let block_type = value
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match block_type {
                    "thinking" => {
                        self.current_block = block;
                        self.in_reasoning = true;
                        self.reasoning_kind = ReasoningKind::Summary;
                        vec![RunEvent::ReasoningStart {
                            block,
                            kind: ReasoningKind::Summary,
                        }]
                    }
                    "redacted_thinking" => {
                        self.current_block = block;
                        self.in_reasoning = false;
                        // Omitted blocks produce start+end only; the pipeline
                        // decides whether to display a marker.
                        vec![
                            RunEvent::ReasoningStart {
                                block,
                                kind: ReasoningKind::Omitted,
                            },
                            RunEvent::ReasoningEnd { block },
                        ]
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_delta" => {
                let delta = match value.get("delta") {
                    Some(d) => d,
                    None => return Vec::new(),
                };
                match delta.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "thinking_delta" => {
                        let text = delta.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
                        if text.is_empty() {
                            return Vec::new();
                        }
                        self.current_block = block;
                        let mut events = self.ensure_reasoning_start(ReasoningKind::Summary);
                        events.push(RunEvent::ReasoningDelta {
                            block,
                            text: normalize_line_endings(text),
                        });
                        events
                    }
                    // Signatures/ciphertext are never forwarded (RS-S5).
                    "signature_delta" => Vec::new(),
                    "text_delta" => {
                        let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        if text.is_empty() {
                            return Vec::new();
                        }
                        let mut events = self.end_reasoning();
                        events.push(RunEvent::Text {
                            text: normalize_line_endings(text),
                        });
                        events
                    }
                    _ => Vec::new(),
                }
            }
            "content_block_stop" => {
                if self.in_reasoning && self.current_block == block {
                    self.end_reasoning()
                } else {
                    Vec::new()
                }
            }
            "message_start" | "message_delta" | "message_stop" | "ping" | "init" | "step_start" => Vec::new(),

            // Gemini: a `thought` carries reasoning text.
            "thought" => {
                let text = value
                    .get("thought")
                    .and_then(|t| t.as_str())
                    .or_else(|| value.get("text").and_then(|t| t.as_str()))
                    .or_else(|| value.get("content").and_then(|t| t.as_str()))
                    .unwrap_or("");
                if text.is_empty() {
                    return Vec::new();
                }
                self.current_block = block;
                let mut events = self.ensure_reasoning_start(ReasoningKind::Raw);
                events.push(RunEvent::ReasoningDelta {
                    block,
                    text: normalize_line_endings(text),
                });
                events
            }
            // Gemini: a `message` carries assistant text.
            "message" => {
                let text = value
                    .get("content")
                    .and_then(|t| t.as_str())
                    .or_else(|| value.get("text").and_then(|t| t.as_str()))
                    .or_else(|| value.get("delta").and_then(|t| t.as_str()))
                    .unwrap_or("");
                if text.is_empty() {
                    return Vec::new();
                }
                let mut events = self.end_reasoning();
                events.push(RunEvent::Text {
                    text: normalize_line_endings(text),
                });
                events
            }
            // Gemini: error event.
            "error" => {
                let message = value
                    .get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| value.get("error").and_then(|e| e.as_str()))
                    .unwrap_or("unknown error")
                    .to_string();
                let code = value.get("code").and_then(|c| c.as_str()).map(|s| s.to_string());
                vec![RunEvent::Error { message, code }]
            }
            // Legacy / simplified event shapes used by tests and other CLIs.
            "thinking" | "reasoning" => {
                let text = value
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .or_else(|| value.get("text").and_then(|d| d.as_str()))
                    .or_else(|| {
                        value
                            .get("part")
                            .and_then(|p| p.get("text"))
                            .and_then(|t| t.as_str())
                    })
                    .unwrap_or("");
                if text.is_empty() {
                    // Empty thinking payload = omitted block.
                    self.in_reasoning = false;
                    return vec![
                        RunEvent::ReasoningStart {
                            block,
                            kind: ReasoningKind::Omitted,
                        },
                        RunEvent::ReasoningEnd { block },
                    ];
                }
                self.current_block = block;
                let mut events = self.ensure_reasoning_start(ReasoningKind::Raw);
                events.push(RunEvent::ReasoningDelta {
                    block,
                    text: normalize_line_endings(text),
                });
                events
            }
            "thinking_end" | "reasoning_end" | "redacted_thinking" => self.end_reasoning(),
            "text" | "content" => {
                let text = value
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .or_else(|| value.get("text").and_then(|d| d.as_str()))
                    .or_else(|| {
                        value
                            .get("part")
                            .and_then(|p| p.get("text"))
                            .and_then(|t| t.as_str())
                    })
                    .unwrap_or("");
                if text.is_empty() {
                    return Vec::new();
                }
                let mut events = self.end_reasoning();
                events.push(RunEvent::Text {
                    text: normalize_line_endings(text),
                });
                events
            }
            "tool_calls" | "tool" | "tool_use" => {
                let calls = value
                    .get("calls")
                    .and_then(|c| c.as_array())
                    .map(|arr| arr.as_slice())
                    .or_else(|| value.get("tool_calls").and_then(|c| c.as_array()).map(|a| a.as_slice()))
                    .unwrap_or(&[]);
                let tool_calls: Vec<super::event::ToolCall> = calls
                    .iter()
                    .filter_map(|c| {
                        Some(super::event::ToolCall {
                            id: c.get("id")?.as_str()?.to_string(),
                            r#type: c
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("function")
                                .to_string(),
                            function: super::event::FunctionCall {
                                name: c.get("function")?.get("name")?.as_str()?.to_string(),
                                arguments: c
                                    .get("function")?
                                    .get("arguments")
                                    .map(|a| {
                                        a.as_str()
                                            .map(|s| s.to_string())
                                            .unwrap_or_else(|| a.to_string())
                                    })
                                    .unwrap_or_default(),
                            },
                        })
                    })
                    .collect();
                if tool_calls.is_empty() {
                    Vec::new()
                } else {
                    let mut events = self.end_reasoning();
                    events.push(RunEvent::ToolCalls { calls: tool_calls });
                    events
                }
            }
            "activity" => {
                let kind = match value.get("kind").and_then(|v| v.as_str()).unwrap_or("") {
                    "starting" => ActivityKind::Starting,
                    "thinking" => ActivityKind::Thinking,
                    "tool_requested" => ActivityKind::ToolRequested,
                    "waiting_client" => ActivityKind::WaitingClient,
                    "writing" => ActivityKind::Writing,
                    _ => ActivityKind::Thinking,
                };
                value
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|label| {
                        vec![RunEvent::Activity {
                            kind,
                            label: label.to_string(),
                        }]
                    })
                    .unwrap_or_default()
            }
            "usage" | "step_finish" => {
                let usage = value.get("usage").unwrap_or(value);
                let tokens = usage.get("tokens").unwrap_or(usage);
                let prompt = tokens
                    .get("prompt_tokens")
                    .or_else(|| tokens.get("input"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let completion = tokens
                    .get("completion_tokens")
                    .or_else(|| tokens.get("output"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if prompt == 0 && completion == 0 {
                    return Vec::new();
                }
                vec![RunEvent::Usage {
                    prompt_tokens: prompt as u32,
                    completion_tokens: completion as u32,
                    total_tokens: (prompt + completion) as u32,
                }]
            }
            "done" | "finish" | "result" => {
                let mut events = self.end_reasoning();
                if !self.done {
                    self.done = true;
                    events.push(RunEvent::Done {
                        reason: FinishReason::Stop,
                    });
                }
                events
            }
            _ => Vec::new(),
        }
    }

    fn ensure_reasoning_start(&mut self, kind: ReasoningKind) -> Vec<RunEvent> {
        if self.in_reasoning {
            return Vec::new();
        }
        self.in_reasoning = true;
        self.reasoning_kind = kind;
        vec![RunEvent::ReasoningStart {
            block: self.current_block,
            kind,
        }]
    }

    fn end_reasoning(&mut self) -> Vec<RunEvent> {
        if self.in_reasoning {
            self.in_reasoning = false;
            let block = self.current_block;
            self.current_block = self.current_block.wrapping_add(1);
            vec![RunEvent::ReasoningEnd { block }]
        } else {
            Vec::new()
        }
    }

    fn parse_activity(&mut self, line: &str) -> Option<RunEvent> {
        let lower = line.to_ascii_lowercase();
        if lower.contains("thinking") {
            Some(RunEvent::Activity {
                kind: ActivityKind::Thinking,
                label: "thinking".to_string(),
            })
        } else if lower.contains("tool") {
            Some(RunEvent::Activity {
                kind: ActivityKind::ToolRequested,
                label: "tool".to_string(),
            })
        } else if lower.contains("waiting") {
            Some(RunEvent::Activity {
                kind: ActivityKind::WaitingClient,
                label: "waiting".to_string(),
            })
        } else {
            None
        }
    }

    pub fn finish(&mut self) -> Vec<RunEvent> {
        let mut events = self.end_reasoning();
        if !self.done {
            self.done = true;
            events.push(RunEvent::Done {
                reason: FinishReason::Stop,
            });
        }
        events
    }
}

impl Default for ReasoningParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_thinking_delta_sequence() {
        let mut p = ReasoningParser::new();
        let start = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}}"#);
        assert!(matches!(start[0], RunEvent::ReasoningStart { kind: ReasoningKind::Summary, .. }));
        let d = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}}"#);
        assert!(matches!(d[0], RunEvent::ReasoningDelta { .. }));
        let sig = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"secret"}}}"#);
        assert!(sig.is_empty());
        let stop = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#);
        assert!(matches!(stop[0], RunEvent::ReasoningEnd { .. }));
        let text = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"answer"}}}"#);
        assert!(matches!(text[0], RunEvent::Text { .. }));
    }

    #[test]
    fn redacted_thinking_is_omitted_without_content() {
        let mut p = ReasoningParser::new();
        let out = p.parse_line(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking"}}}"#);
        assert!(matches!(out[0], RunEvent::ReasoningStart { kind: ReasoningKind::Omitted, .. }));
        assert!(matches!(out[1], RunEvent::ReasoningEnd { .. }));
    }

    #[test]
    fn crlf_and_malformed_lines() {
        let mut p = ReasoningParser::new();
        let out = p.parse_line("{\"type\":\"text\",\"delta\":\"hi\"}\r\n");
        assert!(matches!(out[0], RunEvent::Text { .. }));
        assert!(p.parse_line("{not json").is_empty());
    }

    #[test]
    fn oversized_line_is_dropped() {
        let mut p = ReasoningParser::new();
        let line = "x".repeat(MAX_LINE_BYTES + 1);
        assert!(p.parse_line(&line).is_empty());
    }
}
