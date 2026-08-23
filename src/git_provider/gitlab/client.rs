//! Low-level GitLab REST API client. Handles authentication, request dispatch, and response parsing for the GitLab API.
//!
//!
//! @module review-engine
use anyhow::{Context, Result};
use reqwest::Client as HttpClient;
use tracing::{error, info};

use crate::models::*;

/// GitLab REST API client for MR operations.
///
/// Handles authentication, request dispatch, and response parsing.
/// Supports fetching MR metadata, diff content, posting inline comments,
/// and managing MR approval state.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    /// GitLab API base URL (e.g. `https://gitlab.com/api/v4`).
    base_url: String,
    /// URL-encoded project path (e.g. `group%2Fproject`).
    project_path: String,
    /// Merge request internal ID (iid).
    mr_iid: u32,
    /// Private token or personal access token for authentication.
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

/// The parsed components of a GitLab merge request web URL.
///
/// Produced by [`Client::parse_mr_url`]; carries everything needed to build
/// the API base URL without constructing a client, so API handlers can
/// validate an MR URL synchronously at enqueue time.
#[derive(Debug, Clone)]
pub struct ParsedMrUrl {
    /// URL scheme (`http` or `https`), preserved from the input URL.
    pub scheme: String,
    /// Host with optional `:port` suffix (e.g. `gitlab.internal:8443`).
    pub host: String,
    /// Project path (e.g. `group/project`).
    pub project_path: String,
    /// Merge request internal ID (iid).
    pub mr_iid: u32,
}

impl ParsedMrUrl {
    /// GitLab REST API base URL derived from the MR URL, keeping the
    /// original scheme and port (e.g. `http://localhost:8929/api/v4`).
    pub fn base_url(&self) -> String {
        format!("{}://{}/api/v4", self.scheme, self.host)
    }
}

impl Client {
    /// Parse and validate a GitLab MR web URL into its API components.
    ///
    /// Accepts `http://` and `https://` (the scheme is preserved for the API
    /// base URL) and an optional numeric `:port` on the host, so self-hosted
    /// GitLab instances on plain HTTP and/or non-standard ports work. This
    /// is a pure parse — no network, no credential — so enqueue-path
    /// handlers use it to reject malformed URLs with 422 instead of failing
    /// inside the async review task.
    pub fn parse_mr_url(mr_url: &str) -> Result<ParsedMrUrl> {
        let (scheme, stripped) = mr_url
            .strip_prefix("https://")
            .map(|rest| ("https", rest))
            .or_else(|| mr_url.strip_prefix("http://").map(|rest| ("http", rest)))
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
        validate_gitlab_host(host, mr_url)?;
        if project_path.contains("..") || project_path.starts_with('/') || project_path.ends_with('/') {
            anyhow::bail!("Invalid GitLab project path in MR URL: {mr_url}");
        }

        let mr_iid: u32 = iid_str
            .parse()
            .with_context(|| format!("Failed to parse MR IID as integer: {iid_str}"))?;

        Ok(ParsedMrUrl {
            scheme: scheme.to_string(),
            host: host.to_string(),
            project_path: project_path.to_string(),
            mr_iid,
        })
    }

