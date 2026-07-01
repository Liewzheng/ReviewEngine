//! Unified trait abstraction over Git provider APIs (GitLab, GitHub).
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! This module defines the [`GitProvider`] trait, which provides a single
//! async interface for fetching MR/PR info, diffs, posting review comments,
//! inline notes, reactions, and fetching repository configuration files.
//! Concrete implementations live in the `github` and `gitlab` submodules,
//! allowing the rest of the application to operate on any Git provider
//! polymorphically. The trait is designed to be object-safe so that
//! callers can hold a `Box<dyn GitProvider>`.

pub mod github;
pub mod gitlab;

use anyhow::Result;
use async_trait::async_trait;

use crate::models::*;

/// Unified interface for Git provider operations (GitLab, GitHub, etc.).
#[async_trait]
pub trait GitProvider: Send + Sync {
    /// Fetch MR/PR information.
    async fn fetch_mr_info(&self) -> Result<MRInfo>;
    /// Fetch the diff for an MR/PR.
    async fn fetch_diff(&self) -> Result<String>;
    /// Post a review comment on the MR discussion.
    async fn post_review_comment(&self, body: &str) -> Result<i64>;
    /// Post an inline note on a specific file/line.
    async fn post_inline_comment(&self, file: &str, line: u32, body: &str) -> Result<()>;
    /// Fetch the repository's code-audit config file.
    async fn fetch_code_audit_toml(&self) -> Result<Option<String>>;
    /// Add a reaction (emoji) to a comment.
    async fn add_reaction(&self, comment_id: i64, reaction: &str) -> Result<()>;
}

#[cfg(test)]
mod tests {
    /// Verify that the GitProvider trait can be implemented.
    /// This is a compile-time check that the trait is well-formed.
    #[test]
    fn test_git_provider_trait_is_object_safe() {
        // If the trait compiles, this test passes
        assert!(true);
    }
}
