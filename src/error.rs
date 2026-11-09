use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("CLI binary not found: {0}")]
    CliNotFound(String),

    #[error("Request timeout")]
    Timeout,

    #[error("Authentication required")]
    Unauthorized,

    #[error("Invalid API key")]
    InvalidApiKey,

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Tool error: {0}")]
    ToolError(String),

    #[error("Streaming not implemented")]
    StreamingNotImplemented,

    #[error("tool_bridge_unsupported")]
    ToolBridgeUnsupported,

    #[error("tool_isolation_unavailable")]
    ToolIsolationUnavailable,

    #[error("tool_run_expired")]
    ToolRunExpired,

    #[error("tool_already_resolved")]
    ToolAlreadyResolved,

    #[error("too_many_parked_runs")]
    TooManyParkedRuns,

    #[error("unsupported_tool_type")]
    UnsupportedToolType,

    #[error("model_no_tool_support")]
    ModelNoToolSupport,

    #[error("provider_error: {0}")]
    ProviderError(String),

    #[error("tool_isolation_violation: {0}")]
    ToolIsolationViolation(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::ToolBridgeUnsupported => "tool_bridge_unsupported",
            AppError::ToolIsolationUnavailable => "tool_isolation_unavailable",
            AppError::ToolRunExpired => "tool_run_expired",
            AppError::ToolAlreadyResolved => "tool_already_resolved",
            AppError::TooManyParkedRuns => "too_many_parked_runs",
            AppError::UnsupportedToolType => "unsupported_tool_type",
            AppError::ModelNoToolSupport => "model_no_tool_support",
            AppError::ProviderError(_) => "provider_error",
            AppError::ToolIsolationViolation(_) => "tool_isolation_violation",
            AppError::ToolError(_) => "tool_error",
            _ => "internal_error",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::ProviderNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::ModelNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::CliNotFound(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::Timeout => (StatusCode::REQUEST_TIMEOUT, self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::InvalidApiKey => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::ConfigError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            AppError::ToolError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::StreamingNotImplemented => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::ToolBridgeUnsupported => (StatusCode::NOT_IMPLEMENTED, self.to_string()),
            AppError::ToolIsolationUnavailable => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::ToolRunExpired => (StatusCode::CONFLICT, self.to_string()),
            AppError::ToolAlreadyResolved => (StatusCode::CONFLICT, self.to_string()),
            AppError::TooManyParkedRuns => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            AppError::UnsupportedToolType => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::ModelNoToolSupport => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::ProviderError(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::ToolIsolationViolation(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
        };

        let code = self.code();
        // Per spec §7.6: 5xx -> server_error/provider_error, 4xx -> invalid_request_error
        let error_type = if status.is_server_error() {
            "provider_error"
        } else {
            "invalid_request_error"
        };
        let body = axum::Json(serde_json::json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code,
            }
        }));

        (status, body).into_response()
    }
}
