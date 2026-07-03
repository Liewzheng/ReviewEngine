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
            api_base: "https://api.github.com".to_string(),
            owner,
            repo,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> Client {
        Client::new_test("test_token", "https://github.com/owner/repo/pull/1", &server.uri()).unwrap()
    }

    /// Matcher that only matches requests without a `page` query parameter.
    struct NoPage;

    impl wiremock::Match for NoPage {
        fn matches(&self, request: &wiremock::Request) -> bool {
            !request.url.query_pairs().any(|(k, _)| k == "page")
        }
    }

    // ─── fetch_pr_info ──────────────────────────────

    #[tokio::test]
    async fn test_fetch_pr_info() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "number": 1,
                "title": "Test PR",
                "body": "description",
                "head": {"label": "owner:branch", "ref": "feature", "sha": "abc123"},
                "base": {"label": "owner:main", "ref": "main", "sha": "def456"},
                "user": {"id": 100, "login": "testuser"},
                "merge_commit_sha": null,
                "merged": false
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let info = client.fetch_pr_info().await.unwrap();

        assert_eq!(info.project_path, "owner/repo");
        assert_eq!(info.mr_iid, 1);
        assert_eq!(info.title, "Test PR");
        assert_eq!(info.description, "description");
        assert_eq!(info.source_branch, "feature");
        assert_eq!(info.target_branch, "main");
        assert_eq!(info.git_hash, "abc123");
        assert_eq!(info.base_sha, Some("def456".to_string()));
        assert_eq!(info.merge_commit_sha, None);
        assert_eq!(info.pr_author, Some("testuser".to_string()));
        assert_eq!(info.pr_author_id, Some(100));
    }

    #[tokio::test]
    async fn test_fetch_pr_info_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let err = client.fetch_pr_info().await.unwrap_err();
        assert!(err.to_string().contains("401"), "error should mention 401, got: {err}");
    }

    #[tokio::test]
    async fn test_fetch_pr_info_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let err = client.fetch_pr_info().await.unwrap_err();
        assert!(err.to_string().contains("403"), "error should mention 403, got: {err}");
    }

    // ─── fetch_diff ─────────────────────────────────

    #[tokio::test]
    async fn test_fetch_diff_ok() {
        let server = MockServer::start().await;
        let diff_text = "diff --git a/src/main.rs b/src/main.rs\nindex abc..def 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n+new line\n old line";

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1"))
            .and(header("Accept", "application/vnd.github.v3.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string(diff_text))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let diff = client.fetch_diff().await.unwrap();
        assert_eq!(diff, diff_text);
    }

    // ─── create_pr_review ───────────────────────────

    #[tokio::test]
    async fn test_create_pr_review_ok() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 42})))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let id = client.create_pr_review("test body").await.unwrap();
        assert_eq!(id, 42);
    }

    #[tokio::test]
    async fn test_create_pr_review_403() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/pulls/1/reviews"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let err = client.create_pr_review("test body").await.unwrap_err();
        assert!(err.to_string().contains("403"), "error should mention 403, got: {err}");
    }

    // ─── create_review_comment ──────────────────────

    #[tokio::test]
    async fn test_create_review_comment_ok() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/pulls/1/comments"))
            .respond_with(ResponseTemplate::new(201))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let result = client.create_review_comment("src/main.rs", 10, "nice code").await;
        assert!(result.is_ok());
    }

    // ─── list_review_comments (paginated) ───────────

    #[tokio::test]
    async fn test_list_review_comments_paginated() {
        let server = MockServer::start().await;
        let base_uri = server.uri();

        let page1 = json!([
            {"id": 1, "body": "comment1", "user": {"id": 200, "login": "botuser"}, "path": "src/main.rs", "line": 10, "pull_request_review_id": 42}
        ]);
        let page2 = json!([
            {"id": 2, "body": "comment2", "user": {"id": 200, "login": "botuser"}, "path": "src/lib.rs", "line": 20, "pull_request_review_id": 43}
        ]);

        let next_url = format!("{base_uri}/repos/owner/repo/pulls/1/comments?per_page=100&page=2");
        let link_header = format!(r#"<{next_url}>; rel="next", <{next_url}>; rel="last""#);

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1/comments"))
            .and(query_param("per_page", "100"))
            .and(NoPage)
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(page1)
                    .insert_header("Link", link_header),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1/comments"))
            .and(query_param("per_page", "100"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page2))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let comments = client.list_review_comments().await.unwrap();

        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, 1);
        assert_eq!(comments[1].id, 2);
    }

    // ─── list_pr_reviews (paginated) ────────────────

    #[tokio::test]
    async fn test_list_pr_reviews_paginated() {
        let server = MockServer::start().await;
        let base_uri = server.uri();

        let page1 = json!([
            {"id": 42, "body": "review body", "user": {"id": 200, "login": "botuser"}, "state": "COMMENT"}
        ]);
        let page2 = json!([
            {"id": 43, "body": "second review", "user": {"id": 200, "login": "botuser"}, "state": "APPROVE"}
        ]);

        let next_url = format!("{base_uri}/repos/owner/repo/pulls/1/reviews?per_page=100&page=2");
        let link_header = format!(r#"<{next_url}>; rel="next", <{next_url}>; rel="last""#);

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1/reviews"))
            .and(query_param("per_page", "100"))
            .and(NoPage)
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(page1)
                    .insert_header("Link", link_header),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/pulls/1/reviews"))
            .and(query_param("per_page", "100"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(page2))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let reviews = client.list_pr_reviews().await.unwrap();

        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].id, 42);
        assert_eq!(reviews[1].id, 43);
    }

    // ─── get_current_user ───────────────────────────

    #[tokio::test]
    async fn test_get_current_user_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 200, "login": "botuser"})))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let user = client.get_current_user().await.unwrap();
        assert_eq!(user.id, 200);
        assert_eq!(user.login, "botuser");
    }
}
