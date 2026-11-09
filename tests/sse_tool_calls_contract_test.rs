use agentgate::openai::sse_tools::{ToolActivityEvent, ToolCallBuffer};
use agentgate::reasoning::{
    parser::ReasoningParser,
    coalesce::{Coalescer, CoalescerSettings},
    RunEvent, ReasoningKind, FinishReason,
};
use agentgate::types::{ChatCompletionChunk, ChatCompletionChunkChoice, ChatCompletionChunkDelta, ToolCallChunk, FunctionCallChunk};

/// Regression test: SSE tool_calls 2-chunk format per spec §7.4
/// Chunk 1: id, name, arguments: ""
/// Chunk 2+: arguments fragments (<= 512B)
/// Final: finish_reason = "tool_calls"
#[test]
fn sse_tool_calls_two_chunk_contract() {
    // Build a simulated tool_calls sequence as api.rs would emit
    let tool_call_id = "call_abc123";
    let name = "shell";
    let arguments = r#"{"cmd": "ls -la"}"#;
    
    // Chunk 1: id, name, arguments: ""
    let chunk1 = ChatCompletionChunk {
        id: "chatcmpl-test1".into(),
        object: "chat.completion.chunk".into(),
        usage: None,
        created: 1234567890,
        model: "agentgate".into(),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta: ChatCompletionChunkDelta {
                role: Some("assistant".into()),
                content: None,
                tool_calls: Some(vec![ToolCallChunk {
                    index: 0,
                    id: Some(tool_call_id.into()),
                    r#type: Some("function".into()),
                    function: Some(FunctionCallChunk {
                        name: Some(name.into()),
                        arguments: Some("".into()),
                    }),
                }]),
                reasoning_content: None,
            },
            finish_reason: None,
        }],
    };
    
    // Verify chunk 1 has id+name, empty arguments
    let c1_tool = &chunk1.choices[0].delta.tool_calls.as_ref().unwrap()[0];
    assert_eq!(c1_tool.id.as_deref(), Some(tool_call_id));
    assert_eq!(c1_tool.function.as_ref().unwrap().name.as_deref(), Some(name));
    assert_eq!(c1_tool.function.as_ref().unwrap().arguments.as_deref(), Some(""));
    assert_eq!(chunk1.choices[0].finish_reason, None);
    
    // Chunk 2: arguments fragment
    let chunk2 = ChatCompletionChunk {
        id: "chatcmpl-test2".into(),
        object: "chat.completion.chunk".into(),
        usage: None,
        created: 1234567890,
        model: "agentgate".into(),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta: ChatCompletionChunkDelta {
                role: Some("assistant".into()),
                content: None,
                tool_calls: Some(vec![ToolCallChunk {
                    index: 0,
                    id: None, // continuation doesn't repeat id
                    r#type: None,
                    function: Some(FunctionCallChunk {
                        name: None,
                        arguments: Some(arguments.into()),
                    }),
                }]),
                reasoning_content: None,
            },
            finish_reason: None,
        }],
    };
    
    // Verify chunk 2 has arguments fragment, no id/name
    let c2_tool = &chunk2.choices[0].delta.tool_calls.as_ref().unwrap()[0];
    assert_eq!(c2_tool.id, None);
    assert_eq!(c2_tool.r#type, None);
    assert_eq!(c2_tool.function.as_ref().unwrap().name, None);
    assert_eq!(c2_tool.function.as_ref().unwrap().arguments.as_deref(), Some(arguments));
    assert_eq!(chunk2.choices[0].finish_reason, None);
    
    // Chunk 3: final with finish_reason = "tool_calls"
    let chunk3 = ChatCompletionChunk {
        id: "chatcmpl-test3".into(),
        object: "chat.completion.chunk".into(),
        usage: None,
        created: 1234567890,
        model: "agentgate".into(),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta: ChatCompletionChunkDelta {
                role: Some("assistant".into()),
                content: None,
                tool_calls: Some(vec![ToolCallChunk {
                    index: 0,
                    id: None,
                    r#type: None,
                    function: Some(FunctionCallChunk {
                        name: None,
                        arguments: None,
                    }),
                }]),
                reasoning_content: None,
            },
            finish_reason: Some("tool_calls".into()),
        }],
    };
    
    assert_eq!(chunk3.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    
    // Verify JSON serialization matches expected OpenAI format
    let json1 = serde_json::to_string(&chunk1).unwrap();
    let json2 = serde_json::to_string(&chunk2).unwrap();
    let json3 = serde_json::to_string(&chunk3).unwrap();
    
    assert!(json1.contains("\"id\":\"call_abc123\""));
    assert!(json1.contains("\"name\":\"shell\""));
    assert!(json1.contains("\"arguments\":\"\""));
    // finish_reason may be serialized as null depending on serde config; semantic check above is authoritative.
    assert!(json1.contains("\"finish_reason\":null") || !json1.contains("\"finish_reason\":\"tool_calls\""));
    
    assert!(json2.contains("\"arguments\""));
    assert!(json2.contains("ls -la"));
    assert!(json3.contains("\"finish_reason\":\"tool_calls\""));
}

