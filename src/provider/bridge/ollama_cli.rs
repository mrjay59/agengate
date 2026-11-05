//! Ollama CLI bridge provider — V2 adapter implementing spec_ollama-cli.md (RS9).

use crate::bridge::BridgeClient;
use crate::provider::bridge::{
    BridgeConfig, BridgeProfile, BridgeProviderFactory, BridgeProviderV2, BridgeSession,
    BridgeToolResult, SessionOutput,
};
use crate::types::{FunctionCall, ToolCall};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Child;
use tokio::sync::Mutex;

/// Ollama CLI bridge provider (V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaCliBridgeProvider {
    profile: BridgeProfile,
}

impl OllamaCliBridgeProvider {
    pub fn new() -> Self {
        Self {
            profile: BridgeProfile {
                name: "ollama-cli".into(),
                display_name: "Ollama CLI".into(),
                binary: "ollama".into(),
                args: vec!["run".into()],
                env: HashMap::new(),
                mcp_config_template: None,
                tool_schema_format: "openai".into(),
                supports_streaming: true,
                guard_patterns: vec![],
            },
        }
    }
}

impl Default for OllamaCliBridgeProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Output parsing (prompt_tools_fallback)
// ===========================================================================

/// Remove ANSI escape sequences (`ESC [ ... m`) emitted by the Ollama CLI
/// spinner so the JSON payload can be parsed reliably.
pub(crate) fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Strip an optional Markdown code fence around the model output.
fn unfence(text: &str) -> &str {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        // Drop the optional language identifier on the first line.
        let body = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest);
        return body.trim_end_matches("```").trim();
    }
    t
}

/// Locate the first balanced-looking JSON object/array span in `text`.
fn json_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(['{', '['])?;
    let close = if text[start..].starts_with('{') { '}' } else { ']' };
    let end = text.rfind(close)?;
    if end <= start {
        return None;
    }
    Some((start, end + 1))
}

fn normalize_arguments(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    }
}

fn normalize_tool_call(value: &Value) -> Option<ToolCall> {
    let obj = value.as_object()?;

    // OpenAI style: { id, type, function: { name, arguments } }
    if let Some(function) = obj.get("function").and_then(|f| f.as_object()) {
        let name = function.get("name").and_then(|v| v.as_str())?.to_string();
        let arguments = normalize_arguments(function.get("arguments"));
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
        return Some(ToolCall {
            id,
            r#type: "function".to_string(),
            function: FunctionCall { name, arguments },
        });
    }

    // Compact style: { name, arguments } / { name, parameters }
    let name = obj.get("name").and_then(|v| v.as_str())?.to_string();
    let arguments = normalize_arguments(obj.get("arguments").or_else(|| obj.get("parameters")));
    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
    Some(ToolCall {
        id,
        r#type: "function".to_string(),
        function: FunctionCall { name, arguments },
    })
}

fn clean_residual(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Extract tool calls from a model reply.
///
/// Accepts an OpenAI-style `{"tool_calls":[...]}` object, a bare
/// `{"name":...,"arguments":...}` object, or an array of either. Returns the
/// remaining visible text alongside the parsed calls.
pub(crate) fn parse_tool_call_output(raw: &str) -> Option<(String, Vec<ToolCall>)> {
    let text = strip_ansi(raw);
    let trimmed = unfence(&text).trim();
    if trimmed.is_empty() {
        return None;
    }

    // Whole reply is JSON.
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        let calls = tool_calls_from_value(&value);
        if !calls.is_empty() {
            return Some((String::new(), calls));
        }
    }

    // JSON embedded in prose / code fence.
    let (start, end) = json_span(trimmed)?;
    let candidate = &trimmed[start..end];
    let value: Value = serde_json::from_str(candidate).ok()?;
    let calls = tool_calls_from_value(&value);
    if calls.is_empty() {
        return None;
    }
    let mut content = String::new();
    content.push_str(trimmed[..start].trim());
    content.push_str(trimmed[end..].trim());
    Some((clean_residual(&content), calls))
}

