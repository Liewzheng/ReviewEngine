//! Low-level GitLab REST API client. Handles authentication, request dispatch, and response parsing for the GitLab API.
//!
//!
//! @module review-engine
use anyhow::{Context, Result};
use reqwest::Client as HttpClient;
use tracing::{error, info};

use crate::models::*;

#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    base_url: String,
    project_path: String,
    mr_iid: u32,
    gitlab_token: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("project_path", &self.project_path)
            .field("mr_iid", &self.mr_iid)
            .field("gitlab_token", &"***")
            .finish()
    }
}

impl Client {
    pub fn new(gitlab_token: &str, mr_url: &str) -> Result<Self> {
        let stripped = mr_url
            .strip_prefix("https://")
            .or_else(|| mr_url.strip_prefix("http://"))
            .with_context(|| format!("Invalid MR URL format (no scheme): {mr_url}"))?;

        let sep = "/-/merge_requests/";
        let sep_idx = stripped
            .rfind(sep)
            .with_context(|| format!("Invalid MR URL format (missing '/-/merge_requests/'): {mr_url}"))?;

        let host_and_path = &stripped[..sep_idx];
        let iid_str = &stripped[sep_idx + sep.len()..];

        let slash_idx = host_and_path
            .find('/')
            .with_context(|| format!("Invalid MR URL format (no host/path separator): {mr_url}"))?;

        let host = &host_and_path[..slash_idx];
        let project_path = &host_and_path[slash_idx + 1..];

        // Validate host and project_path to prevent path traversal / command injection
        if host.is_empty() || host.contains('/') || host.contains("..") || host.contains(':') {
            anyhow::bail!("Invalid GitLab host in MR URL: {mr_url}");
        }
        if project_path.contains("..") || project_path.starts_with('/') || project_path.ends_with('/') {
            anyhow::bail!("Invalid GitLab project path in MR URL: {mr_url}");
        }

        let mr_iid: u32 = iid_str
            .parse()
            .with_context(|| format!("Failed to parse MR IID as integer: {iid_str}"))?;

        let base_url = format!("https://{host}/api/v4");
        let http = HttpClient::new();

        let client = Self {
            http,
            base_url,
            project_path: project_path.to_string(),
            mr_iid,
            gitlab_token: gitlab_token.to_string(),
        };

        info!(
            path = %client.project_path,
            iid = client.mr_iid,
            "GitLab client initialized"
        );

        Ok(client)
    }

    fn encoded_project_path(&self) -> String {
        encode_project_path(&self.project_path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.gitlab_token)
    }

    /// Get the authenticated user's GitLab user ID.
    /// Uses the raw /user endpoint (not scoped to a project).
    pub async fn get_current_user_id(&self) -> Result<u64> {
        let url = format!("{}/user", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header())
            .send()
            .await
            .with_context(|| "Failed to send GET /user")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab API returned {status} for GET /user: {text}");
        }

        let value: serde_json::Value = resp.json().await.with_context(|| "Failed to parse /user response")?;

        let id = value["id"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Failed to parse user ID from /user response"))?;
        Ok(id)
    }

