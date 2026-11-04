use crate::bridge::ToolSchema;
use crate::permission::ClientToolsPolicy;
use crate::provider::ProviderResponse;
use crate::types::{ChatCompletionRequest, Message, MessageRole, ToolCall, FunctionCall};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;

/// Tool choice handling per OpenAI spec
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    None,
    Auto,
    Required,
    Function { name: String },
}

impl Default for ToolChoice {
    fn default() -> Self {
        ToolChoice::Auto
    }
}

impl ToolChoice {
    /// Parse from request tool_choice field
    pub fn from_value(value: Option<serde_json::Value>) -> Result<Self> {
        match value {
            None => Ok(ToolChoice::Auto),
            Some(v) => {
                if v.is_string() {
                    match v.as_str() {
                        Some("none") => Ok(ToolChoice::None),
                        Some("auto") => Ok(ToolChoice::Auto),
                        Some("required") => Ok(ToolChoice::Required),
                        Some(s) => Ok(ToolChoice::Function { name: s.to_string() }),
                        None => Ok(ToolChoice::Auto),
                    }
                } else if v.is_object() {
                    // {"type": "function", "function": {"name": "..."}}
                    if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
                        if t == "function" {
                            if let Some(name) = v.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()) {
                                return Ok(ToolChoice::Function { name: name.to_string() });
                            }
                        }
                    }
                    Ok(ToolChoice::Auto)
                } else {
                    Ok(ToolChoice::Auto)
                }
            }
        }
    }

    /// Check if tool calling is allowed
    pub fn allows_tools(&self) -> bool {
        !matches!(self, ToolChoice::None)
    }

    /// Check if a specific function is required
    pub fn requires_function(&self, name: &str) -> bool {
        matches!(self, ToolChoice::Function { name: n } if n == name)
    }
}

/// Validate tool schema against OpenAI function calling format
pub fn validate_tool_schema(schema: &ToolSchema) -> Result<()> {
    // Check name
    if schema.name.is_empty() {
        return Err(anyhow!("Tool name cannot be empty"));
    }
    if schema.name.len() > 64 {
        return Err(anyhow!("Tool name exceeds 64 characters"));
    }
    if !schema.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(anyhow!("Tool name contains invalid characters"));
    }

    // Check parameters is valid JSON schema
    if !schema.parameters.is_object() {
        return Err(anyhow!("Tool parameters must be an object"));
    }

    // Check required fields
    if let Some(props) = schema.parameters.get("properties") {
        if !props.is_object() {
            return Err(anyhow!("Tool parameters.properties must be an object"));
        }
    }

    Ok(())
}

/// Convert agentgate ToolSchema to OpenAI function format (for tools array)
pub fn tool_schema_to_openai(schema: &ToolSchema) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": schema.name,
            "description": schema.description,
            "parameters": schema.parameters
        }
    })
}

/// Convert ToolSchema to ToolDefinition for OpenAI API
pub fn tool_schema_to_tool_definition(schema: &ToolSchema) -> crate::types::ToolDefinition {
    crate::types::ToolDefinition {
        r#type: "function".to_string(),
        function: crate::types::FunctionDefinition {
            name: schema.name.clone(),
            description: Some(schema.description.clone()),
            parameters: Some(schema.parameters.clone()),
        },
    }
}

/// Convert Vec<ToolSchema> to Vec<ToolDefinition>
pub fn tool_schemas_to_tool_definitions(schemas: &[ToolSchema]) -> Vec<crate::types::ToolDefinition> {
    schemas.iter().map(tool_schema_to_tool_definition).collect()
}

/// Process request tools based on client tools policy
pub fn process_request_tools(
    request: &mut ChatCompletionRequest,
    agentgate_tools: Vec<ToolSchema>,
    policy: ClientToolsPolicy,
) {
    match policy {
        ClientToolsPolicy::Bridge => {
            // Replace with agentgate tools
            request.tools = Some(tool_schemas_to_tool_definitions(&agentgate_tools));
        }
        ClientToolsPolicy::Passthrough => {
            // Keep client tools if provided, otherwise use agentgate
            if request.tools.is_none() {
                request.tools = Some(tool_schemas_to_tool_definitions(&agentgate_tools));
            }
        }
        ClientToolsPolicy::Ignore => {
            // Always use agentgate tools
            request.tools = Some(tool_schemas_to_tool_definitions(&agentgate_tools));
        }
    }

    // Validate tool_choice if tools are present
    if let Some(ref tools) = request.tools {
        if !tools.is_empty() {
            request.tool_choice = Some(serde_json::json!({ "type": "function", "function": { "name": "auto" }}));
        }
    }
}

