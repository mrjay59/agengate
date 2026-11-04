use crate::config::ProviderConfig;
use crate::types::{ChatCompletionRequest, ChatCompletionResponse, ChatCompletionChunk};
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;

/// Ollama native proxy provider - forwards requests directly to Ollama API
pub struct OllamaNativeProvider {
    config: ProviderConfig,
    client: Client,
    base_url: String,
}

impl OllamaNativeProvider {
    pub fn new(config: ProviderConfig) -> Self {
        let base_url = config.base_url.clone()
            .unwrap_or_else(|| "http://localhost:11434".to_string());

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("Failed to create HTTP client");

        Self { config, client, base_url }
    }

    /// Get available models from Ollama
    pub async fn get_models(&self) -> Result<Vec<OllamaModel>> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let data: OllamaTagsResponse = resp.json().await?;
        
        Ok(data.models.into_iter().map(|m| OllamaModel {
            name: m.name,
            details: m.details,
        }).collect())
    }

    /// Check if model supports tool calling
    fn model_supports_tools(&self, model: &str) -> bool {
        !self.config.model_no_tool_support.iter().any(|m| m == model)
    }

    /// Forward chat completion to Ollama
    pub async fn forward_chat(
        &self,
        model: &str,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        // Check model_no_tool_support: reject if tools requested for non-tool-capable model
        if request.tools.is_some() && !self.model_supports_tools(model) {
            return Err(anyhow!("model_no_tool_support: model '{}' does not support tool calling", model));
        }

        // Convert to Ollama format
        let ollama_req = self.to_ollama_request(model, &request, false, true);

        let url = format!("{}/api/chat", self.base_url);
        let resp = self.client.post(&url)
            .json(&ollama_req)
            .send()
            .await?;

        let ollama_resp: OllamaChatResponse = resp.json().await?;
        self.from_ollama_response(ollama_resp)
    }

    /// Stream a chat completion as [`RunEvent`]s (RS3).
    pub async fn stream_events(
        &self,
        model: &str,
        request: &ChatCompletionRequest,
        tx: mpsc::Sender<crate::reasoning::RunEvent>,
    ) -> Result<()> {
        use crate::reasoning::{FinishReason, ReasoningKind, RunEvent};
        use futures::StreamExt;

        if request.tools.is_some() && !self.model_supports_tools(model) {
            let _ = tx
                .send(RunEvent::Error {
                    message: format!(
                        "model_no_tool_support: model '{}' does not support tool calling",
                        model
                    ),
                    code: Some("provider_error".to_string()),
                })
                .await;
            return Ok(());
        }

        let ollama_req = self.to_ollama_request(model, request, true, true);
        let url = format!("{}/api/chat", self.base_url);
        let resp = self.client.post(&url).json(&ollama_req).send().await?;
        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut in_reasoning = false;
        let mut block: u32 = 0;

        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(idx) = buffer.find('\n') {
                let line: String = buffer[..idx].to_string();
                buffer.drain(..=idx);
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(chunk) = serde_json::from_str::<OllamaChatChunk>(&line) else {
                    continue;
                };
                // Thinking delta.
                if let Some(thinking) = chunk.message.thinking.as_deref() {
                    if !thinking.is_empty() {
                        if !in_reasoning {
                            in_reasoning = true;
                            let _ = tx
                                .send(RunEvent::ReasoningStart {
                                    block,
                                    kind: ReasoningKind::Raw,
                                })
                                .await;
                        }
                        let _ = tx
                            .send(RunEvent::ReasoningDelta {
                                block,
                                text: thinking.to_string(),
                            })
                            .await;
                    }
                }
                // Content delta ends reasoning.
                if !chunk.message.content.is_empty() {
                    if in_reasoning {
                        in_reasoning = false;
                        let _ = tx.send(RunEvent::ReasoningEnd { block }).await;
                        block = block.wrapping_add(1);
                    }
                    let _ = tx
                        .send(RunEvent::Text {
                            text: chunk.message.content,
                        })
                        .await;
                }
                if chunk.done {
                    if in_reasoning {
                        in_reasoning = false;
                        let _ = tx.send(RunEvent::ReasoningEnd { block }).await;
                    }
                    let _ = tx
                        .send(RunEvent::Done {
                            reason: FinishReason::Stop,
                        })
                        .await;
                }
            }
        }
        Ok(())
    }

    /// Forward streaming chat completion to Ollama
    pub async fn forward_chat_stream(
        &self,
        model: &str,
        request: &ChatCompletionRequest,
        tx: mpsc::Sender<ChatCompletionChunk>,
    ) -> Result<()> {
        // Check model_no_tool_support: reject if tools requested for non-tool-capable model
        if request.tools.is_some() && !self.model_supports_tools(model) {
            return Err(anyhow!("model_no_tool_support: model '{}' does not support tool calling", model));
        }

        let ollama_req = self.to_ollama_request(model, &request, true, true);
        let url = format!("{}/api/chat", self.base_url);

        let resp = self.client.post(&url)
            .json(&ollama_req)
            .send()
            .await?;

        let mut stream = resp.bytes_stream();

        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            let lines = String::from_utf8_lossy(&bytes);

            for line in lines.lines() {
                if line.trim().is_empty() {
                    continue;
                }

                if let Ok(ollama_chunk) = serde_json::from_str::<OllamaChatChunk>(line) {
                    if let Some(chunk) = self.from_ollama_chunk(ollama_chunk) {
                        let _ = tx.send(chunk).await;
                    }
                }
            }
        }

        Ok(())
    }

    fn to_ollama_request(
        &self,
        model: &str,
        request: &ChatCompletionRequest,
        stream: bool,
        think: bool,
    ) -> OllamaChatRequest {
        let messages: Vec<OllamaMessage> = request.messages.iter().map(|m| {
            OllamaMessage {
                role: m.role.to_string(),
                content: m.content.clone().unwrap_or_default(),
                images: None,
                thinking: None,
            }
        }).collect();

        OllamaChatRequest {
            model: model.to_string(),
            messages,
            stream,
            think: if think { Some(true) } else { None },
            options: Some(OllamaOptions {
                num_ctx: Some(self.config.num_ctx.unwrap_or(8192)),
                temperature: request.temperature.map(|t| t as f32),
                top_p: request.top_p.map(|t| t as f32),
                num_predict: request.max_tokens.map(|v| v as i64),
            }),
            tools: request.tools.as_ref().map(|tools| {
                tools.iter().filter_map(|t| {
                    let func = &t.function;
                    Some(OllamaTool {
                        name: func.name.clone(),
                        description: func.description.clone(),
                        parameters: func.parameters.clone(),
                    })
                }).collect()
            }),
        }
    }

    fn from_ollama_response(&self, resp: OllamaChatResponse) -> Result<ChatCompletionResponse> {
        let role = crate::types::MessageRole::from_string(&resp.message.role);
        let message = crate::types::Message {
            role,
            content: Some(resp.message.content),
            reasoning_content: resp.message.thinking,
            tool_calls: resp.message.tool_calls.map(|tcs| tcs.into_iter().map(|tc| {
                crate::types::ToolCall {
                    id: tc.id,
                    r#type: "function".to_string(),
                    function: crate::types::FunctionCall {
                        name: tc.function.name,
                        arguments: tc.function.arguments,
                    },
                }
            }).collect()),
            ..Default::default()
        };

        Ok(ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            model: resp.model,
            choices: vec![crate::types::Choice {
                index: 0,
                message,
                finish_reason: Some(if resp.done { "stop".to_string() } else { "length".to_string() }),
            }],
            usage: Some(crate::types::Usage {
                prompt_tokens: resp.prompt_eval_count.unwrap_or(0) as u32,
                completion_tokens: resp.eval_count.unwrap_or(0) as u32,
                total_tokens: (resp.prompt_eval_count.unwrap_or(0) + resp.eval_count.unwrap_or(0)) as u32,
            }),
        })
    }

    fn from_ollama_chunk(&self, chunk: OllamaChatChunk) -> Option<ChatCompletionChunk> {
        let role = crate::types::MessageRole::from_string(&chunk.message.role);
        let delta = crate::types::ChatCompletionChunkDelta {
            role: Some(role.to_string()),
            content: Some(chunk.message.content),
            tool_calls: chunk.message.tool_calls.map(|tcs| tcs.into_iter().map(|tc| {
                crate::types::ToolCallChunk {
                    index: tc.index as u32,
                    id: Some(tc.id),
                    r#type: Some("function".to_string()),
                    function: Some(crate::types::FunctionCallChunk {
                        name: Some(tc.function.name),
                        arguments: Some(tc.function.arguments),
                    }),
                }
            }).collect()),
            reasoning_content: None,
        };

        Some(ChatCompletionChunk {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            object: "chat.completion.chunk".to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            model: chunk.model,
            choices: vec![crate::types::ChatCompletionChunkChoice {
                index: 0,
                delta,
                finish_reason: if chunk.done { Some("stop".to_string()) } else { None },
            }],
            usage: None,
        })
    }
}