    pub fn new(gitlab_token: &str, mr_url: &str) -> Result<Self> {
        let parsed = Self::parse_mr_url(mr_url)?;

        let client = Self {
            http: HttpClient::new(),
            base_url: parsed.base_url(),
            project_path: parsed.project_path,
            mr_iid: parsed.mr_iid,
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
        let raw_url = format!(
            "{}/projects/{}/merge_requests/{}/raw_diffs",
            self.base_url, project, self.mr_iid
        );

        info!(url = %raw_url, "Fetching MR diff via raw_diffs");

        let resp = self
            .http
            .get(&raw_url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .with_context(|| format!("Failed to send GET {raw_url}"))?;

        if resp.status().is_success() {
            // raw_diffs returns the complete unified git diff as plain text
            // (including `diff --git` headers), so it can be returned as-is.
            return resp
                .text()
                .await
                .with_context(|| "Failed to read raw_diffs response body");
        }

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        info!(
            status = %status,
            body = %body,
            "raw_diffs unavailable, falling back to /changes"
        );

        self.fetch_diff_via_changes().await
    }

    /// Fallback diff fetch for GitLab instances without `raw_diffs`
    /// (pre-15.7): uses `GET .../merge_requests/:iid/changes` and joins the
    /// per-change `diff` fragments. Note these fragments are headerless
    /// patch bodies (starting at `@@`), so downstream parsers see no
    /// `diff --git` file headers.
    async fn fetch_diff_via_changes(&self) -> Result<String> {
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

    /// Fetch the raw content of a repository file at the given git ref.
    ///
    /// Uses `GET /projects/:id/repository/files/:path/raw?ref=:ref` (the same
    /// endpoint family as [`fetch_config_toml`](Self::fetch_config_toml)).
    pub async fn fetch_file_raw(&self, path: &str, git_ref: &str) -> Result<String> {
        validate_repo_file_path(path)?;
        let project = self.encoded_project_path();
        let url = format!(
            "{}/projects/{}/repository/files/{}/raw",
            self.base_url,
            project,
            encode_file_path(path),
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(&[("ref", git_ref)])
            .send()
            .await
            .with_context(|| format!("Failed to send GET {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, path = %path, "Failed to fetch file content");
            anyhow::bail!("GitLab API returned {status} for file '{path}': {body}");
        }

        resp.text()
            .await
            .with_context(|| format!("Failed to read file content response for '{path}'"))
    }

    /// Search the project's blobs for `query`, returning up to `limit`
    /// distinct matching file paths.
    ///
    /// Uses `GET /projects/:id/search?scope=blobs`.
    pub async fn search_code_paths(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        let project = self.encoded_project_path();
        let url = format!("{}/projects/{}/search", self.base_url, project);
        let per_page = limit.to_string();

        let resp = self
            .http
            .get(&url)
            .header("Authorization", self.auth_header())
            .query(&[("scope", "blobs"), ("search", query), ("per_page", per_page.as_str())])
            .send()
            .await
            .with_context(|| format!("Failed to send GET {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, "Failed to search blobs");
            anyhow::bail!("GitLab API returned {status} for blob search: {body}");
        }

        let value: serde_json::Value = resp
            .json()
            .await
            .with_context(|| "Failed to parse blob search response")?;
        Ok(parse_blob_search_paths(&value, limit))
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

/// Validate the host portion of a GitLab MR URL, allowing an optional
/// numeric `:port` suffix (1-65535) for self-hosted instances on
/// non-standard ports. Keeps the anti-traversal intent of the original
/// host check: the host must be non-empty and must not contain `/`, `..`,
/// or `@` (userinfo would smuggle a different effective host into the API
/// URL). IPv6 literals are rejected with an explicit message — bracketed
/// forms and bare multi-colon forms are both out of scope.
fn validate_gitlab_host(host: &str, mr_url: &str) -> Result<()> {
    if host.contains('@') {
        anyhow::bail!("Invalid GitLab host in MR URL (userinfo is not allowed): {mr_url}");
    }
    if host.starts_with('[') || host.matches(':').count() > 1 {
        anyhow::bail!("Invalid GitLab host in MR URL (IPv6 literals are not supported): {mr_url}");
    }
    let hostname = match host.split_once(':') {
        Some((hostname, port)) => {
            let valid = !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
                && port.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p));
            if !valid {
                anyhow::bail!("Invalid GitLab host port in MR URL (expected a numeric port in 1-65535): {mr_url}");
            }
            hostname
        }
        None => host,
    };
    if hostname.is_empty() || hostname.contains('/') || hostname.contains("..") {
        anyhow::bail!("Invalid GitLab host in MR URL: {mr_url}");
    }
    Ok(())
}

/// Defensive validation of a repository-relative file path before it is
/// embedded in an API URL, consistent with `post_inline_note`.
fn validate_repo_file_path(path: &str) -> Result<()> {
    if path.is_empty() || path.contains("..") || path.starts_with('/') || path.starts_with('~') {
        anyhow::bail!("Invalid repository file path: {path}");
    }
    Ok(())
}

/// Percent-encode a repository file path for use as a single URL path segment
/// (GitLab requires the full path, including slashes, to be URL-encoded).
fn encode_file_path(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Extract distinct file paths from a GitLab `scope=blobs` search response,
/// capped at `limit` entries.
fn parse_blob_search_paths(value: &serde_json::Value, limit: usize) -> Vec<String> {
    let mut paths = Vec::new();
    let Some(items) = value.as_array() else {
        return paths;
    };
    for item in items {
        if let Some(path) = item.get("path").and_then(|p| p.as_str()) {
            if !paths.iter().any(|p| p == path) {
                paths.push(path.to_string());
            }
        }
        if paths.len() >= limit {
            break;
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a client pointed at a wiremock server. Tests live in the same
    /// module, so private fields are accessible; a struct literal is used
    /// instead of `Client::new` so `base_url` is the mock server URI exactly
    /// (`Client::new` derives it from the MR URL with an `/api/v4` suffix,
    /// which would not match the wiremock path matchers below).
    fn make_test_client(server: &MockServer) -> Client {
        Client {
            http: HttpClient::new(),
            base_url: server.uri(),
            project_path: "group/project".to_string(),
            mr_iid: 1,
            gitlab_token: "test_token".to_string(),
        }
    }

    // ─── helpers ──────────────────────────────────

    // ─── parse_mr_url ─────────────────────────────

    #[test]
    fn test_parse_mr_url_accepts_plain_https() {
        let parsed = Client::parse_mr_url("https://gitlab.example.com/group/proj/-/merge_requests/1").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.host, "gitlab.example.com");
        assert_eq!(parsed.project_path, "group/proj");
        assert_eq!(parsed.mr_iid, 1);
        assert_eq!(parsed.base_url(), "https://gitlab.example.com/api/v4");
    }

    #[test]
    fn test_parse_mr_url_accepts_http_with_explicit_port() {
        // Self-hosted GitLab EE testbed shape: plain HTTP + explicit port.
        let url = "http://localhost:8929/review-lab/demo-app/-/merge_requests/1";
        let parsed = Client::parse_mr_url(url).unwrap();
        assert_eq!(parsed.scheme, "http");
        assert_eq!(parsed.host, "localhost:8929");
        assert_eq!(parsed.project_path, "review-lab/demo-app");
        assert_eq!(parsed.mr_iid, 1);
        assert_eq!(parsed.base_url(), "http://localhost:8929/api/v4");

        let client = Client::new("test_token", url).unwrap();
        assert_eq!(client.base_url, "http://localhost:8929/api/v4");
        assert_eq!(client.project_path, "review-lab/demo-app");
        assert_eq!(client.mr_iid, 1);
    }

    #[test]
    fn test_parse_mr_url_accepts_https_with_explicit_port() {
        let parsed = Client::parse_mr_url("https://gitlab.internal:8443/g/p/-/merge_requests/7").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.host, "gitlab.internal:8443");
        assert_eq!(parsed.project_path, "g/p");
        assert_eq!(parsed.mr_iid, 7);
        assert_eq!(parsed.base_url(), "https://gitlab.internal:8443/api/v4");
    }

    #[test]
    fn test_parse_mr_url_rejects_userinfo_host() {
        for url in [
            "https://user@gitlab.example.com/g/p/-/merge_requests/1",
            "https://user:pass@gitlab.example.com/g/p/-/merge_requests/1",
        ] {
            let err = Client::parse_mr_url(url).unwrap_err();
            assert!(
                err.to_string().contains("userinfo"),
                "url {url} must fail with a userinfo error, got: {err}"
            );
        }
    }

    #[test]
    fn test_parse_mr_url_rejects_invalid_ports() {
        // Empty, zero, non-numeric, and out-of-range ports are all rejected.
        for host in [
            "localhost:",
            "localhost:0",
            "localhost:abc",
            "localhost:99999",
            "localhost:65536",
            "localhost:99999999999999999999",
        ] {
            let url = format!("http://{host}/g/p/-/merge_requests/1");
            let err = Client::parse_mr_url(&url).unwrap_err();
            assert!(
                err.to_string().contains("port"),
                "url {url} must fail with a port error, got: {err}"
            );
        }
        // Boundaries: 1 and 65535 remain valid.
        assert!(Client::parse_mr_url("http://localhost:1/g/p/-/merge_requests/1").is_ok());
        assert!(Client::parse_mr_url("http://localhost:65535/g/p/-/merge_requests/1").is_ok());
    }

    #[test]
    fn test_parse_mr_url_rejects_dotdot_in_host() {
        for url in [
            "https://git..lab.example.com/g/p/-/merge_requests/1",
            "https://../g/p/-/merge_requests/1",
        ] {
            let err = Client::parse_mr_url(url).unwrap_err();
            assert!(
                err.to_string().contains("Invalid GitLab host"),
                "url {url} must fail with a host error, got: {err}"
            );
        }
    }

    #[test]
    fn test_parse_mr_url_rejects_ipv6_literals() {
        for url in [
            "http://[::1]:8929/g/p/-/merge_requests/1",
            "http://[fe80::1]/g/p/-/merge_requests/1",
            "http://::1/g/p/-/merge_requests/1",
        ] {
            let err = Client::parse_mr_url(url).unwrap_err();
            assert!(
                err.to_string().contains("IPv6"),
                "url {url} must fail with an IPv6 error, got: {err}"
            );
        }
    }

    #[test]
    fn test_parse_mr_url_rejects_bad_iid() {
        for url in [
            "https://gitlab.example.com/g/p/-/merge_requests/abc",
            "https://gitlab.example.com/g/p/-/merge_requests/",
            "https://gitlab.example.com/g/p/-/merge_requests/1.5",
        ] {
            let err = Client::parse_mr_url(url).unwrap_err();
            assert!(
                err.to_string().contains("MR IID"),
                "url {url} must fail with an iid error, got: {err}"
            );
        }
    }

    #[test]
    fn test_parse_mr_url_rejects_missing_scheme_and_empty_host() {
        let err = Client::parse_mr_url("not-a-valid-url").unwrap_err();
        assert!(err.to_string().contains("no scheme"), "unexpected error: {err}");

        let err = Client::parse_mr_url("https://gitlab.example.com/g/p").unwrap_err();
        assert!(
            err.to_string().contains("/-/merge_requests/"),
            "unexpected error: {err}"
        );

        let err = Client::parse_mr_url("https:///g/p/-/merge_requests/1").unwrap_err();
        assert!(
            err.to_string().contains("Invalid GitLab host"),
            "empty host must be rejected, got: {err}"
        );
    }

    #[test]
    fn test_encode_file_path() {
        assert_eq!(encode_file_path("src/main.rs"), "src%2Fmain.rs");
        assert_eq!(encode_file_path("a b/c#.rs"), "a%20b%2Fc%23.rs");
        assert_eq!(encode_file_path("plain.rs"), "plain.rs");
    }

    #[test]
    fn test_validate_repo_file_path() {
        assert!(validate_repo_file_path("src/main.rs").is_ok());
        assert!(validate_repo_file_path("../secret").is_err());
        assert!(validate_repo_file_path("/etc/passwd").is_err());
        assert!(validate_repo_file_path("~/key").is_err());
        assert!(validate_repo_file_path("").is_err());
    }

    #[test]
    fn test_parse_blob_search_paths_dedups_and_caps() {
        let value = json!([
            {"path": "src/a.rs", "data": "..."},
            {"path": "src/b.rs", "data": "..."},
            {"path": "src/a.rs", "data": "..."},
            {"path": "src/c.rs", "data": "..."}
        ]);
        assert_eq!(
            parse_blob_search_paths(&value, 20),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string(), "src/c.rs".to_string()]
        );
        assert_eq!(
            parse_blob_search_paths(&value, 2),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
    }

    #[test]
    fn test_parse_blob_search_paths_tolerates_garbage() {
        assert!(parse_blob_search_paths(&json!({"unexpected": true}), 20).is_empty());
        assert!(parse_blob_search_paths(&json!([{"no_path": 1}]), 20).is_empty());
    }

    #[test]
    fn test_encode_project_path_escapes_each_slash() {
        assert_eq!(encode_project_path("group/project"), "group%2Fproject");
        assert_eq!(encode_project_path("a/b/c"), "a%2Fb%2Fc");
        assert_eq!(encode_project_path("single"), "single");
        assert_eq!(encode_project_path(""), "");
    }

    #[test]
    fn test_encode_file_path_preserves_safe_chars_and_encodes_rest() {
        assert_eq!(encode_file_path("src/main.rs"), "src%2Fmain.rs");
        assert_eq!(encode_file_path("a b/c#.rs"), "a%20b%2Fc%23.rs");
        assert_eq!(encode_file_path("plain.rs"), "plain.rs");
        // Unreserved chars stay; spaces, slashes and non-ASCII are percent-encoded.
        assert_eq!(encode_file_path("_~-."), "_~-.");
        assert_eq!(encode_file_path("x y"), "x%20y");
    }

    #[test]
    fn test_validate_repo_file_path_rejects_traversal_forms() {
        assert!(validate_repo_file_path("src/main.rs").is_ok());
        assert!(validate_repo_file_path("a/b/c.rs").is_ok());
        assert!(validate_repo_file_path("..").is_err());
        assert!(validate_repo_file_path("../secret").is_err());
        assert!(validate_repo_file_path("a/../b").is_err());
        assert!(validate_repo_file_path("/etc/passwd").is_err());
        assert!(validate_repo_file_path("~/key").is_err());
        assert!(validate_repo_file_path("").is_err());
    }

    #[test]
    fn test_parse_blob_search_paths_skips_missing_path_and_non_string() {
        let value = json!([
            {"path": "src/a.rs"},
            {"path": 42},
            {},
            {"path": "src/b.rs"}
        ]);
        assert_eq!(
            parse_blob_search_paths(&value, 20),
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
    }

    #[test]
    fn test_parse_blob_search_paths_limit_zero_still_yields_first() {
        // The cap is checked AFTER pushing, so limit 0 behaves like limit 1:
        // at least one path is returned when any exists.
        let value = json!([{"path": "src/a.rs"}]);
        assert_eq!(parse_blob_search_paths(&value, 0), vec!["src/a.rs".to_string()]);
        let value = json!([{"path": "src/a.rs"}, {"path": "src/b.rs"}]);
        assert_eq!(parse_blob_search_paths(&value, 1), vec!["src/a.rs".to_string()]);
    }

    // ─── fetch_diff ───────────────────────────────

    const RAW_DIFF_BODY: &str = "diff --git a/src/main.rs b/src/main.rs\nindex 1111111..2222222 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,2 +1,2 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n }\n";

    #[tokio::test]
    async fn test_fetch_diff_raw_diffs_returns_body_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/merge_requests/1/raw_diffs"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RAW_DIFF_BODY))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let diff = client.fetch_diff().await.unwrap();
        assert_eq!(diff, RAW_DIFF_BODY);
        assert!(diff.contains("diff --git a/src/main.rs b/src/main.rs"));
    }

    #[tokio::test]
    async fn test_fetch_diff_falls_back_to_changes_on_raw_diffs_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/merge_requests/1/raw_diffs"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"404 Not Found\"}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/merge_requests/1/changes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "changes": [
                    {"diff": "@@ -1,2 +1,2 @@\n fn main() {\n-old\n+new\n }"},
                    {"diff": "@@ -1 +1 @@\n-a\n+b"}
                ]
            })))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let diff = client.fetch_diff().await.unwrap();
        assert_eq!(
            diff,
            "@@ -1,2 +1,2 @@\n fn main() {\n-old\n+new\n }\n@@ -1 +1 @@\n-a\n+b"
        );
    }

    #[tokio::test]
    async fn test_fetch_diff_errors_when_raw_diffs_and_changes_both_fail() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/merge_requests/1/raw_diffs"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"404 Not Found\"}"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/merge_requests/1/changes"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let err = client.fetch_diff().await.unwrap_err();
        assert!(err.to_string().contains("500"), "error should mention 500, got: {err}");
    }

