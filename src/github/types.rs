use serde::Deserialize;

/// A GitHub Pull Request as returned by the API.
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u32,
    pub title: String,
    pub body: Option<String>,
    pub head: PrBranch,
    pub base: PrBranch,
    pub user: PrUser,
    pub merge_commit_sha: Option<String>,
    pub merged: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrBranch {
    pub label: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrUser {
    pub id: u64,
    pub login: String,
}

/// A review comment on a PR (inline or top-level).
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewComment {
    pub id: i64,
    pub body: String,
    pub user: PrUser,
    pub path: Option<String>,
    pub line: Option<u32>,
    #[serde(rename = "pull_request_review_id")]
    pub review_id: Option<i64>,
}

/// The authenticated user.
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubUser {
    pub id: u64,
    pub login: String,
}

/// A top-level PR review (not an inline comment).
#[derive(Debug, Clone, Deserialize)]
pub struct PrReview {
    pub id: i64,
    pub body: Option<String>,
    pub user: PrUser,
    pub state: String,
}
