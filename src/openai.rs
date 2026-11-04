pub mod tools;
pub mod sse_tools;

pub use tools::{
    ToolChoice, ToolCallIdGenerator, TOOL_CALL_ID_GENERATOR,
    validate_tool_schema, tool_schema_to_openai, tool_schema_to_tool_definition, tool_schemas_to_tool_definitions,
    process_request_tools, extract_tool_calls, extract_tool_calls_from_provider_response,
    build_tool_result_message, filter_tools_by_choice,
};
pub use sse_tools::{
    ToolActivityEvent, StreamingToolCall, ToolCallBuffer,
    tool_result_to_chunk, tool_event_to_inline_chunk, ToolEventWriter,
};