    /// Send a GET request to the GitLab API and return the JSON response.
    async fn get_json(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!(
            "{}/projects/{}/{}",
            self.base_url.trim_end_matches('/'),
            self.encoded_project_path(),
            path,
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .with_context(|| format!("Failed to send GET to {path}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab API returned {status} for GET {path}: {text}");
        }

        resp.json()
            .await
            .with_context(|| format!("Failed to parse response from {path}"))
    }

    /// Send a POST request to the GitLab API and return the JSON response.
    async fn post_json<T: serde::Serialize>(&self, path: &str, body: &T) -> anyhow::Result<serde_json::Value> {
        let url = format!(
            "{}/projects/{}/{}",
            self.base_url.trim_end_matches('/'),
            self.encoded_project_path(),
            path,
        );

        let resp = self
            .http
            .post(&url)
            .header("PRIVATE-TOKEN", &self.gitlab_token)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .with_context(|| format!("Failed to send POST to {path}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab API returned {status} for POST {path}: {text}");
        }

        resp.json()
            .await
            .with_context(|| format!("Failed to parse response from {path}"))
    }

    pub async fn fetch_mr_info(&self) -> Result<MRInfo> {
        let project = self.encoded_project_path();
        let url = format!("{}/projects/{}/merge_requests/{}", self.base_url, project, self.mr_iid);

        info!(url = %url, "Fetching MR info");

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .with_context(|| format!("Failed to send GET {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Failed to fetch MR info");
            anyhow::bail!("GitLab API returned {status}: {body}");
        }

        #[derive(serde::Deserialize)]
        struct GitLabMRResponse {
            title: String,
            description: Option<String>,
            source_branch: String,
            target_branch: String,
            diff_refs: Option<DiffRefs>,
        }

        #[derive(serde::Deserialize)]
        struct DiffRefs {
            base_sha: Option<String>,
            head_sha: Option<String>,
            start_sha: Option<String>,
        }

        let gl: GitLabMRResponse = resp.json().await.context("Failed to parse MR info JSON response")?;

        let diff_refs = gl.diff_refs;
        let git_hash = diff_refs.as_ref().and_then(|d| d.head_sha.clone()).unwrap_or_default();
        let base_sha = diff_refs.as_ref().and_then(|d| d.base_sha.clone());
        let start_sha = diff_refs.as_ref().and_then(|d| d.start_sha.clone());

        Ok(MRInfo {
            project_path: self.project_path.clone(),
            mr_iid: self.mr_iid,
            title: gl.title,
            description: gl.description.unwrap_or_default(),
            source_branch: gl.source_branch,
            target_branch: gl.target_branch,
            git_hash,
            base_sha,
            start_sha,
            merge_commit_sha: None,
            pr_author: None,
            pr_author_id: None,
        })
    }

    pub async fn fetch_diff(&self) -> Result<String> {
        let project = self.encoded_project_path();
        let url = format!(
            "{}/projects/{}/merge_requests/{}/changes",
            self.base_url, project, self.mr_iid
        );

        info!(url = %url, "Fetching MR diff");

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .with_context(|| format!("Failed to send GET {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Failed to fetch MR diff");
            anyhow::bail!("GitLab API returned {status}: {body}");
        }

        #[derive(serde::Deserialize)]
        struct GitLabChangesResponse {
            changes: Vec<Change>,
        }

        #[derive(serde::Deserialize)]
        struct Change {
            diff: String,
        }

        let changes: GitLabChangesResponse = resp.json().await.context("Failed to parse MR changes JSON response")?;

        let raw: Vec<String> = changes.changes.into_iter().map(|c| c.diff).collect();
        Ok(raw.join("\n"))
    }

    pub async fn fetch_config_toml(&self) -> Result<Option<String>> {
        let project = self.encoded_project_path();
        let url = format!(
            "{}/projects/{}/repository/files/.code-audit-config.toml/raw",
            self.base_url, project,
        );

        info!("Fetching .code-audit-config.toml from repo root");

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(&[("ref", "HEAD")])
            .send()
            .await
            .with_context(|| format!("Failed to send GET {url}"))?;

        if resp.status().as_u16() == 404 {
            info!("No .code-audit-config.toml found in repository");
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Failed to fetch .code-audit-config.toml");
            anyhow::bail!("HTTP {} when fetching .code-audit-config.toml: {}", status, body);
        }

        let content = resp
            .text()
            .await
            .context("Failed to read .code-audit-config.toml response body")?;

        if content.is_empty() {
            return Ok(None);
        }

        info!("Successfully fetched .code-audit-config.toml");
        Ok(Some(content))
    }

    pub async fn post_comment(&self, body: &str) -> Result<()> {
        let project = self.encoded_project_path();
        let url = format!(
            "{}/projects/{}/merge_requests/{}/notes",
            self.base_url, project, self.mr_iid
        );

        info!("Posting comment to MR !{}", self.mr_iid);

        #[derive(serde::Serialize)]
        struct NoteBody<'a> {
            body: &'a str,
        }

        let resp = self
            .http
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&NoteBody { body })
            .send()
            .await
            .with_context(|| format!("Failed to send POST {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            error!(status = %status, body = %resp_body, "Failed to post comment");
            anyhow::bail!("GitLab API returned {status}: {resp_body}");
        }

        info!("Comment posted successfully");
        Ok(())
    }

    pub async fn delete_comment(&self, note_id: i64) -> Result<()> {
        let project = self.encoded_project_path();
        let url = format!(
            "{}/projects/{}/merge_requests/{}/notes/{}",
            self.base_url, project, self.mr_iid, note_id
        );

        info!(note_id = note_id, "Deleting comment from MR !{}", self.mr_iid);

        let resp = self
            .http
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .with_context(|| format!("Failed to send DELETE {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Failed to delete comment");
            anyhow::bail!("GitLab API returned {status}: {body}");
        }

        info!(note_id = note_id, "Comment deleted successfully");
        Ok(())
    }

