//! Error types used by profcast

use thiserror::Error;

/// Convenience Result wrapper used throughout profcast
pub type Result<T> = std::result::Result<T, ProfcastError>;

/// Errors returned by profcast operations
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfcastError {
    /// An I/O operation failed
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON operation failed
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Input bytes were not valid UTF-8 text
    #[error("invalid UTF-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    /// A [`Profile`](crate::model::Profile) could not be parsed from its input
    #[error("parse error on line {line}: {message}")]
    Parse {
        /// 1-based line number where parsing failed
        line: usize,
        /// Human-readable description of what went wrong
        message: String,
    },

    /// A [`Profile`](crate::model::Profile) violated the data model's
    /// structural invariants
    #[error("invalid profile: {0}")]
    InvalidProfile(String),

    /// A binary format's wire representation could not be decoded
    #[error("decode error: {0}")]
    Decode(String),

    /// A live [`Source`](crate::capture::Source) failed while sampling
    #[error("capture error: {0}")]
    Capture(String),

    /// A requested operation is not supported on this host (wrong platform,
    /// missing kernel feature, insufficient permissions)
    #[error("unsupported: {0}")]
    Unsupported(String),
}