/// Extract tool calls from assistant message content
/// Supports both native tool_calls and text-based formats
pub fn extract_tool_calls(message: &Message) -> Vec<ToolCall> {
    if let Some(ref tool_calls) = message.tool_calls {
        return tool_calls.clone();
    }

    // Try to parse from content (for models that output JSON in text)
    if let Some(content) = message.content.as_deref().filter(|c| !c.is_empty()) {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(content) {
            if let Some(arr) = parsed.as_array() {
                return arr.iter().filter_map(|v| {
                    if let (Some(name), Some(args)) = (
                        v.get("name").and_then(|n| n.as_str()),
                        v.get("arguments"),
                    ) {
                        Some(ToolCall {
                            id: TOOL_CALL_ID_GENERATOR.lock().unwrap().generate(),
                            r#type: "function".to_string(),
                            function: FunctionCall {
                                name: name.to_string(),
                                arguments: args.to_string(),
                            },
                        })
                    } else {
                        None
                    }
                }).collect();
            }
        }
    }

    vec![]
}

/// Build tool result message for OpenAI format
pub fn build_tool_result_message(
    tool_call_id: String,
    name: String,
    content: String,
) -> Message {
    Message {
        role: MessageRole::Tool,
        content: Some(content),
        tool_call_id: Some(tool_call_id),
        name: Some(name),
        tool_calls: None,
        reasoning_content: None,
    }
}

/// Apply tool_choice constraint to available tools
pub fn filter_tools_by_choice(
    tools: Vec<ToolSchema>,
    choice: &ToolChoice,
) -> Vec<ToolSchema> {
    match choice {
        ToolChoice::None => vec![],
        ToolChoice::Function { name } => {
            tools.into_iter().filter(|t| t.name == *name).collect()
        }
        ToolChoice::Auto | ToolChoice::Required => tools,
    }
}

/// Extract tool calls from ProviderResponse (stub)
pub fn extract_tool_calls_from_provider_response(_resp: &ProviderResponse) -> Vec<ToolCall> {
    vec![]
}

/// Generator for unique tool_call_id
/// Format: call_ + 16 hex (8 hex run_id + 2 batch + 2 index + 4 random)
pub struct ToolCallIdGenerator {
    run_id: u64,
    batch: u8,
    index: u8,
    #[allow(dead_code)]
    counter: AtomicU64,
    prefix: String,
}

impl ToolCallIdGenerator {
    pub fn new(run_id: u64, prefix: impl Into<String>) -> Self {
        Self {
            run_id,
            batch: 0,
            index: 0,
            counter: AtomicU64::new(0),
            prefix: prefix.into(),
        }
    }

    pub fn new_batch(run_id: u64, batch: u8, prefix: impl Into<String>) -> Self {
        Self {
            run_id,
            batch,
            index: 0,
            counter: AtomicU64::new(0),
            prefix: prefix.into(),
        }
    }

    pub fn generate(&mut self) -> String {
        let index = self.index;
        self.index = self.index.wrapping_add(1);

        // 8 hex run_id + 2 hex batch + 2 hex index + 4 hex random
        let random_suffix: u16 = (self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) as u16;
        format!("{}_{:08x}{:02x}{:02x}{:04x}", self.prefix, self.run_id & 0xFFFFFFFF, self.batch, index, random_suffix)
    }

    /// Generate batch of IDs
    pub fn generate_batch(&mut self, n: usize) -> Vec<String> {
        (0..n).map(|_| self.generate()).collect()
    }
}

impl Default for ToolCallIdGenerator {
    fn default() -> Self {
        Self::new(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0),
            "call",
        )
    }
}

/// Thread-safe global generator (wrapped in Mutex for mutable generate)
use std::sync::Mutex;
pub static TOOL_CALL_ID_GENERATOR: std::sync::LazyLock<Mutex<ToolCallIdGenerator>> =
    std::sync::LazyLock::new(|| Mutex::new(ToolCallIdGenerator::default()));

/// NameMap for tool name aliasing per spec
/// budget = max_len - prefix_len, alias format: t{NN}_{sanitized}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NameMap {
    /// Mapping from original tool name to alias
    pub aliases: HashMap<String, String>,
    /// Reverse mapping from alias to original name
    pub reverse: HashMap<String, String>,
    /// Next available index for t{NN} format
    next_index: usize,
    /// Maximum length for OpenAI (64 chars)
    max_len: usize,
    /// Prefix for alias (e.g., "t")
    prefix: String,
}

impl NameMap {
    pub fn new(max_len: usize, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        let _budget = max_len.saturating_sub(prefix.len() + 3); // "tNN_" prefix
        Self {
            aliases: HashMap::new(),
            reverse: HashMap::new(),
            next_index: 0,
            max_len,
            prefix,
        }
    }