    /// Update an existing note's body using the GitLab PUT API.
    pub async fn update_note(&self, note_id: i64, body: &str) -> Result<()> {
        let project = self.encoded_project_path();
        let url = format!(
            "{}/projects/{}/merge_requests/{}/notes/{}",
            self.base_url, project, self.mr_iid, note_id
        );

        info!(note_id = note_id, "Updating note on MR !{}", self.mr_iid);

        let payload = serde_json::json!({ "body": body });
        let resp = self
            .http
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to send PUT {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab API returned {status}: {text}");
        }

        info!(note_id = note_id, "Note updated successfully");
        Ok(())
    }

    /// List all discussions on this MR.
    pub async fn list_discussions(&self) -> Result<Vec<Discussion>> {
        let value = self
            .get_json(&format!("merge_requests/{}/discussions?per_page=100", self.mr_iid))
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Post a note (review comment) and return its GitLab note ID.
    pub async fn post_note(&self, body: &str) -> Result<i64> {
        info!("Posting note to MR !{}", self.mr_iid);

        let body = serde_json::json!({ "body": body });
        let value = self
            .post_json(&format!("merge_requests/{}/notes", self.mr_iid), &body)
            .await?;

        #[derive(serde::Deserialize)]
        struct NoteResponse {
            id: i64,
        }

        let note: NoteResponse = serde_json::from_value(value)?;
        info!(note_id = note.id, "Note posted successfully");
        Ok(note.id)
    }

    /// Post an inline comment (discussion) on a specific file and line.
    pub async fn post_inline_note(&self, file: &str, line: u32, body: &str) -> Result<()> {
        // Defensive: validate file path to prevent API abuse from hallucinated paths
        if file.contains("..") || file.starts_with('/') || file.starts_with('~') {
            anyhow::bail!("Invalid file path for inline comment: {}", file);
        }
        // Fetch MR info to obtain the SHA refs for the position
        let mr_info = self.fetch_mr_info().await?;
        let head_sha = &mr_info.git_hash;
        let base_sha = mr_info
            .base_sha
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("base_sha is required for inline comments"))?;
        let start_sha = mr_info
            .start_sha
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("start_sha is required for inline comments"))?;

        info!(
            file = %file,
            line = line,
            "Posting inline note to MR !{}", self.mr_iid
        );

        #[derive(serde::Serialize)]
        struct Position<'a> {
            position_type: &'a str,
            new_path: &'a str,
            new_line: u32,
            base_sha: &'a str,
            start_sha: &'a str,
            head_sha: &'a str,
        }

        #[derive(serde::Serialize)]
        struct DiscussionBody<'a> {
            body: &'a str,
            position: Position<'a>,
        }

        let discussion_body = DiscussionBody {
            body,
            position: Position {
                position_type: "text",
                new_path: file,
                new_line: line,
                base_sha,
                start_sha,
                head_sha,
            },
        };

        self.post_json(&format!("merge_requests/{}/discussions", self.mr_iid), &discussion_body)
            .await?;

        info!("Inline note posted successfully");
        Ok(())
    }

    /// Add a reaction (award emoji) to a note/comment.
    pub async fn award_emoji(&self, comment_id: i64, reaction: &str) -> Result<()> {
        info!(
            note_id = comment_id,
            reaction = %reaction,
            "Adding reaction to note on MR !{}", self.mr_iid
        );

        let body = serde_json::json!({ "name": reaction });
        self.post_json(
            &format!("merge_requests/{}/notes/{}/award_emoji", self.mr_iid, comment_id),
            &body,
        )
        .await?;

        info!(
            note_id = comment_id,
            reaction = %reaction,
            "Reaction added successfully"
        );
        Ok(())
    }
}

/// A GitLab MR discussion thread.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Discussion {
    pub notes: Vec<DiscussionNote>,
}

/// A single note within a discussion.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DiscussionNote {
    pub id: i64,
    pub body: String,
    pub author: NoteAuthor,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NoteAuthor {
    pub id: u64,
}

fn encode_project_path(path: &str) -> String {
    path.replace('/', "%2F")
}