    // ─── fetch_file_raw ───────────────────────────

    #[tokio::test]
    async fn test_fetch_file_raw_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/repository/files/src%2Fmain.rs/raw"))
            .and(query_param("ref", "main"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fn main() {}\n"))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let content = client.fetch_file_raw("src/main.rs", "main").await.unwrap();
        assert_eq!(content, "fn main() {}\n");
    }

    #[tokio::test]
    async fn test_fetch_file_raw_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/repository/files/src%2Fmissing.rs/raw"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"message\":\"404 File Not Found\"}"))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let err = client.fetch_file_raw("src/missing.rs", "main").await.unwrap_err();
        assert!(err.to_string().contains("404"), "error should mention 404, got: {err}");
    }

    #[tokio::test]
    async fn test_fetch_file_raw_rejects_unsafe_path() {
        let server = MockServer::start().await;
        let client = make_test_client(&server);
        assert!(client.fetch_file_raw("../secret", "main").await.is_err());
    }

    // ─── search_code_paths ────────────────────────

    #[tokio::test]
    async fn test_search_code_paths_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/search"))
            .and(query_param("scope", "blobs"))
            .and(query_param("search", "authenticate"))
            .and(query_param("per_page", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"basename": "auth.rs", "path": "src/auth.rs", "data": "fn authenticate()"},
                {"basename": "login.rs", "path": "src/login.rs", "data": "authenticate()"}
            ])))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let paths = client.search_code_paths("authenticate", 20).await.unwrap();
        assert_eq!(paths, vec!["src/auth.rs".to_string(), "src/login.rs".to_string()]);
    }

    #[tokio::test]
    async fn test_search_code_paths_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fproject/search"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = make_test_client(&server);
        let err = client.search_code_paths("x", 20).await.unwrap_err();
        assert!(err.to_string().contains("401"), "error should mention 401, got: {err}");
    }
}
