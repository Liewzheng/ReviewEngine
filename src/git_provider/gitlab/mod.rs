//! GitLab implementation of the GitProvider trait and supporting client module.
//!
//! This module is the single source of truth for GitLab integration in
//! review-engine. The `client` submodule lives alongside the provider so all
//! GitLab-specific code is co-located under `src/git_provider/gitlab/`.
//!
//!
//! @module review-engine

pub mod client;

use anyhow::Result;
use async_trait::async_trait;

use crate::git_provider::GitProvider;
use crate::models::MRInfo;

const BOT_DISCUSSION_TITLE: &str = "# CodeReview Board";

/// GitLab implementation of GitProvider.
pub struct GitLabProvider {
    client: client::Client,
}

impl GitLabProvider {
    pub fn new(gitlab_token: &str, mr_url: &str) -> Result<Self> {
        let client = client::Client::new(gitlab_token, mr_url)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl GitProvider for GitLabProvider {
    async fn fetch_mr_info(&self) -> Result<MRInfo> {
        self.client.fetch_mr_info().await
    }

    async fn fetch_diff(&self) -> Result<String> {
        self.client.fetch_diff().await
    }

    async fn post_review_comment(&self, body: &str) -> Result<i64> {
        self.client.post_note(body).await
    }

    async fn post_inline_comment(&self, file: &str, line: u32, body: &str) -> Result<()> {
        self.client.post_inline_note(file, line, body).await
    }

    async fn fetch_code_audit_toml(&self) -> Result<Option<String>> {
        self.client.fetch_config_toml().await
    }

    async fn add_reaction(&self, comment_id: i64, reaction: &str) -> Result<()> {
        self.client.award_emoji(comment_id, reaction).await
    }

    async fn find_or_update_discussion(&self, body: &str) -> Result<String> {
        let bot_user_id = self.client.get_current_user_id().await?;
        let discussions = self.client.list_discussions().await?;

        for discussion in &discussions {
            for note in &discussion.notes {
                if note.author.id == bot_user_id && note.body.starts_with(BOT_DISCUSSION_TITLE) {
                    self.client.update_note(note.id, body).await?;
                    return Ok(note.id.to_string());
                }
            }
        }

        let id = self.client.post_note(body).await?;
        Ok(id.to_string())
    }

    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()> {
        let note_id: i64 = discussion_id.parse()?;
        self.client.update_note(note_id, body).await
    }
}
