//! Best-effort redaction for reasoning/activity text (spec_reasoning §13).
//!
//! Reasoning can contain secrets (file contents, API keys, tool arguments).
//! Redaction is deliberately best-effort: it reduces accidental exposure but
//! is not a security boundary. Full reasoning is never logged.

use regex::{Regex, RegexBuilder};

/// Placeholder inserted for redacted matches.
pub const REDACTED: &str = "[redacted]";

/// Compiled redactor.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    patterns: Vec<Regex>,
    enabled: bool,
}

impl Redactor {
    /// Build from config patterns. Invalid patterns are skipped (never fail a
    /// request because of a bad regex).
    pub fn from_patterns(patterns: &[String]) -> Self {
        let patterns = patterns
            .iter()
            .filter_map(|p| {
                RegexBuilder::new(p)
                    .case_insensitive(p.to_ascii_lowercase().contains("(?i)"))
                    .build()
                    .ok()
            })
            .collect::<Vec<_>>();
        Self {
            enabled: !patterns.is_empty(),
            patterns,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    /// Redact all matches. Returns a new string; the input is untouched.
    pub fn redact(&self, input: &str) -> String {
        if !self.enabled || input.is_empty() {
            return input.to_string();
        }
        let mut out = input.to_string();
        for re in &self.patterns {
            out = re.replace_all(&out, REDACTED).to_string();
        }
        out
    }

    /// Redact in place when needed.
    pub fn redact_in_place(&self, value: &mut String) {
        if self.enabled && !value.is_empty() {
            *value = self.redact(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_api_keys_and_sk_tokens() {
        let r = Redactor::from_patterns(&[
            r"(?i)api[_-]?key\s*[:=]\s*\S+".to_string(),
            r"sk-[A-Za-z0-9_-]{16,}".to_string(),
        ]);
        let out = r.redact("here api_key=supersecret and sk-abcdefghijklmnop12345 end");
        assert!(!out.contains("supersecret"));
        assert!(!out.contains("sk-abcdefghijklmnop12345"));
        assert_eq!(out.matches(REDACTED).count(), 2);
    }

    #[test]
    fn leaves_clean_text_untouched() {
        let r = Redactor::from_patterns(&[r"secret-\d+".to_string()]);
        assert_eq!(r.redact("nothing sensitive"), "nothing sensitive");
    }

    #[test]
    fn invalid_patterns_are_ignored() {
        let r = Redactor::from_patterns(&["(".to_string(), r"token=\S+".to_string()]);
        assert_eq!(r.pattern_count(), 1);
        assert!(r.redact("token=abc").contains(REDACTED));
    }

    #[test]
    fn disabled_redactor_is_noop() {
        let r = Redactor::from_patterns(&[]);
        assert!(!r.is_enabled());
        assert_eq!(r.redact("sk-abcdefghijklmnop12345"), "sk-abcdefghijklmnop12345");
    }
}
