use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, AppError>;

/// Stable machine-readable classifications. Network restrictions are never
/// reported as parser failures or as successful empty extraction results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    NotFound,
    QueueFull,
    RateLimited,
    Cancelled,
    Network,
    Dns,
    AccessRestricted,
    UpstreamRateLimited,
    PlayerChanged,
    UnsupportedFormat,
    ExpiredUrl,
    DownloadInterrupted,
    DownloadTooLarge,
    FfmpegFailed,
    Storage,
    Timeout,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    /// Messages must be safe for clients: omit credentials, signed stream URLs,
    /// local filesystem paths and upstream response bodies.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn status(&self) -> StatusCode {
        match self.code {
            ErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::QueueFull => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::RateLimited | ErrorCode::UpstreamRateLimited => {
                StatusCode::TOO_MANY_REQUESTS
            }
            ErrorCode::Cancelled => StatusCode::CONFLICT,
            ErrorCode::AccessRestricted => StatusCode::FORBIDDEN,
            ErrorCode::UnsupportedFormat => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::DownloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorCode::Network
            | ErrorCode::Dns
            | ErrorCode::PlayerChanged
            | ErrorCode::ExpiredUrl
            | ErrorCode::DownloadInterrupted => StatusCode::BAD_GATEWAY,
            ErrorCode::FfmpegFailed | ErrorCode::Storage | ErrorCode::Internal => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let mut response = (status, Json(serde_json::json!({"error": self}))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_classified_and_stable() {
        let error = AppError::new(ErrorCode::AccessRestricted, "Public access denied");
        assert_eq!(error.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            serde_json::to_value(error).unwrap()["code"],
            "ACCESS_RESTRICTED"
        );
        assert_eq!(
            AppError::new(ErrorCode::PlayerChanged, "Unsupported response").status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            AppError::new(ErrorCode::QueueFull, "Try later").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
