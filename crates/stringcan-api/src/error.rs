use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use stringcan_core::EngineError;
use stringcan_payments::ProcessorError;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("payment failed: {0}")]
    PaymentFailed(String),
    #[error("processor unavailable")]
    ProcessorUnavailable,
    #[error("internal error")]
    Internal(String),
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::AmountOutOfRange
            | EngineError::InvalidConfig(_)
            | EngineError::ZeroPayment => ApiError::BadRequest(e.to_string()),
            EngineError::UnknownSubscriber(_) => ApiError::NotFound,
            EngineError::Audit(_) => ApiError::Internal(e.to_string()),
            _ => ApiError::Conflict(e.to_string()),
        }
    }
}

impl From<ProcessorError> for ApiError {
    fn from(e: ProcessorError) -> Self {
        match e {
            ProcessorError::Declined(m) => ApiError::PaymentFailed(m),
            ProcessorError::Invalid(m) => ApiError::BadRequest(m.into()),
            ProcessorError::NotWired(_) | ProcessorError::LiveKeyRefused => {
                ApiError::ProcessorUnavailable
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::PaymentFailed(_) => StatusCode::PAYMENT_REQUIRED,
            ApiError::ProcessorUnavailable => StatusCode::BAD_GATEWAY,
            ApiError::Internal(detail) => {
                tracing::error!(%detail, "internal error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}