    /// Get or create alias for a tool name
    pub fn get_or_create(&mut self, name: &str) -> String {
        if let Some(alias) = self.aliases.get(name) {
            return alias.clone();
        }

        let sanitized = crate::bridge::sanitize_tool_name(name);
        let alias = format!("{}{:02}_{}", self.prefix, self.next_index, sanitized);

        // Ensure within max_len budget
        let alias = if alias.len() > self.max_len {
            let budget = self.max_len.saturating_sub(self.prefix.len() + 3);
            format!("{}{:02}_{}", self.prefix, self.next_index, &sanitized[..budget.min(sanitized.len())])
        } else {
            alias
        };

        self.aliases.insert(name.to_string(), alias.clone());
        self.reverse.insert(alias.clone(), name.to_string());
        self.next_index += 1;
        alias
    }

    /// Get original name from alias
    pub fn resolve(&self, alias: &str) -> Option<&str> {
        self.reverse.get(alias).map(|s| s.as_str())
    }

    /// Get all aliases
    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.aliases
    }

    /// Build tool definitions with aliases applied
    pub fn apply_to_tools(&self, tools: &[ToolSchema]) -> Vec<serde_json::Value> {
        tools.iter().map(|s| {
            let alias = self.aliases.get(&s.name).cloned().unwrap_or_else(|| s.name.clone());
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": alias,
                    "description": s.description,
                    "parameters": s.parameters
                }
            })
        }).collect()
    }

    /// Build reverse map for tool call resolution
    pub fn build_reverse_map(&self) -> HashMap<String, String> {
        self.reverse.clone()
    }
}

/// Schema normalization profile
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemaProfile {
    /// Permissive: allow additionalProperties, no strict validation
    Permissive,
    /// Strict Gemini: additionalProperties: false, strict required fields
    StrictGemini,
    /// Default OpenAI: additionalProperties: false
    OpenAI,
}

impl Default for SchemaProfile {
    fn default() -> Self {
        SchemaProfile::OpenAI
    }
}

/// Normalize tool schema according to profile
pub fn normalize_schema(schema: &ToolSchema, profile: SchemaProfile) -> ToolSchema {
    let mut normalized = schema.clone();
    // Spec §8.2: root inputSchema must be {"type":"object"}
    if !normalized.parameters.is_object() {
        normalized.parameters = serde_json::json!({"type":"object","properties":{}});
    } else {
        if normalized.parameters.get("type").and_then(|v| v.as_str()) != Some("object") {
            normalized.parameters["type"] = serde_json::json!("object");
        }
        if normalized.parameters.get("properties").is_none() {
            normalized.parameters["properties"] = serde_json::json!({});
        }
        // remove $schema
        if let Some(obj) = normalized.parameters.as_object_mut() {
            obj.remove("$schema");
        }
    }

    match profile {
        SchemaProfile::Permissive => {
            // Keep as-is, add additionalProperties: true if object
            if let Some(props) = normalized.parameters.get_mut("properties") {
                if let Some(obj) = props.as_object_mut() {
                    for (_, prop) in obj.iter_mut() {
                        if let Some(prop_obj) = prop.as_object_mut() {
                            prop_obj.remove("additionalProperties");
                            // Add permissive additionalProperties
                            prop_obj.insert("additionalProperties".to_string(), serde_json::json!(true));
                        }
                    }
                }
            }
        }
        SchemaProfile::StrictGemini => {
            // Enforce additionalProperties: false and required fields
            if let Some(props) = normalized.parameters.get_mut("properties") {
                if let Some(obj) = props.as_object_mut() {
                    for (_, prop) in obj.iter_mut() {
                        if let Some(prop_obj) = prop.as_object_mut() {
                            prop_obj.insert("additionalProperties".to_string(), serde_json::json!(false));
                        }
                    }
                }
            }
            // Ensure required array exists
            if normalized.parameters.get("required").is_none() {
                if let Some(props) = normalized.parameters.get("properties") {
                    if let Some(obj) = props.as_object() {
                        let required: Vec<String> = obj.keys().cloned().collect();
                        normalized.parameters["required"] = serde_json::json!(required);
                    }
                }
            }
        }
        SchemaProfile::OpenAI => {
            // Standard OpenAI: additionalProperties: false
            if let Some(props) = normalized.parameters.get_mut("properties") {
                if let Some(obj) = props.as_object_mut() {
                    for (_, prop) in obj.iter_mut() {
                        if let Some(prop_obj) = prop.as_object_mut() {
                            prop_obj.insert("additionalProperties".to_string(), serde_json::json!(false));
                        }
                    }
                }
            }
        }
    }

    normalized
}