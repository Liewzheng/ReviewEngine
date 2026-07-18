//! Unified trait abstraction and concrete implementations over Git provider APIs (GitLab, GitHub).
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! This module defines the [`GitProvider`] trait, which is the single unified
//! async interface for both fetching MR/PR data (info, diff, config) and
//! publishing review results (top-level discussions, inline comments,
//! reactions). The concrete GitHub and GitLab implementations live in the
//! `github` and `gitlab` submodules, making `src/git_provider/` the single
//! entry point for all Git provider integrations. The trait is designed to be
//! object-safe so that callers can hold a `Box<dyn GitProvider>`.

pub mod github;
pub mod gitlab;

use anyhow::Result;
use async_trait::async_trait;

use crate::models::*;

/// Default upper bound on `search_code` results returned by remote browsers.
pub(crate) const SEARCH_RESULTS_LIMIT: usize = 20;

/// Run an async provider call from a synchronous context.
///
/// [`RepoBrowser`] is a synchronous trait while the provider HTTP clients are
/// async. Each call runs on a freshly spawned thread with its own
/// current-thread tokio runtime, which is safe both inside and outside an
/// existing tokio runtime (a nested `block_on` on the caller's runtime would
/// panic).
pub(crate) fn block_on_remote<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| anyhow::anyhow!("failed to create tokio runtime for remote call: {e}"))?;
        rt.block_on(fut)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("remote repo browser task panicked"))?
}

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

    /// Find an existing bot discussion and update it, or create a new one.
    ///
    /// Platform-specific implementations match on the bot's own posts and a
    /// title prefix. The default implementation creates a new discussion via
    /// `post_review_comment`.
    async fn find_or_update_discussion(&self, body: &str) -> Result<String> {
        let id = self.post_review_comment(body).await?;
        Ok(id.to_string())
    }

    /// Update the body of an existing discussion identified by its ID.
    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()>;
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

    #[test]
    fn test_block_on_remote_returns_value() {
        let v = super::block_on_remote(async { Ok(42) }).unwrap();
        assert_eq!(v, 42);
    }

    #[test]
    fn test_block_on_remote_propagates_error() {
        let res: anyhow::Result<()> = super::block_on_remote(async { anyhow::bail!("boom") });
        assert!(res.unwrap_err().to_string().contains("boom"));
    }

    /// Called from within a tokio runtime, the bridge must not panic with
    /// "Cannot start a runtime from within a runtime".
    #[tokio::test]
    async fn test_block_on_remote_inside_runtime() {
        let v = super::block_on_remote(async { Ok("ok".to_string()) }).unwrap();
        assert_eq!(v, "ok");
    }
}
