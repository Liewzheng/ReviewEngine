//! GitLab implementation of the Publisher trait. Posts review results back to GitLab MR discussions and inline comments.
//!
//!
//! @module review-engine
use anyhow::Result;
use async_trait::async_trait;

use super::{InlineNote, Publisher};
use crate::gitlab::client::Client as GitLabClient;

const BOT_DISCUSSION_TITLE: &str = "# CodeReview Board";

/// GitLab implementation of Publisher.
pub struct GitLabPublisher {
    client: GitLabClient,
}

impl GitLabPublisher {
    pub fn new(gitlab_token: &str, mr_url: &str) -> Result<Self> {
        let client = GitLabClient::new(gitlab_token, mr_url)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Publisher for GitLabPublisher {
    async fn post_mr_discussion(&self, body: &str) -> Result<String> {
        let note_id = self.client.post_note(body).await?;
        Ok(note_id.to_string())
    }

    async fn post_inline_note(&self, note: &InlineNote) -> Result<()> {
        self.client.post_inline_note(&note.file, note.line, &note.body).await
    }

    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()> {
        let note_id: i64 = discussion_id.parse()?;
        self.client.update_note(note_id, body).await
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

        self.post_mr_discussion(body).await
    }
}
