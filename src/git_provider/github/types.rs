use serde::Deserialize;

/// A GitHub Pull Request as returned by the REST API.
///
/// Contains the PR metadata needed for review: title, body, branch refs,
/// author info, and merge state.
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    /// PR number (e.g. `123`).
    pub number: u32,
    /// PR title.
    pub title: String,
    /// PR body / description (may be `None` if empty).
    pub body: Option<String>,
    /// Source (head) branch reference.
    pub head: PrBranch,
    /// Target (base) branch reference.
    pub base: PrBranch,
    /// PR author's GitHub user info.
    pub user: PrUser,
    /// SHA of the merge commit, if the PR has been merged.
    pub merge_commit_sha: Option<String>,
    /// Whether the PR has been merged.
    pub merged: Option<bool>,
}

/// Branch reference for the head or base of a pull request.
#[derive(Debug, Clone, Deserialize)]
pub struct PrBranch {
    /// Display label (e.g. `owner:feature-branch`).
    pub label: String,
    /// Git ref name (e.g. `feature-branch`).
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// Commit SHA at the branch tip.
    pub sha: String,
}

/// Minimal GitHub user information returned with PR data.
#[derive(Debug, Clone, Deserialize)]
pub struct PrUser {
    /// GitHub numeric user ID.
    pub id: u64,
    /// GitHub login / username.
    pub login: String,
}

/// A review comment on a PR (inline or top-level).
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewComment {
    /// Comment ID.
    pub id: i64,
    /// Comment body text (Markdown).
    pub body: String,
    /// Comment author.
    pub user: PrUser,
    /// File path for inline comments (`None` for top-level review comments).
    pub path: Option<String>,
    /// Line number in the diff for inline comments.
    pub line: Option<u32>,
    /// Parent review ID (links inline comments to their top-level review).
    #[serde(rename = "pull_request_review_id")]
    pub review_id: Option<i64>,
}

/// The authenticated GitHub user.
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubUser {
    /// GitHub numeric user ID.
    pub id: u64,
    /// GitHub login / username.
    pub login: String,
}

/// A top-level PR review (not an inline comment).
#[derive(Debug, Clone, Deserialize)]
pub struct PrReview {
    /// Review ID.
    pub id: i64,
    /// Review body text (Markdown, may be `None`).
    pub body: Option<String>,
    /// Review author.
    pub user: PrUser,
    /// Review state: `"APPROVED"`, `"CHANGES_REQUESTED"`, `"COMMENTED"`, etc.
    pub state: String,
}
