use crate::bridge::ToolResult;
use crate::types::{ChatCompletionChunk, ChatCompletionChunkChoice, ChatCompletionChunkDelta};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// SSE event for tool activity (agentgate extension)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum ToolActivityEvent {
    /// Tool call started
    ToolCallStart {
        tool_call_id: String,
        name: String,
        arguments: String,
    },
    /// Tool call arguments delta (streaming)
    ToolCallDelta {
        tool_call_id: String,
        arguments_delta: String,
    },
    /// Tool call completed
    ToolCallEnd {
        tool_call_id: String,
        name: String,
        result: serde_json::Value,
        is_error: bool,
    },
    /// Tool call failed
    ToolCallError {
        tool_call_id: String,
        name: String,
        error: String,
    },
}

impl ToolActivityEvent {
    /// Convert to SSE formatted string
    pub fn to_sse(&self) -> String {
        let data = serde_json::to_string(self).unwrap_or_default();
        format!("event: agentgate.tool\ndata: {}\n\n", data)
    }

    /// Create start event
    pub fn start(tool_call_id: String, name: String, arguments: String) -> Self {
        Self::ToolCallStart {
            tool_call_id,
            name,
            arguments,
        }
    }

    /// Create delta event
    pub fn delta(tool_call_id: String, arguments_delta: String) -> Self {
        Self::ToolCallDelta {
            tool_call_id,
            arguments_delta,
        }
    }

    /// Create end event
    pub fn end(tool_call_id: String, name: String, result: serde_json::Value, is_error: bool) -> Self {
        Self::ToolCallEnd {
            tool_call_id,
            name,
            result,
            is_error,
        }
    }

    /// Create error event
    pub fn error(tool_call_id: String, name: String, error: String) -> Self {
        Self::ToolCallError {
            tool_call_id,
            name,
            error,
        }
    }
}

/// Tool call accumulator for streaming
#[derive(Debug, Clone)]
pub struct StreamingToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub index: usize,
}

impl StreamingToolCall {
    pub fn new(id: String, name: String, index: usize) -> Self {
        Self {
            id,
            name,
            arguments: String::new(),
            index,
        }
    }

    pub fn append_arguments(&mut self, delta: &str) {
        self.arguments.push_str(delta);
    }
}

/// Buffer for accumulating streaming tool calls
pub struct ToolCallBuffer {
    calls: VecDeque<StreamingToolCall>,
    current_index: usize,
}

impl ToolCallBuffer {
    pub fn new() -> Self {
        Self {
            calls: VecDeque::new(),
            current_index: 0,
        }
    }

    /// Start a new tool call
    pub fn start_call(&mut self, id: String, name: String) -> &mut StreamingToolCall {
        let call = StreamingToolCall::new(id, name, self.current_index);
        self.calls.push_back(call);
        self.current_index += 1;
        self.calls.back_mut().unwrap()
    }

    /// Append arguments to current call
    pub fn append_arguments(&mut self, delta: &str) {
        if let Some(call) = self.calls.back_mut() {
            call.append_arguments(delta);
        }
    }

    /// Complete current call and get result
    pub fn complete_call(&mut self, result: ToolResult) -> Option<(StreamingToolCall, ToolResult)> {
        self.calls.pop_front().map(|call| (call, result))
    }

    /// Get current call
    pub fn current(&mut self) -> Option<&mut StreamingToolCall> {
        self.calls.back_mut()
    }

    /// Check if buffer has pending calls
    pub fn has_pending(&self) -> bool {
        !self.calls.is_empty()
    }

    /// Clear buffer
    pub fn clear(&mut self) {
        self.calls.clear();
    }
}

impl Default for ToolCallBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert ToolResult to ChatCompletionChunk for tool result
pub fn tool_result_to_chunk(result: &ToolResult) -> ChatCompletionChunk {
    ChatCompletionChunk {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        object: "chat.completion.chunk".to_string(),
        usage: None,        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        model: "agentgate".to_string(),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta: ChatCompletionChunkDelta {
                role: Some("tool".to_string()),
                content: Some(result.content.to_string()),
                tool_calls: None,
                reasoning_content: None,
            },
            finish_reason: Some("tool_result".to_string()),
        }],
    }
}

/// Convert ToolActivityEvent to ChatCompletionChunk (for inline display)
pub fn tool_event_to_inline_chunk(event: &ToolActivityEvent) -> Option<ChatCompletionChunk> {
    match event {
        ToolActivityEvent::ToolCallStart { tool_call_id, name, arguments } => {
            Some(ChatCompletionChunk {
                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                object: "chat.completion.chunk".to_string(),
        usage: None,                created: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                model: "agentgate".to_string(),
                choices: vec![ChatCompletionChunkChoice {
                    index: 0,
                    delta: ChatCompletionChunkDelta {
                        role: Some("assistant".to_string()),
                        content: Some(format!("\n[Tool: {} started]\n", name)),
                        tool_calls: Some(vec![crate::types::ToolCallChunk {
                            index: 0,
                            id: Some(tool_call_id.clone()),
                            r#type: Some("function".to_string()),
                            function: Some(crate::types::FunctionCallChunk {
                                name: Some(name.clone()),
                                arguments: Some(arguments.clone()),
                            }),
                        }]),
                        reasoning_content: None,
                    },
                    finish_reason: None,
                }],
            })
        }
        ToolActivityEvent::ToolCallEnd { tool_call_id: _, name, result: _, is_error } => {
            let status = if *is_error { "error" } else { "ok" };
            Some(ChatCompletionChunk {
                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                object: "chat.completion.chunk".to_string(),
        usage: None,                created: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                model: "agentgate".to_string(),
                choices: vec![ChatCompletionChunkChoice {
                    index: 0,
                    delta: ChatCompletionChunkDelta {
                        role: Some("assistant".to_string()),
                        content: Some(format!("\n[Tool: {} completed: {}]\n", name, status)),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                }],
            })
        }
        ToolActivityEvent::ToolCallError { name, error, .. } => {
            Some(ChatCompletionChunk {
                id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                object: "chat.completion.chunk".to_string(),
        usage: None,                created: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                model: "agentgate".to_string(),
                choices: vec![ChatCompletionChunkChoice {
                    index: 0,
                    delta: ChatCompletionChunkDelta {
                        role: Some("assistant".to_string()),
                        content: Some(format!("\n[Tool: {} failed: {}]\n", name, error)),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                }],
            })
        }
        ToolActivityEvent::ToolCallDelta { .. } => None,
    }
}

/// SSE event writer for tool activity
pub struct ToolEventWriter {
    buffer: VecDeque<ToolActivityEvent>,
}

impl ToolEventWriter {
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::new(),
        }
    }

    pub fn push(&mut self, event: ToolActivityEvent) {
        self.buffer.push_back(event);
    }

    pub fn pop(&mut self) -> Option<ToolActivityEvent> {
        self.buffer.pop_front()
    }

    pub fn drain(&mut self) -> Vec<ToolActivityEvent> {
        self.buffer.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

impl Default for ToolEventWriter {
    fn default() -> Self {
        Self::new()
    }
}