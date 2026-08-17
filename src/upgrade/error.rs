//! Error type for the self-update subsystem.
//!
//! Kept local to `upgrade` (rather than extending `crate::error`) so the
//! module stays self-contained and does not force the global error enum to
//! grow upgrade-specific variants.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, UpgradeError>;

#[derive(Debug, Error)]
pub enum UpgradeError {
    /// Transport-level failure (DNS, connect, read, TLS).
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// HTTP 4xx/5xx from the release endpoint or the asset CDN.
    #[error("GitHub API returned {status}: {body}")]
    Api { status: u16, body: String },

    /// Filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Malformed version tag, checksum file, or download size mismatch.
    #[error("invalid data: {0}")]
    InvalidData(String),

    /// The current OS/ARCH has no published release asset.
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),

    /// SHA-256 hash mismatch between downloaded asset and its sidecar.
    #[error("checksum mismatch: expected {expected}, computed {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    /// Archive entry path that could escape the extraction root (zip-slip).
    #[error("unsafe archive entry: {0}")]
    UnsafeEntry(String),

    /// Corrupt or otherwise unreadable gzip/tar/zip payload.
    #[error("archive error: {0}")]
    Archive(String),

    /// No stable release / no matching asset on the remote side.
    #[error("not found: {0}")]
    NotFound(String),
}

impl UpgradeError {
    /// Construct an `InvalidData` error (malformed input, size mismatch, etc.).
    pub fn invalid_data(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct an `UnsupportedPlatform` error for missing release assets.
    pub fn unsupported_platform(msg: impl Into<String>) -> Self {
        Self::UnsupportedPlatform(msg.into())
    }

    /// Construct a `ChecksumMismatch` error with expected and actual hashes.
    pub fn checksum_mismatch(expected: impl Into<String>, actual: impl Into<String>) -> Self {
        Self::ChecksumMismatch {
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    /// Construct an `UnsafeEntry` error for zip-slip / path traversal.
    pub fn unsafe_entry(msg: impl Into<String>) -> Self {
        Self::UnsafeEntry(msg.into())
    }

    /// Construct an `Archive` error for corrupt/unreadable archive payloads.
    pub fn archive(msg: impl Into<String>) -> Self {
        Self::Archive(msg.into())
    }

    /// Construct a `NotFound` error for missing releases or assets.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
}