fn tool_calls_from_value(value: &Value) -> Vec<ToolCall> {
    match value {
        Value::Array(items) => items.iter().filter_map(normalize_tool_call).collect(),
        Value::Object(obj) => {
            if let Some(Value::Array(items)) = obj
                .get("tool_calls")
                .or_else(|| obj.get("tool_call"))
            {
                return items.iter().filter_map(normalize_tool_call).collect();
            }
            if obj.contains_key("name")
                && (obj.contains_key("arguments") || obj.contains_key("parameters"))
            {
                return normalize_tool_call(value).into_iter().collect();
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Build the `ollama run` argv, injecting `--keepalive` when configured.
pub(crate) fn build_run_args(profile: &BridgeProfile, config: &BridgeConfig) -> Vec<String> {
    let mut args = profile.args.clone();
    if let Some(keep_alive) = config.keep_alive.as_deref() {
        args.push("--keepalive".to_string());
        args.push(keep_alive.to_string());
    }
    args.push(config.model.clone());
    args
}

#[async_trait]
impl BridgeProviderV2 for OllamaCliBridgeProvider {
    fn profile(&self) -> &BridgeProfile {
        &self.profile
    }

    async fn start_bridge(
        &self,
        config: BridgeConfig,
        client: Arc<BridgeClient>,
    ) -> Result<Box<dyn BridgeSession>> {
        Ok(Box::new(OllamaCliSession::new(config, client, self.profile.clone())))
    }

    async fn health_check(&self) -> Result<bool> {
        // Check if ollama binary is available
        Ok(which::which("ollama").is_ok())
    }
}

#[derive(Debug, Clone)]
struct OllamaCliSession {
    config: BridgeConfig,
    #[allow(dead_code)]
    client: Arc<BridgeClient>,
    profile: BridgeProfile,
    session_id: String,
    #[allow(dead_code)]
    process: Arc<Mutex<Option<Child>>>,
    closed: Arc<Mutex<bool>>,
}

impl OllamaCliSession {
    fn new(config: BridgeConfig, client: Arc<BridgeClient>, profile: BridgeProfile) -> Self {
        // Reuse the bridge-registered session id when present (spec §9.2).
        let session_id = config
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Self {
            config,
            client,
            profile,
            session_id,
            process: Arc::new(Mutex::new(None)),
            closed: Arc::new(Mutex::new(false)),
        }
    }

    async fn spawn_process(&self) -> Result<Child> {
        let mut cmd = tokio::process::Command::new(&self.profile.binary);
        cmd.args(build_run_args(&self.profile, &self.config))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .envs(&self.profile.env);

        let child = cmd.spawn()?;
        Ok(child)
    }
}

#[async_trait]
impl BridgeSession for OllamaCliSession {
    fn session_id(&self) -> &String {
        &self.session_id
    }

    fn provider_profile(&self) -> &BridgeProfile {
        &self.profile
    }

    /// Preflight isolation check per spec §13.2 (wired via shared helper).
    async fn preflight_isolation_check(&self) -> Result<bool> {
        crate::provider::bridge::preflight_isolation_check(&self.profile, &self.session_id, "ollama").await
    }

    async fn send_user_message(&self, text: &str) -> Result<()> {
        if *self.closed.lock().await {
            return Err(anyhow::anyhow!("ollama cli session is closed"));
        }
        let mut process_guard = self.process.lock().await;
        if process_guard.is_none() {
            *process_guard = Some(self.spawn_process().await?);
        }

        if let Some(child) = process_guard.as_mut() {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(text.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await?;
            }
        }
        Ok(())
    }

    async fn send_tool_result(&self, result: BridgeToolResult) -> Result<()> {
        let json = serde_json::json!({
            "tool_call_id": result.tool_call_id,
            "result": result.result,
            "is_error": result.is_error,
        });
        self.send_user_message(&json.to_string()).await
    }

    async fn close(&self) -> Result<()> {
        *self.closed.lock().await = true;
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            let _ = child.kill().await;
        }
        Ok(())
    }

    async fn wait_for_completion(&self) -> Result<SessionOutput> {
        let mut process_guard = self.process.lock().await;

        let mut child = match process_guard.take() {
            Some(child) => child,
            None => return Ok(SessionOutput::default()),
        };

        // Close stdin so a non-interactive `ollama run` exits after answering.
        drop(child.stdin.take());

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (stdout_res, stderr_res) = tokio::join!(
            async move {
                let mut buf = String::new();
                if let Some(mut out) = stdout {
                    out.read_to_string(&mut buf).await?;
                }
                Ok::<_, std::io::Error>(buf)
            },
            async move {
                let mut buf = String::new();
                if let Some(mut err) = stderr {
                    let _ = err.read_to_string(&mut buf).await;
                }
                buf
            }
        );
        let raw = stdout_res?;
        let _ = child.wait().await;

        let raw = strip_ansi(&raw);
        let raw = raw.trim();

        if self.config.prompt_tools_fallback {
            if let Some((content, tool_calls)) = parse_tool_call_output(raw) {
                return Ok(SessionOutput {
                    content,
                    reasoning: None,
                    tool_calls: Some(tool_calls),
                });
            }
        }

        if raw.is_empty() {
            let err = stderr_res.trim();
            if !err.is_empty() {
                return Err(anyhow::anyhow!("ollama cli failed: {}", err));
            }
        }

        Ok(SessionOutput {
            content: raw.to_string(),
            reasoning: None,
            tool_calls: None,
        })
    }
}

#[derive(Debug, Default)]
pub struct OllamaCliBridgeFactory;

#[async_trait]
impl BridgeProviderFactory for OllamaCliBridgeFactory {
    fn provider_name(&self) -> &str {
        "ollama-cli"
    }

    async fn create(&self, _config: BridgeConfig) -> Result<Box<dyn BridgeProviderV2>> {
        Ok(Box::new(OllamaCliBridgeProvider::new()))
    }

    fn default_profile(&self) -> crate::provider::bridge::BridgeProfileSpec {
        crate::provider::bridge::BridgeProfileSpec {
            name: "ollama-cli".into(),
            display_name: "Ollama CLI".into(),
            binary: "ollama".into(),
            args: vec!["run".into()],
            env: HashMap::new(),
            mcp_config_template: None,
            tool_schema_format: "openai".into(),
            supports_streaming: true,
            guard_patterns: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_name() {
        let f = OllamaCliBridgeFactory;
        assert_eq!(f.provider_name(), "ollama-cli");
    }

    #[tokio::test]
    async fn health_check_when_not_installed() {
        let provider = OllamaCliBridgeProvider::new();
        // Should not panic even if ollama is not installed
        let _ = provider.health_check().await;
    }

    fn profile() -> BridgeProfile {
        OllamaCliBridgeProvider::new().profile().clone()
    }

    #[test]
    fn run_args_inject_keepalive_before_model() {
        let config = BridgeConfig {
            model: "llama3.2".into(),
            keep_alive: Some("10m".into()),
            ..Default::default()
        };
        let args = build_run_args(&profile(), &config);
        assert_eq!(args, vec!["run", "--keepalive", "10m", "llama3.2"]);
    }

    #[test]
    fn run_args_without_keepalive() {
        let config = BridgeConfig {
            model: "qwen2.5".into(),
            keep_alive: None,
            ..Default::default()
        };
        assert_eq!(build_run_args(&profile(), &config), vec!["run", "qwen2.5"]);
    }

    #[test]
    fn parses_openai_style_tool_calls() {
        let raw = r#"{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"a.txt\"}"}}]}"#;
        let (content, calls) = parse_tool_call_output(raw).expect("tool calls");
        assert!(content.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "write_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn parses_compact_and_fenced_tool_calls() {
        let raw = "Saya akan menulis file.\n```json\n{\"tool_calls\":[{\"name\":\"write_file\",\"arguments\":{\"path\":\"a.txt\"}}]}\n```";
        let (content, calls) = parse_tool_call_output(raw).expect("tool calls");
        assert_eq!(content, "Saya akan menulis file.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        assert!(calls[0].id.starts_with("call_"));
        assert_eq!(calls[0].function.arguments, r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn parses_bare_tool_call_object() {
        let raw = r#"{"name":"read_file","parameters":{"path":"b.txt"}}"#;
        let (_, calls) = parse_tool_call_output(raw).expect("tool call");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"b.txt"}"#);
    }

    #[test]
    fn plain_text_is_not_a_tool_call() {
        assert!(parse_tool_call_output("Halo, apa kabar?").is_none());
        assert!(parse_tool_call_output(r#"{"status":"ok"}"#).is_none());
    }

    #[test]
    fn strips_ansi_sequences() {
        assert_eq!(strip_ansi("\u{1b}[1mhello\u{1b}[0m"), "hello");
    }

    #[test]
    fn session_reuses_configured_session_id() {
        let client = Arc::new(BridgeClient {
            host: "127.0.0.1".into(),
            port: 1,
        });
        let config = BridgeConfig {
            session_id: Some("session-123".into()),
            ..Default::default()
        };
        let session = OllamaCliSession::new(config, client, profile());
        assert_eq!(session.session_id(), "session-123");
    }
}