/// Ollama API request/response types
#[derive(serde::Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaTagModel>,
}

#[derive(serde::Deserialize)]
struct OllamaTagModel {
    name: String,
    details: OllamaModelDetails,
}

#[derive(serde::Deserialize)]
pub struct OllamaModel {
    pub name: String,
    pub details: OllamaModelDetails,
}

#[derive(serde::Deserialize)]
#[derive(Debug, Clone)]
pub struct OllamaModelDetails {
    pub family: String,
    pub parameter_size: String,
    pub quantizations: Vec<String>,
}

#[derive(serde::Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    options: Option<OllamaOptions>,
    tools: Option<Vec<OllamaTool>>,
}

#[derive(serde::Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    images: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
}

#[derive(serde::Serialize)]
struct OllamaOptions {
    num_ctx: Option<usize>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    num_predict: Option<i64>,
}

#[derive(serde::Serialize)]
struct OllamaTool {
    name: String,
    description: Option<String>,
    parameters: Option<Value>,
}

#[derive(serde::Deserialize)]
struct OllamaChatResponse {
    model: String,
    message: OllamaMessageResponse,
    done: bool,
    prompt_eval_count: Option<usize>,
    eval_count: Option<usize>,
}

#[derive(serde::Deserialize)]
struct OllamaMessageResponse {
    role: String,
    content: String,
    #[serde(default)]
    thinking: Option<String>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(serde::Deserialize)]
struct OllamaToolCall {
    id: String,
    function: OllamaFunctionCall,
    #[allow(dead_code)]
    index: usize,
}

#[derive(serde::Deserialize)]
struct OllamaFunctionCall {
    name: String,
    arguments: String,
}

#[derive(serde::Deserialize)]
struct OllamaChatChunk {
    model: String,
    message: OllamaMessageChunk,
    done: bool,
}

#[derive(serde::Deserialize)]
struct OllamaMessageChunk {
    role: String,
    content: String,
    #[serde(default)]
    thinking: Option<String>,
    tool_calls: Option<Vec<OllamaToolCallChunk>>,
}

#[derive(serde::Deserialize)]
struct OllamaToolCallChunk {
    id: String,
    function: OllamaFunctionCallChunk,
    #[allow(dead_code)]
    index: usize,
}

#[derive(serde::Deserialize)]
struct OllamaFunctionCallChunk {
    name: String,
    arguments: String,
}