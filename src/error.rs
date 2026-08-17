//! Central error types for the review engine.
//!
//! This module defines a shared `ReviewEngineError` enum and `Result` alias.
//! Existing code largely uses `anyhow`; this module is introduced incrementally
//! and new fallible code should prefer `crate::error::Result`.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ReviewEngineError>;

#[derive(Debug, Error)]
pub enum ReviewEngineError {
    /// Filesystem or I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML configuration parse failure.
    #[error("configuration parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    /// LLM provider-level error (API key, endpoint, etc.).
    #[error("provider error: {0}")]
    Provider(String),

    /// LLM interaction error (rate limit, timeout, malformed response).
    #[error("LLM error: {0}")]
    LLM(String),

    /// Requested resource not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// Input validation failure.
    #[error("validation error: {0}")]
    Validation(String),

    /// Unexpected internal error.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ReviewEngineError {
    /// Construct a provider-level error.
    pub fn provider(msg: impl Into<String>) -> Self {
        Self::Provider(msg.into())
    }

    /// Construct an LLM interaction error.
    pub fn llm(msg: impl Into<String>) -> Self {
        Self::LLM(msg.into())
    }

    /// Construct a not-found error.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Construct a validation error.
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    /// Construct an internal error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}
