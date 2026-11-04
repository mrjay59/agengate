//! Reasoning format output (spec_reasoning §5).
//!
//! The SSE writer is stateful because `think_tags` needs exactly one opening
//! `<think>` chunk and one closing `</think>` chunk, and literal close tags in
//! model output must be escaped.

use serde_json::{json, Value};

use super::event::{ReasoningFormat, ReasoningKind};
use super::limit::escape_think_close;

/// Reasoning output container (non-stream).
#[derive(Debug, Clone, Default)]
pub struct ReasoningOutput {
    pub reasoning_content: String,
    pub reasoning: String,
    pub think_tags: String,
}

/// Format a complete reasoning string (non-streaming).
pub fn format_reasoning(
    content: &str,
    format: ReasoningFormat,
    _kind: ReasoningKind,
) -> ReasoningOutput {
    let mut output = ReasoningOutput::default();
    let escaped = escape_think_close(content);

    match format {
        ReasoningFormat::ReasoningContent => {
            output.reasoning_content = content.to_string();
        }
        ReasoningFormat::Reasoning => {
            output.reasoning = content.to_string();
        }
        ReasoningFormat::Both => {
            output.reasoning_content = content.to_string();
            output.reasoning = content.to_string();
        }
        ReasoningFormat::ThinkTags => {
            output.think_tags = format!("<think>\n{}\n</think>\n\n", escaped);
        }
        ReasoningFormat::None => {}
        ReasoningFormat::Passthrough => {
            output.reasoning_content = content.to_string();
            output.reasoning = content.to_string();
            output.think_tags = format!("<think>\n{}\n</think>\n\n", escaped);
        }
    }

    output
}

/// Stateful SSE delta writer.
#[derive(Debug, Clone)]
pub struct SseWriter {
    format: ReasoningFormat,
    null_content: bool,
    in_think: bool,
}

impl SseWriter {
    pub fn new(format: ReasoningFormat, null_content: bool) -> Self {
        Self {
            format,
            null_content,
            in_think: false,
        }
    }

    pub fn format(&self) -> ReasoningFormat {
        self.format
    }

    pub fn is_in_think(&self) -> bool {
        self.in_think
    }

    /// Convert a reasoning delta into zero or more `delta` objects.
    pub fn reasoning_deltas(&mut self, text: &str) -> Vec<Value> {
        if text.is_empty() {
            return Vec::new();
        }
        match self.format {
            ReasoningFormat::ReasoningContent => vec![self.reasoning_content_delta(text)],
            ReasoningFormat::Reasoning => vec![self.reasoning_delta(text)],
            ReasoningFormat::Both => vec![self.both_delta(text)],
            ReasoningFormat::ThinkTags => {
                let mut out = Vec::new();
                if !self.in_think {
                    self.in_think = true;
                    out.push(json!({ "content": "<think>\n" }));
                }
                // Escape literal close tags so they cannot end the block early.
                out.push(json!({ "content": escape_think_close(text) }));
                out
            }
            ReasoningFormat::None | ReasoningFormat::Passthrough => Vec::new(),
        }
    }

    /// Close the reasoning block. Returns the `</think>` chunk when needed.
    pub fn reasoning_end_deltas(&mut self) -> Vec<Value> {
        if self.format == ReasoningFormat::ThinkTags && self.in_think {
            self.in_think = false;
            vec![json!({ "content": "\n</think>\n\n" })]
        } else {
            Vec::new()
        }
    }

    /// Convert a text delta. Closes `<think>` first when still open.
    pub fn text_deltas(&mut self, text: &str) -> Vec<Value> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut out = self.reasoning_end_deltas();
        out.push(json!({ "content": text }));
        out
    }

    /// Ensure `</think>` has been emitted (e.g. before `tool_calls` or `[DONE]`).
    pub fn close_think(&mut self) -> Vec<Value> {
        self.reasoning_end_deltas()
    }

    fn reasoning_content_delta(&self, text: &str) -> Value {
        let mut delta = json!({ "reasoning_content": text });
        if self.null_content {
            delta["content"] = Value::Null;
        }
        delta
    }

    fn reasoning_delta(&self, text: &str) -> Value {
        let mut delta = json!({ "reasoning": text });
        if self.null_content {
            delta["content"] = Value::Null;
        }
        delta
    }

    fn both_delta(&self, text: &str) -> Value {
        let mut delta = json!({ "reasoning_content": text, "reasoning": text });
        if self.null_content {
            delta["content"] = Value::Null;
        }
        delta
    }
}

