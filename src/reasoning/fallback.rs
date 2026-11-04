//! Fallback activity + omitted-thinking marker (spec_reasoning §8).
//!
//! The gateway never fabricates model reasoning. When a CLI does not expose
//! thinking (or exposes an *omitted* block) we can only emit honest gateway
//! activity such as "preparing agent" or "requesting tool: write_file".

use std::time::{Duration, Instant};

use crate::config::ReasoningConfig;

use super::event::{ActivityKind, RunEvent};

/// `reasoning.fallback`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackMode {
    /// Send nothing.
    None,
    /// Send synthetic (but honest) gateway activity. Default.
    Activity,
    /// Ask the model to think explicitly. Experimental, never default.
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
    /// Show one activity label explaining the content is hidden.
    Activity,
    /// Show nothing at all.
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

/// Supported label languages (`reasoning.activity_lang`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityLang {
    Id,
    En,
}

impl ActivityLang {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Self::En,
            _ => Self::Id,
        }
    }
}

/// Runtime settings derived from [`ReasoningConfig`].
#[derive(Debug, Clone)]
pub struct FallbackSettings {
    pub mode: FallbackMode,
    pub via_reasoning: bool,
    pub include_args: bool,
    pub min_interval: Duration,
    pub lang: ActivityLang,
    pub prefix: String,
    pub omitted_marker: OmittedMarker,
    /// Delay before the generic "model is working" label.
    pub thinking_delay: Duration,
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
            min_interval: Duration::from_millis(cfg.activity_min_interval_ms),
            lang: ActivityLang::parse(&cfg.activity_lang),
            prefix: cfg.activity_prefix.clone(),
            omitted_marker: OmittedMarker::parse(&cfg.omitted_marker),
            thinking_delay: Duration::from_millis(2_000),
        }
    }

    /// Labels are routed through the reasoning path when enabled.
    pub fn emits_activity(&self) -> bool {
        self.mode == FallbackMode::Activity && self.via_reasoning
    }
}

/// Format an activity label with the configured prefix.
pub fn activity_label(settings: &FallbackSettings, kind: ActivityKind, tool: Option<&str>) -> String {
    let body = match (settings.lang, kind) {
        (ActivityLang::Id, ActivityKind::Starting) => "Menyiapkan agen…".to_string(),
        (ActivityLang::Id, ActivityKind::Thinking) => "Model sedang bekerja…".to_string(),
        (ActivityLang::Id, ActivityKind::ToolRequested) => match tool {
            Some(name) => format!("Meminta tool: {}", name),
            None => "Meminta tool…".to_string(),
        },
        (ActivityLang::Id, ActivityKind::WaitingClient) => {
            "Menunggu hasil tool dari klien…".to_string()
        }
        (ActivityLang::Id, ActivityKind::Writing) => "Menulis jawaban…".to_string(),
        (ActivityLang::En, ActivityKind::Starting) => "Preparing agent…".to_string(),
        (ActivityLang::En, ActivityKind::Thinking) => "Model is working…".to_string(),
        (ActivityLang::En, ActivityKind::ToolRequested) => match tool {
            Some(name) => format!("Requesting tool: {}", name),
            None => "Requesting tool…".to_string(),
        },
        (ActivityLang::En, ActivityKind::WaitingClient) => {
            "Waiting for tool result from client…".to_string()
        }
        (ActivityLang::En, ActivityKind::Writing) => "Writing answer…".to_string(),
    };

    format!("{}{}", settings.prefix, body)
}

/// Label used when a thinking block is present but its contents are omitted.
pub fn omitted_label(settings: &FallbackSettings) -> String {
    let body = match settings.lang {
        ActivityLang::Id => "Model berpikir (isi disembunyikan oleh penyedia)",
        ActivityLang::En => "Model is thinking (content hidden by provider)",
    };
    format!("{}{}", settings.prefix, body)
}

/// Stateful activity emitter.
///
/// Responsibilities:
/// * never repeat the same label twice in a row;
/// * enforce `activity_min_interval_ms`;
/// * emit a "model is working" label after a quiet period;
/// * track whether real reasoning was observed so activity is only used as a
///   fallback (or for tool/waiting status).
#[derive(Debug)]
pub struct ActivityTracker {
    settings: FallbackSettings,
    started_at: Instant,
    last_emit_at: Option<Instant>,
    last_label: Option<String>,
    saw_reasoning: bool,
    saw_text: bool,
    starting_emitted: bool,
    thinking_emitted: bool,
    pub omitted_emitted: bool,
}

impl ActivityTracker {
    pub fn new(settings: FallbackSettings) -> Self {
        Self {
            settings,
            started_at: Instant::now(),
            last_emit_at: None,
            last_label: None,
            saw_reasoning: false,
            saw_text: false,
            starting_emitted: false,
            thinking_emitted: false,
            omitted_emitted: false,
        }
    }

