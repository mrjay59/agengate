//! `reasoning.fallback`
//!
//! The gateway never fabricates model reasoning. When a CLI does not expose
//! thinking (or exposes an *omitted* block) we emit nothing synthetic:
//! reasoning comes only from the agent CLI (`ReasoningDelta`/`ReasoningStart`).
//! Synthetic gateway activity labels ("Menyiapkan agen", "Model sedang bekerja")
//! were removed — they were not real model reasoning and added noise.
//!
//! Kept: config parsing types for `reasoning.*` fields so `agentgate.toml`
//! deserialization stays backward-compatible.

use crate::config::ReasoningConfig;

/// `reasoning.fallback`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackMode {
    None,
    Activity,
    Induce,
}

impl FallbackMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "induce" => Self::Induce,
            _ => Self::Activity,
        }
    }
}

/// `reasoning.omitted_marker`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmittedMarker {
    Activity,
    None,
}

impl OmittedMarker {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Self::None,
            _ => Self::Activity,
        }
    }
}

/// Runtime settings derived from [`ReasoningConfig`].
#[derive(Debug, Clone)]
pub struct FallbackSettings {
    pub mode: FallbackMode,
    pub via_reasoning: bool,
    pub include_args: bool,
    pub min_interval: std::time::Duration,
    pub prefix: String,
    pub omitted_marker: OmittedMarker,
    /// Delay before any generic "model is working" label.
    pub thinking_delay: std::time::Duration,
}

impl Default for FallbackSettings {
    fn default() -> Self {
        Self::from_config(&ReasoningConfig::default())
    }
}

impl FallbackSettings {
    pub fn from_config(cfg: &ReasoningConfig) -> Self {
        Self {
            mode: FallbackMode::parse(&cfg.fallback),
            via_reasoning: cfg.activity_via_reasoning,
            include_args: cfg.activity_args,
            min_interval: std::time::Duration::from_millis(cfg.activity_min_interval_ms),
            prefix: cfg.activity_prefix.clone(),
            omitted_marker: OmittedMarker::parse(&cfg.omitted_marker),
            thinking_delay: std::time::Duration::from_millis(2_000),
        }
    }
}

/// No synthetic labels are emitted. Kept for API compatibility.
pub fn activity_label(_kind: &str, _lang: &str) -> Option<String> {
    None
}

/// No synthetic labels are emitted.
pub fn omitted_label(_lang: &str) -> Option<String> {
    None
}