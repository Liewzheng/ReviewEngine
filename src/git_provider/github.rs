use anyhow::Result;
use async_trait::async_trait;

use crate::git_provider::GitProvider;
use crate::github::client::Client as GitHubClient;
use crate::models::MRInfo;

/// GitHub implementation of GitProvider.
pub struct GitHubProvider {
    client: GitHubClient,
}

impl GitHubProvider {
    pub fn new(token: &str, pr_url: &str) -> Result<Self> {
        let client = GitHubClient::new(token, pr_url)?;
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
        Ok(())
    }
}
