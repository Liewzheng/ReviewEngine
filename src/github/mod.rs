//! GitHub API client and data types.
//!
//! The `client` submodule implements the [`GitProvider`] trait for
//! GitHub pull requests, handling authentication, fetching PR metadata
//! and diffs, and posting reviews and inline comments via the GitHub
//! REST API. The `types` submodule provides GitHub-specific request
//! and response structures, including review comment payloads and
//! reaction types.

pub mod client;
pub mod pagination;
pub mod types;
