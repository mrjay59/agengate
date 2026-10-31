use crate::error::AppError;
use crate::service::ServiceRegistry;
use crate::types::{
    AddKeyRequest, AddMcpRequest, AddProviderRequest, AddSkillRequest, ChatCompletionRequest,
    ChatCompletionResponse, ModelsResponse, ProxyRequest,
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
) -> Result<Json<ChatCompletionResponse>, AppError> {
    // Ignore stream parameter - always return non-streaming response
    // This fixes UNKNOWN_UPSTREAM_ERROR when OpenAI client sends stream:true
    let response = registry.chat_completion(request).await?;
    Ok(Json(response))
}

async fn health_check() -> &'static str {
    "OK"
}

async fn add_provider(
    State(_registry): State<Arc<ServiceRegistry>>,
    Json(request): Json<AddProviderRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Need to mutate registry - using Arc<RwLock> pattern would need interior mutability
    // For now, just acknowledge
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