/// Parse `X-Agentgate-Reasoning` / config values.
pub fn parse_reasoning_format(value: &str) -> Option<ReasoningFormat> {
    match value.trim().to_ascii_lowercase().as_str() {
        "reasoning_content" => Some(ReasoningFormat::ReasoningContent),
        "reasoning" => Some(ReasoningFormat::Reasoning),
        "both" => Some(ReasoningFormat::Both),
        "think_tags" | "think" => Some(ReasoningFormat::ThinkTags),
        "none" => Some(ReasoningFormat::None),
        "passthrough" => Some(ReasoningFormat::Passthrough),
        _ => None,
    }
}

/// Get response headers for reasoning.
pub fn reasoning_headers(
    format: ReasoningFormat,
    kind: ReasoningKind,
    granularity: &str,
) -> Vec<(String, String)> {
    vec![
        ("X-Agentgate-Reasoning-Format".to_string(), format_to_str(format)),
        ("X-Agentgate-Reasoning-Kind".to_string(), kind_to_str(kind)),
        ("X-Agentgate-Reasoning-Granularity".to_string(), granularity.to_string()),
    ]
}

pub fn format_to_str(f: ReasoningFormat) -> String {
    match f {
        ReasoningFormat::ReasoningContent => "reasoning_content".to_string(),
        ReasoningFormat::Reasoning => "reasoning".to_string(),
        ReasoningFormat::Both => "both".to_string(),
        ReasoningFormat::ThinkTags => "think_tags".to_string(),
        ReasoningFormat::None => "none".to_string(),
        ReasoningFormat::Passthrough => "passthrough".to_string(),
    }
}

fn kind_to_str(k: ReasoningKind) -> String {
    match k {
        ReasoningKind::Raw => "raw".to_string(),
        ReasoningKind::Summary => "summary".to_string(),
        ReasoningKind::Omitted => "omitted".to_string(),
        ReasoningKind::Activity => "activity".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_tags_open_once_and_close_before_text() {
        let mut w = SseWriter::new(ReasoningFormat::ThinkTags, true);
        let a = w.reasoning_deltas("first");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["content"], "<think>\n");
        let b = w.reasoning_deltas("second");
        assert_eq!(b.len(), 1);
        let c = w.text_deltas("answer");
        assert_eq!(c[0]["content"], "\n</think>\n\n");
        assert_eq!(c[1]["content"], "answer");
    }

    #[test]
    fn literal_close_tag_is_escaped() {
        let mut w = SseWriter::new(ReasoningFormat::ThinkTags, true);
        let out = w.reasoning_deltas("bad </think> move");
        let content = out.last().unwrap()["content"].as_str().unwrap();
        assert!(!content.contains("</think>"));
        assert!(content.contains('\u{200b}'));
    }

    #[test]
    fn reasoning_content_can_send_null_content() {
        let mut w = SseWriter::new(ReasoningFormat::ReasoningContent, true);
        let out = w.reasoning_deltas("x");
        assert!(out[0]["content"].is_null());
        assert_eq!(out[0]["reasoning_content"], "x");

        let mut w = SseWriter::new(ReasoningFormat::ReasoningContent, false);
        let out = w.reasoning_deltas("x");
        assert!(out[0].get("content").is_none());
    }

    #[test]
    fn parses_all_formats() {
        for f in ["reasoning_content", "reasoning", "both", "think_tags", "none", "passthrough"] {
            assert!(parse_reasoning_format(f).is_some(), "format {f}");
        }
        assert!(parse_reasoning_format("bogus").is_none());
    }

    #[test]
    fn non_stream_think_tags_wraps_once() {
        let out = format_reasoning("why", ReasoningFormat::ThinkTags, ReasoningKind::Raw);
        assert_eq!(out.think_tags, "<think>\nwhy\n</think>\n\n");
    }
}
