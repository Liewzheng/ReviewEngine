//! Centralized error types for review-engine.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! It defines a single `ReviewEngineError` enum and a `Result<T>` alias
//! that can be adopted incrementally across the codebase.  Existing uses
//! of `anyhow` remain untouched in this PR; new code may return
//! `crate::error::Result<T>` where a typed error is helpful.

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
