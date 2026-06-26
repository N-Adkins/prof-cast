//! Error types used by profcast

use thiserror::Error;

/// Convenience Result wrapper used throughout profcast
pub type Result<T> = std::result::Result<T, ProfcastError>;

/// Errors returned by profcast operations
#[derive(Debug, Error)]
pub enum ProfcastError {
    /// An I/O operation failed
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON operation failed
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