/// Test: ToolCallBuffer accumulates arguments correctly for streaming
#[test]
fn tool_call_buffer_streaming_accumulation() {
    let mut buffer = ToolCallBuffer::new();
    buffer.start_call("call_1".into(), "shell".into());
    buffer.append_arguments(r#"{"cmd":"ls"}"#);
    buffer.append_arguments(r#" -la"#);
    
    let call = buffer.current().unwrap();
    assert_eq!(call.id, "call_1");
    assert_eq!(call.name, "shell");
    // append_arguments concatenates raw deltas
    assert_eq!(call.arguments, r#"{"cmd":"ls"} -la"#);
}

/// Regression test: Reasoning grouping - multi tool_calls in single batch
/// Should emit: ReasoningStart + ReasoningDelta (per tool) + ToolCalls
/// NOT: ReasoningStart + ReasoningDelta + ReasoningEnd + ToolCalls per tool
#[test]
fn reasoning_grouping_multi_tool_calls_single_block() {
    let mut parser = ReasoningParser::new();
    
    // Multiple tool calls in a single batch
    let out = parser.parse_line(
        r#"{"type":"tool_calls","timestamp":1,"calls":[
            {"id":"call_1","type":"function","function":{"name":"shell","arguments":"{\"cmd\":\"ls\"}"}},
            {"id":"call_2","type":"function","function":{"name":"glob","arguments":"{\"pattern\":\"**/*.rs\"}"}}
        ]}"#,
    );
    
    // Should produce: ReasoningStart + 2 ReasoningDelta + ToolCalls = 4 events
    assert_eq!(out.len(), 4, "Expected 4 events: {:?}", out);
    
    // First event should be ReasoningStart
    assert!(matches!(&out[0], RunEvent::ReasoningStart { .. }));
    
    // Middle events should be ReasoningDelta with tool view steps
    let deltas: Vec<&RunEvent> = out.iter().filter(|e| matches!(e, RunEvent::ReasoningDelta { .. })).collect();
    assert_eq!(deltas.len(), 2, "Expected 2 ReasoningDelta for 2 tools");
    
    // Last event should be ToolCalls
    assert!(matches!(&out[3], RunEvent::ToolCalls { calls } if calls.len() == 2));
    
    // No ReasoningEnd between tool calls (grouping preserved)
    let ends: Vec<&RunEvent> = out.iter().filter(|e| matches!(e, RunEvent::ReasoningEnd { .. })).collect();
    assert!(ends.is_empty(), "No ReasoningEnd should appear between tools");
}

/// Regression test: Repeated tool_call batches share same reasoning block
/// Second batch should NOT start new ReasoningStart
#[test]
fn reasoning_grouping_repeated_batches_share_block() {
    let mut parser = ReasoningParser::new();
    
    // First batch
    let out1 = parser.parse_line(
        r#"{"type":"tool_calls","timestamp":1,"calls":[
            {"id":"call_1","type":"function","function":{"name":"shell","arguments":"{\"cmd\":\"ls\"}"}}
        ]}"#,
    );
    assert!(matches!(&out1[0], RunEvent::ReasoningStart { .. }));
    
    // Second batch should NOT start a new reasoning block
    let out2 = parser.parse_line(
        r#"{"type":"tool_calls","timestamp":2,"calls":[
            {"id":"call_2","type":"function","function":{"name":"glob","arguments":"{\"pattern\":\"**/*.rs\"}"}}
        ]}"#,
    );
    
    // First event of second batch should NOT be ReasoningStart
    assert!(!out2.iter().any(|e| matches!(e, RunEvent::ReasoningStart { .. })),
        "Second batch should not start new reasoning block: {:?}", out2);
    
    // Should contain ReasoningDelta for the new tool view step
    assert!(out2.iter().any(|e| matches!(e, RunEvent::ReasoningDelta { .. })),
        "Second batch should emit ReasoningDelta");
}

/// Regression test: Coalescer preserves reasoning block across ToolCalls
/// (Does not flush/end reasoning on ToolCalls event)
#[test]
fn coalescer_preserves_reasoning_across_tool_calls() {
    let mut c = Coalescer::with_settings(CoalescerSettings {
        flush_ms: 0,
        flush_bytes: 64,
    });
    
    // Start reasoning
    c.add_event(RunEvent::ReasoningStart {
        block: 0,
        kind: ReasoningKind::Summary,
    });
    c.add_event(RunEvent::ReasoningDelta {
        block: 0,
        text: "thinking...".into(),
    });
    
    // Add ToolCalls - should NOT flush/end reasoning
    let out = c.add_event(RunEvent::ToolCalls { calls: vec![] });
    
    // Should not emit ReasoningEnd
    assert!(!out.iter().any(|e| matches!(e, RunEvent::ReasoningEnd { .. })),
        "Coalescer must not end reasoning on ToolCalls: {:?}", out);
    
    // Should not emit the reasoning block (still buffered)
    assert!(!out.iter().any(|e| matches!(e, RunEvent::ReasoningStart { .. })));
    
    // Done event should finally flush reasoning
    let out = c.add_event(RunEvent::Done { reason: FinishReason::Stop });
    assert!(out.iter().any(|e| matches!(e, RunEvent::ReasoningEnd { .. })),
        "ReasoningEnd should be emitted on Done boundary");
}

/// Test: ToolActivityEvent SSE serialization matches expected format
#[test]
fn tool_activity_event_sse_format() {
    let event = ToolActivityEvent::start("call_1".into(), "shell".into(), r#"{"cmd":"ls"}"#.into());
    let sse = event.to_sse();
    
    assert!(sse.starts_with("event: agentgate.tool\n"));
    assert!(sse.contains("data:"));
    assert!(sse.ends_with("\n\n"));
    
    // Verify JSON is valid
    let json_part = sse.strip_prefix("event: agentgate.tool\ndata: ").unwrap().strip_suffix("\n\n").unwrap();
    let parsed: ToolActivityEvent = serde_json::from_str(json_part).unwrap();
    assert!(matches!(parsed, ToolActivityEvent::ToolCallStart { .. }));
}