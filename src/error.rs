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
        };

        let body = axum::Json(serde_json::json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
            }
        }));

        (status, body).into_response()
    }
}
