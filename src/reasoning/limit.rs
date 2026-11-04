//! Hard limits for reasoning (spec_reasoning §6.6 / RS8).
//!
//! Reasoning is untrusted, potentially huge, and never persisted. Limits are
//! applied per assistant turn:
//! * `max_chars_per_turn` – dropped characters trigger one truncation marker;
//! * `max_blocks_per_turn` – later blocks are dropped.

/// Marker sent once when reasoning has been truncated.
pub const TRUNCATION_MARKER_ID: &str = "… [reasoning dipotong]";
pub const TRUNCATION_MARKER_EN: &str = "… [reasoning truncated]";

#[derive(Debug, Clone)]
pub struct ReasoningLimiter {
    max_chars: usize,
    max_blocks: u32,
    chars_used: usize,
    blocks_seen: u32,
    truncated: bool,
    marker_sent: bool,
    marker: String,
}

impl ReasoningLimiter {
    pub fn new(max_chars: usize, max_blocks: u32, marker: impl Into<String>) -> Self {
        Self {
            max_chars,
            max_blocks,
            chars_used: 0,
            blocks_seen: 0,
            truncated: false,
            marker_sent: false,
            marker: marker.into(),
        }
    }

    pub fn with_default_marker(max_chars: usize, max_blocks: u32) -> Self {
        Self::new(max_chars, max_blocks, TRUNCATION_MARKER_ID)
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn chars_used(&self) -> usize {
        self.chars_used
    }

    pub fn blocks_seen(&self) -> u32 {
        self.blocks_seen
    }

    /// Admit a new reasoning block. Returns `true` when the block is allowed.
    pub fn admit_block(&mut self) -> bool {
        self.blocks_seen = self.blocks_seen.saturating_add(1);
        if self.blocks_seen > self.max_blocks {
            self.truncated = true;
            return false;
        }
        true
    }

    /// Admit a reasoning delta, truncating on a UTF-8 character boundary.
    ///
    /// Returns the accepted text. When nothing can be accepted, an empty
    /// string is returned; `truncation_marker()` yields the one-shot marker.
    pub fn admit_delta(&mut self, text: &str) -> String {
        if text.is_empty() || self.chars_used >= self.max_chars {
            if !text.is_empty() {
                self.truncated = true;
            }
            return String::new();
        }

        let remaining = self.max_chars - self.chars_used;
        let char_count = text.chars().count();
        if char_count <= remaining {
            self.chars_used += char_count;
            return text.to_string();
        }

        self.truncated = true;
        // Take `remaining` chars, never splitting a UTF-8 scalar value.
        let cut: String = text.chars().take(remaining).collect();
        self.chars_used += cut.chars().count();
        cut
    }

    /// One-shot truncation marker (at most once per turn).
    pub fn truncation_marker(&mut self) -> Option<String> {
        if self.truncated && !self.marker_sent {
            self.marker_sent = true;
            return Some(self.marker.clone());
        }
        None
    }
}

/// Escape literal `</think>` sequences inside reasoning text so they cannot
/// close the wrapping `<think>` block early (RS13).
pub fn escape_think_close(text: &str) -> String {
    // Split-case-insensitive replacement of the literal close tag.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.to_ascii_lowercase().find("</think>") {
        out.push_str(&rest[..idx]);
        out.push_str("</\u{200b}think>");
        rest = &rest[idx + "</think>".len()..];
    }
    out.push_str(rest);
    out
}

/// Normalize Windows CRLF/CR line endings.
pub fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_character_boundary() {
        let mut limiter = ReasoningLimiter::with_default_marker(5, 8);
        let got = limiter.admit_delta("안녕하세요 세계");
        assert_eq!(got, "안녕하세요");
        assert!(limiter.is_truncated());
        assert!(limiter.truncation_marker().is_some());
        // One-shot.
        assert!(limiter.truncation_marker().is_none());
    }

    #[test]
    fn block_limit_drops_later_blocks() {
        let mut limiter = ReasoningLimiter::with_default_marker(1_000, 2);
        assert!(limiter.admit_block());
        assert!(limiter.admit_block());
        assert!(!limiter.admit_block());
        assert!(limiter.is_truncated());
    }

    #[test]
    fn escape_close_tag_is_zero_width_safe() {
        let escaped = escape_think_close("a</think>b</THINK>c");
        assert!(!escaped.to_ascii_lowercase().contains("</think>"));
        assert!(escaped.contains('\u{200b}'));
    }

    #[test]
    fn normalizes_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\rc\n"), "a\nb\nc\n");
    }
}
