//! Error types and result definitions for Codemagic API operations.

use thiserror::Error;

/// Specific error conditions encountered when communicating with Codemagic APIs.
#[derive(Error, Debug)]
pub enum CodemagicError {
    /// Authentication failed (HTTP 401 Unauthorized).
    #[error(
        "Authentication failed (401 Unauthorized). Please verify your Codemagic API token using 'codemagic auth login' or CODEMAGIC_API_TOKEN environment variable."
    )]
    Unauthorized,

    /// Access forbidden (HTTP 403 Forbidden).
    #[error("Permission denied (403 Forbidden): {0}")]
    Forbidden(String),

    /// Requested resource does not exist (HTTP 404 Not Found).
    #[error("Resource not found (404 Not Found): {0}")]
    NotFound(String),

    /// API rate limit exceeded (HTTP 429 Too Many Requests).
    #[error("Rate limit exceeded (429 Too Many Requests). Please retry after some time.")]
    RateLimited,

    /// General Codemagic API error with HTTP status code and server message.
    #[error("Codemagic API error ({status}): {message}")]
    ApiError {
        /// HTTP status code.
        status: u16,
        /// Server error message or detail string.
        message: String,
    },

    /// No API token was supplied or resolved.
    #[error(
        "No API token provided. Please run 'codemagic auth login', pass --token, or set CODEMAGIC_API_TOKEN."
    )]
    MissingToken,

    /// Underlying HTTP client error from `reqwest`.
    #[error("Network / HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization or deserialization failure.
    #[error("JSON serialization / deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// Local file or stream I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Specialized `Result` type for Codemagic client operations.
pub type Result<T> = std::result::Result<T, CodemagicError>;
