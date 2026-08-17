//! Shared types and request/response structures for the REST API layer.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::LLMConfig;

/// API response for a review task's current status.
///
/// Returned by `GET /api/v1/reviews/{task_id}` and included in list
/// responses. Contains both the task lifecycle state and MR metadata
/// for display in the queue monitor dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct TaskStatus {
    /// Task UUID.
    pub task_id: Uuid,
    /// Lifecycle state: `"pending"`, `"running"`, `"completed"`, `"failed"`, `"cancelled"`.
    pub status: &'static str,
    /// ISO 8601 timestamp when the task was created.
    pub created_at: String,
    /// ISO 8601 timestamp when the task completed (if done).
    pub completed_at: Option<String>,
    /// Wall-clock milliseconds from creation to completion.
    pub duration_ms: Option<u64>,
    /// Review report JSON (populated on completion).
    pub result: Option<serde_json::Value>,
    /// Error message (populated on failure).
    pub error: Option<String>,
    /// MR/PR title from the source metadata.
    pub mr_title: Option<String>,
    /// Project or namespace path.
    pub project: Option<String>,
    /// Repository name.
    pub repository: Option<String>,
    /// Source branch name.
    pub branch: Option<String>,
    /// Target (base) branch name.
    pub target_branch: Option<String>,
    /// MR/PR author display name.
    pub author_name: Option<String>,
    /// Author avatar URL for the dashboard.
    pub author_avatar_url: Option<String>,
    /// GitLab MR web URL (for linking in the dashboard).
    pub gitlab_mr_url: Option<String>,
    /// Commit SHA of the reviewed diff.
    pub commit_sha: Option<String>,
    /// Current completion percentage (0–100, only while running).
    pub progress: Option<u8>,
    /// Name of the expert currently being executed.
    pub expert_name: Option<String>,
}

/// Incoming review request from the REST API or CLI.
///
/// Specifies the review source (GitLab MR URL, local repo path, or
/// static diff text), optional TOML config override, optional LLM
/// config overrides, and an optional webhook URL for completion notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// What to review (MR URL, local path, or raw diff).
    pub source: ReviewSource,
    /// Optional TOML config override string.
    pub config: Option<String>,
    /// Optional LLM provider config overrides.
    pub llm_configs: Option<Vec<LLMConfig>>,
    /// Optional webhook URL POSTed once the task completes or fails.
    pub webhook: Option<String>,
}

/// The source of a code review request (tagged enum).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReviewSource {
    /// Review a GitLab merge request by URL + token.
    #[serde(rename = "gitlab_mr")]
    GitLabMr { url: String, token: String },
    /// Review a local Git repository directory.
    #[serde(rename = "local_repo")]
    LocalRepo {
        /// Path to the local repository.
        path: String,
        /// Base branch/ref to compare against.
        base: Option<String>,
        /// Head branch/ref to review.
        head: Option<String>,
    },
    /// Review a static diff string (for testing / pre-generated diffs).
    #[serde(rename = "static_diff")]
    StaticDiff { diff: String },
}

/// Request body for TOML config validation (`POST /api/v1/config/validate`).
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigValidateRequest {
    /// Raw TOML configuration string to validate.
    pub body: String,
}

/// Response from config validation indicating validity and expert count.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigValidateResponse {
    /// Whether the config parsed and validated successfully.
    pub valid: bool,
    /// Number of enabled experts found in the config (if valid).
    pub experts_count: Option<usize>,
    /// Validation error messages (empty when valid).
    pub errors: Vec<String>,
}

/// Summary of an expert definition for the experts list API.
#[derive(Debug, Clone, Serialize)]
pub struct ExpertSummary {
    /// Expert name (key in the config TOML).
    pub name: String,
    /// Expert role description.
    pub role: String,
    /// Expert display title.
    pub title: String,
    /// Trigger word or phrase that activates this expert.
    pub trigger: String,
    /// Whether this expert is enabled in the current config.
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionResponse {
    pub version: String,
    pub commit: String,
    pub features: Vec<String>,
}

// ─── Structured review detail (camelCase, frontend `ReviewDetail`) ─────────

/// One expert's result, matching `frontend/src/types/history.ts` `ExpertResult`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertResultDetail {
    pub expert_id: String,
    pub expert_name: String,
    pub status: String,
    pub score: Option<u8>,
    pub summary: String,
    pub details: Option<String>,
}

/// Author of the reviewed MR/PR, matching `ReviewDetail.author`.
///
/// `name`/`avatar_url` are `Option` so absent values serialize as `null`,
/// consistent with the snake_case `author_name`/`author_avatar_url` fields.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDetailAuthor {
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Structured detail response for `GET /reviews/{task_id}`, camelCase
/// serialized to match the frontend `ReviewDetail` type
/// (`frontend/src/types/history.ts`). Built from a `TaskEntry`'s
/// `ReviewOutput` result and merged on top of the existing snake_case
/// `TaskStatus` fields, so existing consumers keep working unchanged.
///
/// Optional metadata fields (`mr_title`, `project`, `branch`, `commit_sha`,
/// `duration_ms`, author name) are `Option` so absent values serialize as
/// `null` — identical to the snake_case side of the same response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDetail {
    pub id: String,
    pub mr_title: Option<String>,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub target_branch: Option<String>,
    pub author: ReviewDetailAuthor,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub commit_sha: Option<String>,
    pub experts: Vec<ExpertResultDetail>,
    pub raw_comment: Option<String>,
    pub raw_api_response: Option<serde_json::Value>,
    pub gitlab_mr_url: Option<String>,
}

/// Lightweight camelCase list item for `GET /reviews`, matching the frontend
/// `ReviewListItem` type (`frontend/src/types/history.ts`). Deliberately omits
/// the heavy `experts`/`rawComment`/`rawApiResponse` fields that only the
/// detail view needs. Optional metadata fields are `Option` → `null` when
/// absent, consistent with the snake_case `TaskStatus` fields.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewListItem {
    pub id: String,
    pub mr_title: Option<String>,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub target_branch: Option<String>,
    pub author: ReviewDetailAuthor,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub created_at: String,
    pub gitlab_mr_url: Option<String>,
}
