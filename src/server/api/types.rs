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

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewRequest {
    pub source: ReviewSource,
    pub config: Option<String>,
    pub llm_configs: Option<Vec<LLMConfig>>,
}

#[derive(Debug, Clone, Deserialize)]
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
