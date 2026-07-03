//! GitHub implementation of the GitProvider trait and supporting client modules.
//!
//! This module is the single source of truth for GitHub integration in
//! review-engine. The `client`, `pagination`, and `types` submodules live
//! alongside the provider so all GitHub-specific code is co-located under
//! `src/git_provider/github/`.

pub mod client;
pub mod pagination;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;

use crate::git_provider::GitProvider;
use crate::models::MRInfo;

const BOT_REVIEW_TITLE: &str = "# CodeReview Board";

/// GitHub implementation of GitProvider.
pub struct GitHubProvider {
    client: client::Client,
}

impl GitHubProvider {
    /// Create a new `GitHubProvider` for the given personal access token and PR URL.
    ///
    /// # Parameters
    /// * `token` — GitHub personal access token used for API authentication.
    /// * `pr_url` — Full URL to the pull request (e.g. `https://github.com/owner/repo/pull/123`).
    ///
    /// # Errors
    /// Returns an error if `pr_url` cannot be parsed into a valid GitHub PR URL.
    pub fn new(token: &str, pr_url: &str) -> Result<Self> {
        let client = client::Client::new(token, pr_url)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl GitProvider for GitHubProvider {
    async fn fetch_mr_info(&self) -> Result<MRInfo> {
        self.client.fetch_pr_info().await
    }

    async fn fetch_diff(&self) -> Result<String> {
        self.client.fetch_diff().await
    }

    async fn post_review_comment(&self, body: &str) -> Result<i64> {
        self.client.create_pr_review(body).await
    }

    async fn post_inline_comment(&self, file: &str, line: u32, body: &str) -> Result<()> {
        self.client.create_review_comment(file, line, body).await
    }

    async fn fetch_code_audit_toml(&self) -> Result<Option<String>> {
        // GitHub doesn't have a built-in config repo file fetch via PR API.
        // This would need a separate content API call.
        Ok(None)
    }

    async fn add_reaction(&self, _comment_id: i64, _reaction: &str) -> Result<()> {
        // GitHub reactions API: POST /repos/:owner/:repo/pulls/comments/:comment_id/reactions
        // Not implemented yet. Reactions require a different API endpoint.
        anyhow::bail!("add_reaction not implemented for GitHub")
    }

    async fn find_or_update_discussion(&self, body: &str) -> Result<String> {
        let bot_user = self.client.get_current_user().await?;
        let reviews = self.client.list_pr_reviews().await?;

        // Look for the bot's own review (PR review, not comment)
        for review in &reviews {
            if review.user.id == bot_user.id
                && review
                    .body
                    .as_deref()
                    .map_or(false, |b| b.starts_with(BOT_REVIEW_TITLE))
            {
                self.client.update_pr_review(review.id, body).await?;
                return Ok(review.id.to_string());
            }
        }

        // No existing review found — create a new one
        let id = self.client.create_pr_review(body).await?;
        Ok(id.to_string())
    }

    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()> {
        let review_id: i64 = discussion_id.parse()?;
        self.client.update_pr_review(review_id, body).await
    }
}
