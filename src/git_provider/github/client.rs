use anyhow::{Context, Result};
use reqwest::Client as HttpClient;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info};

use crate::models::MRInfo;

use super::types::{GitHubUser, PrReview, PullRequest, ReviewComment};

/// GitHub REST API client.
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
    api_base: String,
    owner: String,
    repo: String,
    pr_number: u32,
    token: String,
    commit_sha: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubClient")
            .field("owner", &self.owner)
            .field("repo", &self.repo)
            .field("pr_number", &self.pr_number)
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl Client {
    /// Create a new GitHub client from a PR URL and personal access token.
    ///
    /// Supported URL formats:
    /// - `https://github.com/owner/repo/pull/123`
    /// - `https://github.com/owner/repo/pull/123/files`
    pub fn new(token: &str, pr_url: &str) -> Result<Self> {
        let stripped = pr_url
            .strip_prefix("https://github.com/")
            .or_else(|| pr_url.strip_prefix("http://github.com/"))
            .ok_or_else(|| anyhow::anyhow!("Invalid GitHub PR URL: {pr_url}"))?;

        let parts: Vec<&str> = stripped.trim_end_matches('/').split('/').collect();
        if parts.len() < 4 || parts[2] != "pull" {
            anyhow::bail!("Invalid GitHub PR URL format: expected .../owner/repo/pull/<number>");
        }

        let owner = parts[0];
        let repo = parts[1];
        let pr_number: u32 = parts[3]
            .parse()
            .with_context(|| format!("Failed to parse PR number from URL: {pr_url}"))?;

        // Validate owner and repo to prevent path traversal / command injection
        if owner.is_empty() || owner.contains("..") || owner.contains('/') || owner.contains(':') {
            anyhow::bail!("Invalid GitHub owner in PR URL: {pr_url}");
        }
        if repo.is_empty() || repo.contains("..") || repo.contains('/') || repo.contains(':') {
            anyhow::bail!("Invalid GitHub repo in PR URL: {pr_url}");
        }

        Ok(Self {
            http: HttpClient::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .with_context(|| "Failed to create HTTP client")?,
            api_base: "https://api.github.com".to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            pr_number,
            token: token.to_string(),
            commit_sha: Arc::new(Mutex::new(None)),
        })
    }

    /// Build a GitHub API URL for the given path.
    fn api_url(&self, path: &str) -> String {
        format!("{}/repos/{}/{}/{}", self.api_base, self.owner, self.repo, path)
    }

    /// Common headers for all GitHub API requests.
    fn headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github.v3+json"));
        headers.insert(
            AUTHORIZATION,
            #[allow(clippy::expect_used)]
            HeaderValue::from_str(&format!("Bearer {}", self.token)).expect("Bearer token is a valid header value"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("review-engine/0.6"));
        headers
    }

    /// Expose the underlying HTTP client for pagination helpers.
    pub fn get_http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Expose authentication headers for pagination helpers.
    pub fn get_headers(&self) -> reqwest::header::HeaderMap {
        self.headers()
    }

    /// Fetch PR information.
    pub async fn fetch_pr_info(&self) -> Result<MRInfo> {
        info!("Fetching PR #{} from {}/{}", self.pr_number, self.owner, self.repo);
        let url = self.api_url(&format!("pulls/{}", self.pr_number));
        let resp = self
            .http
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .with_context(|| format!("Failed to fetch PR #{0}", self.pr_number))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, "Failed to fetch PR info");
            anyhow::bail!("GitHub API returned {status}: {body}");
        }

        #[allow(clippy::unwrap_used)]
        let pr: PullRequest = resp.json().await.with_context(|| "Failed to parse PR response")?;

        if let Ok(mut sha) = self.commit_sha.lock() {
            *sha = Some(pr.head.sha.clone());
        }

        Ok(MRInfo {
            project_path: format!("{}/{}", self.owner, self.repo),
            mr_iid: pr.number,
            title: pr.title,
            description: pr.body.unwrap_or_default(),
            source_branch: pr.head.ref_name,
            target_branch: pr.base.ref_name,
            git_hash: pr.head.sha,
            base_sha: Some(pr.base.sha),
            start_sha: None,
            merge_commit_sha: pr.merge_commit_sha,
            pr_author: Some(pr.user.login),
            pr_author_id: Some(pr.user.id),
            discussion_context: None,
        })
    }

    /// Fetch the PR diff as a raw diff string.
    pub async fn fetch_diff(&self) -> Result<String> {
        info!("Fetching diff for PR #{}", self.pr_number);
        let url = self.api_url(&format!("pulls/{}", self.pr_number));
        let resp = self
            .http
            .get(&url)
            .headers({
                let mut h = self.headers();
                h.insert(
                    reqwest::header::ACCEPT,
                    reqwest::header::HeaderValue::from_static("application/vnd.github.v3.diff"),
                );
                h
            })
            .send()
            .await
            .with_context(|| format!("Failed to fetch diff for PR #{}", self.pr_number))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, "Failed to fetch diff");
            anyhow::bail!("GitHub API returned {status}: {body}");
        }

        resp.text().await.with_context(|| "Failed to read diff response body")
    }

    /// Post a top-level PR review comment (pull request review).
    pub async fn create_pr_review(&self, body: &str) -> Result<i64> {
        info!("Posting PR review on #{}", self.pr_number);
        let url = self.api_url(&format!("pulls/{}/reviews", self.pr_number));
        let payload = serde_json::json!({
            "body": body,
            "event": "COMMENT",
        });
        let resp = self
            .http
            .post(&url)
            .headers(self.headers())
            .json(&payload)
            .send()
            .await
            .with_context(|| "Failed to post PR review")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API returned {status} for POST review: {text}");
        }

        #[derive(Deserialize)]
        struct ReviewResponse {
            id: i64,
        }
        let review: ReviewResponse = resp.json().await?;
        info!(review_id = review.id, "PR review posted");
        Ok(review.id)
    }

    /// Create an inline review comment on a specific file/line.
    pub async fn create_review_comment(&self, file: &str, line: u32, body: &str) -> Result<()> {
        // Defensive: validate file path to prevent API abuse from hallucinated paths
        if file.contains("..") || file.starts_with('/') || file.starts_with('~') {
            anyhow::bail!("Invalid file path for review comment: {}", file);
        }
        info!("Posting inline comment on {}:{} in PR #{}", file, line, self.pr_number);
        let url = self.api_url(&format!("pulls/{}/comments", self.pr_number));
        let mut payload = serde_json::json!({
            "body": body,
            "path": file,
            "line": line,
            "side": "RIGHT",
        });
        if let Ok(sha) = self.commit_sha.lock() {
            if let Some(ref sha) = *sha {
                payload["commit_id"] = serde_json::json!(sha);
            }
        }
        let resp = self
            .http
            .post(&url)
            .headers(self.headers())
            .json(&payload)
            .send()
            .await
            .with_context(|| "Failed to post inline comment")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API returned {status} for POST comment: {text}");
        }
        info!("Inline comment posted");
        Ok(())
    }

    /// List all review comments on the PR (paginated).
    pub async fn list_review_comments(&self) -> Result<Vec<ReviewComment>> {
        let url = self.api_url(&format!("pulls/{}/comments?per_page=100", self.pr_number));
        super::pagination::get_all_paginated(self, &url, 5).await
    }

    /// Update an existing review comment.
    pub async fn update_review_comment(&self, comment_id: i64, body: &str) -> Result<()> {
        let url = self.api_url(&format!("pulls/comments/{}", comment_id));
        let payload = serde_json::json!({ "body": body });
        let resp = self
            .http
            .patch(&url)
            .headers(self.headers())
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to update comment {comment_id}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API returned {status} for PATCH comment: {text}");
        }
        info!(comment_id = comment_id, "Review comment updated");
        Ok(())
    }

    /// List all PR reviews (top-level reviews, not inline comments).
    /// Each review has an id and a body.
    pub async fn list_pr_reviews(&self) -> Result<Vec<PrReview>> {
        let url = self.api_url(&format!("pulls/{}/reviews?per_page=100", self.pr_number));
        super::pagination::get_all_paginated(self, &url, 5).await
    }

    /// Update the body of an existing PR review (top-level review).
    pub async fn update_pr_review(&self, review_id: i64, body: &str) -> Result<()> {
        let url = self.api_url(&format!("pulls/{}/reviews/{}", self.pr_number, review_id));
        let payload = serde_json::json!({ "body": body });
        let resp = self
            .http
            .put(&url)
            .headers(self.headers())
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("Failed to update review {review_id}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API returned {status} for PUT review: {text}");
        }
        info!(review_id = review_id, "PR review updated");
        Ok(())
    }

    /// Create a test client pointing at a custom API base (e.g. wiremock).
    #[cfg(test)]
    pub fn new_test(token: &str, pr_url: &str, api_base: &str) -> Result<Self> {
        let stripped = pr_url
            .strip_prefix("https://github.com/")
            .or_else(|| pr_url.strip_prefix("http://github.com/"))
            .ok_or_else(|| anyhow::anyhow!("Invalid GitHub PR URL: {pr_url}"))?;

        let parts: Vec<&str> = stripped.trim_end_matches('/').split('/').collect();
        if parts.len() < 4 || parts[2] != "pull" {
            anyhow::bail!("Invalid GitHub PR URL format: expected .../owner/repo/pull/<number>");
        }

        let owner = parts[0].to_string();
        let repo = parts[1].to_string();
        let pr_number: u32 = parts[3]
            .parse()
            .with_context(|| format!("Failed to parse PR number from URL: {pr_url}"))?;

        Ok(Self {
            http: HttpClient::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .with_context(|| "Failed to create HTTP client")?,
            api_base: api_base.to_string(),
            owner,
            repo,
            pr_number,
            token: token.to_string(),
            commit_sha: Arc::new(Mutex::new(None)),
        })
    }

    /// Fetch the raw content of a repository file at the given git ref.
    ///
    /// Uses `GET /repos/:owner/:repo/contents/:path?ref=:ref` with the
    /// `application/vnd.github.raw+json` media type so the response body is
    /// the file content itself.
    pub async fn fetch_file_raw(&self, path: &str, git_ref: &str) -> Result<String> {
        // Defensive: validate file path, consistent with create_review_comment
        if path.is_empty() || path.contains("..") || path.starts_with('/') || path.starts_with('~') {
            anyhow::bail!("Invalid repository file path: {path}");
        }
        let url = self.api_url(&format!("contents/{}", encode_content_path(path)));
        let resp = self
            .http
            .get(&url)
            .headers({
                let mut h = self.headers();
                h.insert(
                    reqwest::header::ACCEPT,
                    reqwest::header::HeaderValue::from_static("application/vnd.github.raw+json"),
                );
                h
            })
            .query(&[("ref", git_ref)])
            .send()
            .await
            .with_context(|| format!("Failed to fetch file '{path}'"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, path = %path, "Failed to fetch file content");
            anyhow::bail!("GitHub API returned {status} for file '{path}': {body}");
        }

        resp.text()
            .await
            .with_context(|| format!("Failed to read file content response for '{path}'"))
    }

    /// Search code in this repository, returning up to `limit` distinct
    /// matching file paths.
    ///
    /// Uses `GET /search/code` scoped with `repo:owner/repo`.
    pub async fn search_code_paths(&self, query: &str, limit: usize) -> Result<Vec<String>> {
        let url = format!("{}/search/code", self.api_base);
        let q = format!("{} repo:{}/{}", query, self.owner, self.repo);
        let per_page = limit.to_string();
        let resp = self
            .http
            .get(&url)
            .headers(self.headers())
            .query(&[("q", q.as_str()), ("per_page", per_page.as_str())])
            .send()
            .await
            .with_context(|| "Failed to search code")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, "Failed to search code");
            anyhow::bail!("GitHub API returned {status} for code search: {body}");
        }

        let value: serde_json::Value = resp
            .json()
            .await
            .with_context(|| "Failed to parse code search response")?;
        Ok(parse_code_search_paths(&value, limit))
    }

    /// Get the authenticated user's GitHub user ID.
    pub async fn get_current_user(&self) -> Result<GitHubUser> {
        let url = format!("{}/user", self.api_base);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers())
            .send()
            .await
            .with_context(|| "Failed to fetch current user")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub API returned {status} for GET /user: {text}");
        }

        Ok(resp.json().await?)
    }
}

/// Percent-encode each segment of a repository file path for the contents
/// API, keeping `/` separators intact (GitHub expects real path segments).
fn encode_content_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Extract distinct file paths from a GitHub code-search response, capped at
/// `limit` entries.
fn parse_code_search_paths(value: &serde_json::Value, limit: usize) -> Vec<String> {
    let mut paths = Vec::new();
    let Some(items) = value.get("items").and_then(|i| i.as_array()) else {
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
mod tests;
