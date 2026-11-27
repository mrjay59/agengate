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

    #[error("provider_unavailable: {0}")]
    ProviderUnavailable(String),

    #[error("tool_isolation_violation: {0}")]
    ToolIsolationViolation(String),

    /// FIX-QUOTA-ACCOUNTING: 429 with `error.type = quota_exceeded`.
    #[error("quota_exceeded: {reason}")]
    QuotaExceeded {
        reason: String,
        scope: String,
        reset_at: Option<i64>,
    },

    /// FIX-KEYSTORE: authentication / authorization failures with stable codes.
    #[error("invalid_api_key")]
    KeyInvalid,
    #[error("api_key_expired")]
    KeyExpired,
    #[error("api_key_revoked")]
    KeyRevoked,
    #[error("scope_denied: {0}")]
    ScopeDenied(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("bad_request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not_found: {0}")]
    NotFound(String),
    #[error("store_error: {0}")]
    StoreError(String),
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
            AppError::ProviderUnavailable(_) => "provider_unavailable",
            AppError::ToolIsolationViolation(_) => "tool_isolation_violation",
            AppError::ToolError(_) => "tool_error",
            AppError::QuotaExceeded { .. } => "quota_exceeded",
            AppError::KeyInvalid => "invalid_api_key",
            AppError::KeyExpired => "api_key_expired",
            AppError::KeyRevoked => "api_key_revoked",
            AppError::ScopeDenied(_) => "scope_denied",
            AppError::Forbidden(_) => "forbidden",
            AppError::BadRequest(_) => "bad_request",
            AppError::Conflict(_) => "conflict",
            AppError::NotFound(_) => "not_found",
            AppError::StoreError(_) => "store_error",
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
            AppError::ProviderUnavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::ToolIsolationViolation(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::QuotaExceeded { .. } => (StatusCode::TOO_MANY_REQUESTS, self.to_string()),
            AppError::KeyInvalid | AppError::KeyExpired | AppError::KeyRevoked => {
                (StatusCode::UNAUTHORIZED, self.to_string())
            }
            AppError::ScopeDenied(_) | AppError::Forbidden(_) => (StatusCode::FORBIDDEN, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Conflict(_) => (StatusCode::CONFLICT, self.to_string()),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::StoreError(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };

        let code = self.code();
        // Per spec §7.6: 5xx -> server_error/provider_error, 4xx -> invalid_request_error.
        // FIX-QUOTA-ACCOUNTING: quota errors use their own type + reset metadata
        // (spec §10.2) so clients can tell them apart from ordinary 429s.
        let error_type = match self {
            AppError::QuotaExceeded { .. } => "quota_exceeded",
            AppError::KeyInvalid | AppError::KeyExpired | AppError::KeyRevoked
            | AppError::ScopeDenied(_) | AppError::Forbidden(_) => "invalid_request_error",
            _ if status.is_server_error() => "provider_error",
            _ => "invalid_request_error",
        };
        let mut error_obj = serde_json::json!({
            "message": message,
            "type": error_type,
            "code": code,
        });
        if let AppError::QuotaExceeded { reason, scope, reset_at } = self {
            let obj = error_obj.as_object_mut().expect("json object");
            obj.insert("reason".to_string(), serde_json::json!(reason));
            obj.insert("scope".to_string(), serde_json::json!(scope));
            obj.insert("reset_at".to_string(), serde_json::json!(reset_at));
        }
        let body = axum::Json(serde_json::json!({ "error": error_obj }));

        (status, body).into_response()
    }
}




