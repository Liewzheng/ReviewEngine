//! Central error types for the review engine.
//!
//! This module defines a shared `ReviewEngineError` enum and `Result` alias.
//! Existing code largely uses `anyhow`; this module is introduced incrementally
//! and new fallible code should prefer `crate::error::Result`.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ReviewEngineError>;

#[derive(Debug, Error)]
pub enum ReviewEngineError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("configuration parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("LLM error: {0}")]
    LLM(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ReviewEngineError {
    pub fn provider(msg: impl Into<String>) -> Self {
        Self::Provider(msg.into())
    }

    pub fn llm(msg: impl Into<String>) -> Self {
        Self::LLM(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}
