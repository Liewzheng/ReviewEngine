//! Shared types and request/response structures for the REST API layer.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::LLMConfig;

#[derive(Debug, Clone, Serialize)]
pub struct TaskStatus {
    pub task_id: Uuid,
    pub status: &'static str,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    // MR metadata fields (added for frontend integration)
    pub mr_title: Option<String>,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
    pub target_branch: Option<String>,
    pub author_name: Option<String>,
    pub author_avatar_url: Option<String>,
    pub gitlab_mr_url: Option<String>,
    pub commit_sha: Option<String>,
    pub progress: Option<u8>,
    pub expert_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub source: ReviewSource,
    pub config: Option<String>,
    pub llm_configs: Option<Vec<LLMConfig>>,
    /// Optional webhook URL POSTed once the task completes or fails.
    pub webhook: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReviewSource {
    #[serde(rename = "gitlab_mr")]
    GitLabMr { url: String, token: String },
    #[serde(rename = "local_repo")]
    LocalRepo {
        path: String,
        base: Option<String>,
        head: Option<String>,
    },
    #[serde(rename = "static_diff")]
    StaticDiff { diff: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigValidateRequest {
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigValidateResponse {
    pub valid: bool,
    pub experts_count: Option<usize>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpertSummary {
    pub name: String,
    pub role: String,
    pub title: String,
    pub trigger: String,
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
