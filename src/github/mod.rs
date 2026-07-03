//! GitHub API client and data types (compatibility re-exports).
//!
//! The concrete implementation now lives under `crate::git_provider::github`.
//! This module re-exports the same public items for backwards compatibility
//! with existing callers.

pub use crate::git_provider::github::{client, pagination, types};
