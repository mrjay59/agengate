//! RunEvent enum and related types

use serde::{Deserialize, Serialize};

/// Reasoning kind for model output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningKind {
    /// Raw reasoning content as provided by model
    Raw,
    /// Summary of reasoning
    Summary,
    /// Omitted (no reasoning shown)
    Omitted,
    /// Activity-based reasoning
    Activity,
}

impl std::fmt::Display for ReasoningKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReasoningKind::Raw => write!(f, "raw"),
            ReasoningKind::Summary => write!(f, "summary"),
            ReasoningKind::Omitted => write!(f, "omitted"),
            ReasoningKind::Activity => write!(f, "activity"),
        }
    }
}

/// Activity kind for reasoning events
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Starting,
    Thinking,
    ToolRequested,
    WaitingClient,
    Writing,
}

/// Format options for reasoning content output
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningFormat {
    #[default]
    ReasoningContent,
    Reasoning,
    Both,
    ThinkTags,
    None,
    Passthrough,
}

/// Tool call definition
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Batch of tool calls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallsBatch {
    pub calls: Vec<ToolCall>,
}

/// Usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Finish reason
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Cancelled,
}

/// Run error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunError {
    pub message: String,
    pub code: Option<String>,
}

/// Run event types streaming from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    /// Plain text delta
    Text { text: String },
    /// Reasoning started
    ReasoningStart { block: u32, kind: ReasoningKind },
    /// Reasoning delta chunk
    ReasoningDelta { block: u32, text: String },
    /// Reasoning ended
    ReasoningEnd { block: u32 },
    /// Activity event
    Activity { kind: ActivityKind, label: String },
    /// Tool calls batch
    ToolCalls { calls: Vec<ToolCall> },
    /// Usage statistics
    Usage { prompt_tokens: u32, completion_tokens: u32, total_tokens: u32 },
    /// Done marker
    Done { reason: FinishReason },
    /// Error event
    Error { message: String, code: Option<String> },
}

impl RunEvent {
    /// Check if this is a reasoning-related event
    pub fn is_reasoning(&self) -> bool {
        matches!(self, RunEvent::ReasoningStart { .. } | RunEvent::ReasoningDelta { .. } | RunEvent::ReasoningEnd { .. })
    }

    /// Check if this forces a flush
    pub fn forces_flush(&self) -> bool {
        matches!(self, RunEvent::ReasoningEnd { .. } | RunEvent::ToolCalls { .. } | RunEvent::Done { .. } | RunEvent::Error { .. })
    }
}