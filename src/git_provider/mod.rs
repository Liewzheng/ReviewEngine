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

/// A failed raw repository-file fetch, keeping the HTTP status.
///
/// Callers that can recover differently per failure kind (the adjudication
/// pass of RENG-31: a file the revision does not contain is a different
/// situation from a credential that is not allowed to read the repository)
/// need the status, which a flattened `anyhow::Error` string does not expose.
/// Transport, decode and rejected-path failures carry `status: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFetchError {
    /// HTTP status of the provider response, when there was one.
    pub status: Option<u16>,
    /// Human-readable failure, identical to the message the `anyhow` variant
    /// of the same call produces.
    pub message: String,
}

impl FileFetchError {
    /// The file does not exist at the requested revision.
    pub fn is_not_found(&self) -> bool {
        self.status == Some(404)
    }

    /// The credential may not read the repository (401 unauthenticated /
    /// 403 forbidden).
    pub fn is_unauthorized(&self) -> bool {
        matches!(self.status, Some(401) | Some(403))
    }
}

impl std::fmt::Display for FileFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FileFetchError {}

/// A rejected inline-comment post, keeping the provider's verdict.
///
/// A flattened `anyhow::Error` string does not expose the HTTP status, and the
/// publish pass needs to tell "the provider refused this anchor" (GitLab's 400
/// on the position — RENG-71) from a permission or transport failure, and to
/// report what the provider actually answered. [`FileFetchError`] keeps the
/// same information for repository-file reads; this is its inline-post
/// counterpart. Transport failures, and failures raised before the request is
/// sent (a rejected path, a missing SHA ref), are plain `anyhow` errors with no
/// verdict — the publish pass falls back to the rendered message for those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineNoteError {
    /// HTTP status of the provider response, when there was one.
    pub status: Option<u16>,
    /// The provider's response body, verbatim (callers truncate it for the
    /// log).
    pub body: String,
    /// Human-readable failure, identical to the message the `anyhow` variant
    /// of the same call produces.
    pub message: String,
}

impl std::fmt::Display for InlineNoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InlineNoteError {}

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

/// Where an inline note anchors a diff line (RENG-99).
///
/// `line` is always the **new**-side (head) line number. `old_line` is the
/// base-side number of the *same* line and is `Some` exactly when the diff
/// leaves that line unchanged.
///
/// The pair is what GitLab's API actually matches on. It derives a note's
/// `line_code` itself — a client-sent `line_code` is not honoured — by looking
/// the position up in the diff with strict equality on **both** numbers
/// (`Gitlab::Diff::File#line_for_position`: `line.old_line == pos.old_line &&
/// line.new_line == pos.new_line`, the line's own unchanged side being masked to
/// `nil` for added/removed lines). The documented contract follows from that: an
/// added-line note sends `new_line` alone, a context-line note must send both,
/// and a note whose pair matches no diff line is rejected with
/// `400 … Note {:line_code=>["can't be blank", "must be a valid line code"]}` —
/// the production symptom this type exists to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineAnchor {
    /// Path of the file, relative to the repository root.
    pub file: String,
    /// 1-based line number on the new (head) side.
    pub line: u32,
    /// 1-based line number on the old (base) side, for a line the diff leaves
    /// unchanged. `None` for an added line, whose old side has no number.
    pub old_line: Option<u32>,
}

impl InlineAnchor {
    /// An anchor on new-side line `line`, with no old-side counterpart — the
    /// added-line shape, and the anchor the publisher submitted before RENG-99.
    pub fn new(file: impl Into<String>, line: u32) -> Self {
        Self {
            file: file.into(),
            line,
            old_line: None,
        }
    }

    /// The same anchor plus the old-side number of an unchanged line.
    pub fn with_old_line(mut self, old_line: u32) -> Self {
        self.old_line = Some(old_line);
        self
    }
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

    /// Post an inline note at [`InlineAnchor`], old-side line number included.
    ///
    /// The default implementation drops the old-side number and delegates to
    /// [`Self::post_inline_comment`]: GitHub addresses a review comment by one
    /// side (`side=RIGHT`), so that number is not part of its request. GitLab
    /// overrides this, because its `position` needs the pair for a line the diff
    /// leaves unchanged (RENG-99).
    async fn post_inline_comment_at(&self, anchor: &InlineAnchor, body: &str) -> Result<()> {
        self.post_inline_comment(&anchor.file, anchor.line, body).await
    }

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
