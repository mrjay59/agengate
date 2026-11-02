use crate::error::AppError;
use crate::service::ServiceRegistry;
use crate::types::{
    AddKeyRequest, AddMcpRequest, AddProviderRequest, AddSkillRequest, ChatCompletionRequest,
    ModelsResponse, ProxyRequest,
};
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;
use tracing::info;

pub fn api() -> Router<Arc<ServiceRegistry>> {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/health", get(health_check))
        .route("/admin/provider", post(add_provider))
        .route("/admin/key", post(add_key))
        .route("/admin/proxy", post(proxy))
        .route("/admin/skill", post(add_skill))
        .route("/admin/mcp", post(add_mcp))
}

async fn list_models(
    State(registry): State<Arc<ServiceRegistry>>,
) -> Result<Json<ModelsResponse>, AppError> {
    let models = registry.list_models(None).await?;
    Ok(Json(models))
}

async fn chat_completions(
    State(registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    println!("[API] Request received: model={}, stream={:?}, messages={}",
        request.model, request.stream, request.messages.len());

    let is_stream = request.stream.unwrap_or(false);
    let response = registry.chat_completion(request).await?;

    if is_stream {
        let content = response.choices.first().map(|c| c.message.content.clone()).unwrap_or_default();
        let reasoning = response.choices.first().and_then(|c| c.message.reasoning_content.clone()).unwrap_or_default();
        let mut frames = String::new();
        if !reasoning.trim().is_empty() {
            let reasoning_json = serde_json::json!({
                "id": response.id,
                "object": "chat.completion.chunk",
                "created": response.created,
                "model": response.model,
                "choices": [{
                    "index": 0,
                    "delta": { "reasoning_content": reasoning },
                    "finish_reason": null
                }]
            });
            frames.push_str(&format!("data: {}\n\n", reasoning_json.to_string()));
        }
        if !content.trim().is_empty() {
            let content_json = serde_json::json!({
                "id": response.id,
                "object": "chat.completion.chunk",
                "created": response.created,
                "model": response.model,
                "choices": [{
                    "index": 0,
                    "delta": { "content": content },
                    "finish_reason": null
                }]
            });
            frames.push_str(&format!("data: {}\n\n", content_json.to_string()));
        }
        let done_json = serde_json::json!({
            "id": response.id,
            "object": "chat.completion.chunk",
            "created": response.created,
            "model": response.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        frames.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", done_json.to_string()));
        let mut res = frames.into_response();
        res.headers_mut().insert(axum::http::header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
        Ok(res)
    } else {
        Ok(Json(response).into_response())
    }
}

async fn health_check() -> Json<serde_json::Value> {
    info!("Health check called");
    Json(serde_json::json!({"status": "ok"}))
}

async fn add_provider(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddProviderRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add provider {}", request.id);
    Ok(Json(serde_json::json!({"status": "ok"})))
}

async fn add_key(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddKeyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add key for provider {}", request.provider);
    Ok(Json(serde_json::json!({"status": "ok", "provider": request.provider})))
}

async fn proxy(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<ProxyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: proxy to {}", request.target);
    Ok(Json(serde_json::json!({"status": "ok", "target": request.target})))
}

async fn add_skill(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddSkillRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add skill {}", request.name);
    Ok(Json(serde_json::json!({"status": "ok", "skill": request.name})))
}

async fn add_mcp(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddMcpRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    info!("Admin endpoint: add MCP {}", request.name);
    Ok(Json(serde_json::json!({"status": "ok", "mcp": request.name})))
}
