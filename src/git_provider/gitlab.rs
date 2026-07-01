//! GitLab implementation of the GitProvider trait. Communicates with GitLab API for MR data and review comments.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//!
//! @module review-engine
use anyhow::Result;
use async_trait::async_trait;

use crate::git_provider::GitProvider;
use crate::gitlab::client::Client as GitLabClient;
use crate::models::MRInfo;

/// GitLab implementation of GitProvider.
pub struct GitLabProvider {
    client: GitLabClient,
}

impl GitLabProvider {
    pub fn new(gitlab_token: &str, mr_url: &str) -> Result<Self> {
        let client = GitLabClient::new(gitlab_token, mr_url)?;
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
}
