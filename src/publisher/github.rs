//! GitHub implementation of the Publisher trait. Manages PR review posts and inline comments.
//!
//! @module review-engine: CodeReview Board platform
use anyhow::Result;
use async_trait::async_trait;

use super::{InlineNote, Publisher};
use crate::github::client::Client as GitHubClient;

const BOT_REVIEW_TITLE: &str = "# CodeReview Board";

/// GitHub implementation of Publisher.
pub struct GitHubPublisher {
    client: GitHubClient,
}

impl GitHubPublisher {
    pub fn new(token: &str, pr_url: &str) -> Result<Self> {
        let client = GitHubClient::new(token, pr_url)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Publisher for GitHubPublisher {
    async fn post_mr_discussion(&self, body: &str) -> Result<String> {
        let review_id = self.client.create_pr_review(body).await?;
        Ok(review_id.to_string())
    }

    async fn post_inline_note(&self, note: &InlineNote) -> Result<()> {
        self.client
            .create_review_comment(&note.file, note.line, &note.body)
            .await
    }

    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()> {
        let review_id: i64 = discussion_id.parse()?;
        self.client.update_pr_review(review_id, body).await
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
        self.post_mr_discussion(body).await
    }
}