    pub fn settings(&self) -> &FallbackSettings {
        &self.settings
    }

    pub fn saw_reasoning(&self) -> bool {
        self.saw_reasoning
    }

    pub fn mark_reasoning(&mut self) {
        self.saw_reasoning = true;
    }

    /// Emit the run-start activity once.
    pub fn on_start(&mut self) -> Option<RunEvent> {
        if !self.settings.emits_activity() || self.starting_emitted {
            return None;
        }
        self.starting_emitted = true;
        self.emit(ActivityKind::Starting, None)
    }

    /// Emit a label when the CLI has been silent for `thinking_delay` and no
    /// reasoning/text has been observed yet.
    pub fn tick(&mut self, now: Instant) -> Option<RunEvent> {
        if self.saw_reasoning || self.saw_text || self.thinking_emitted {
            return None;
        }
        if !self.settings.emits_activity() {
            return None;
        }
        if now.duration_since(self.started_at) < self.settings.thinking_delay {
            return None;
        }
        let event = self.emit(ActivityKind::Thinking, None);
        if event.is_some() {
            self.thinking_emitted = true;
        }
        event
    }

    /// Tool calls observed from the CLI. Only the tool name is included by
    /// default (`activity_args = false`).
    pub fn on_tool_requested(&mut self, name: Option<&str>) -> Option<RunEvent> {
        if !self.settings.emits_activity() {
            return None;
        }
        let tool = if self.settings.include_args { name } else { name };
        self.emit(ActivityKind::ToolRequested, tool)
    }

    /// Run is parked waiting for a client-held tool result.
    pub fn on_waiting_client(&mut self) -> Option<RunEvent> {
        if !self.settings.emits_activity() {
            return None;
        }
        self.emit(ActivityKind::WaitingClient, None)
    }

    /// Omitted thinking block marker.
    pub fn on_omitted(&mut self) -> Option<RunEvent> {
        if self.settings.omitted_marker == OmittedMarker::None || self.omitted_emitted {
            return None;
        }
        self.omitted_emitted = true;
        if !self.settings.emits_activity() {
            return None;
        }

        // Bypass the generic dedupe interval: this is a one-shot marker.
        self.last_emit_at = Some(Instant::now());
        Some(RunEvent::Activity {
            kind: ActivityKind::Thinking,
            label: omitted_label(&self.settings),
        })
    }

    pub fn mark_text(&mut self) {
        self.saw_text = true;
    }

    fn emit(&mut self, kind: ActivityKind, tool: Option<&str>) -> Option<RunEvent> {
        let label = activity_label(&self.settings, kind, tool);
        if self.last_label.as_deref() == Some(label.as_str()) {
            return None;
        }
        let now = Instant::now();
        if let Some(last) = self.last_emit_at {
            if now.duration_since(last) < self.settings.min_interval {
                return None;
            }
        }
        self.last_emit_at = Some(now);
        self.last_label = Some(label.clone());
        Some(RunEvent::Activity { kind, label })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> FallbackSettings {
        FallbackSettings {
            min_interval: Duration::from_millis(10),
            thinking_delay: Duration::from_millis(5),
            ..FallbackSettings::default()
        }
    }

    #[test]
    fn labels_are_prefixed_and_localized() {
        let mut s = settings();
        s.lang = ActivityLang::Id;
        assert!(activity_label(&s, ActivityKind::Thinking, None).starts_with("⏳ "));
        assert!(activity_label(&s, ActivityKind::Thinking, None).contains("Model sedang bekerja"));
        s.lang = ActivityLang::En;
        assert!(activity_label(&s, ActivityKind::ToolRequested, Some("read")).contains("Requesting tool: read"));
    }

    #[test]
    fn dedupes_and_rate_limits() {
        let mut tracker = ActivityTracker::new(settings());
        assert!(tracker.on_start().is_some());
        assert!(tracker.on_start().is_none());
        // Same label is suppressed.
        assert!(tracker.on_waiting_client().is_none());
        std::thread::sleep(Duration::from_millis(12));
        assert!(tracker.on_waiting_client().is_some());
    }

    #[test]
    fn thinking_only_after_quiet_delay_without_reasoning() {
        let mut s = settings();
        s.min_interval = Duration::ZERO;
        let mut tracker = ActivityTracker::new(s);
        let _ = tracker.on_start();
        assert!(tracker.tick(Instant::now()).is_none());
        std::thread::sleep(Duration::from_millis(6));
        assert!(tracker.tick(Instant::now()).is_some());
        tracker.mark_reasoning();
        assert!(tracker.tick(Instant::now()).is_none());
    }

    #[test]
    fn omitted_marker_can_be_disabled() {
        let mut s = settings();
        s.omitted_marker = OmittedMarker::None;
        let mut tracker = ActivityTracker::new(s);
        assert!(tracker.on_omitted().is_none());
    }
}
