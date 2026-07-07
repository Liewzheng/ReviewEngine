//! REST API endpoints for creating, listing, and deleting review tasks.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState};
use crate::server::AppState;
use crate::team::orchestrator;

use super::types::{ReviewRequest, ReviewSource, TaskStatus};

const MAX_STATIC_DIFF_BYTES: usize = 5 * 1024 * 1024; // 5 MB

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(submit_review))
        .route("/", get(list_reviews))
        .route("/{task_id}", get(get_review))
        .route("/{task_id}", delete(delete_review))
}

async fn submit_review(State(state): State<Arc<AppState>>, Json(body): Json<ReviewRequest>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "task store not initialized"})),
            )
                .into_response()
        }
    };

    let source_meta = source_meta_from_request(&body.source);
    let task_id = store.create(Some(source_meta)).await;
    let store_clone = store.clone();
    let source = body.source.clone();
    let config_toml = body.config.clone();
    let llm_configs = body.llm_configs.clone().unwrap_or_default();
    let cfg = state.app_config.read().unwrap().clone();

    tokio::spawn(async move {
        store_clone.update(task_id, TaskState::Running, None, None).await;

        let diff_raw = match resolve_source(source, &cfg).await {
            Ok(d) => d,
            Err(e) => {
                store_clone
                    .update(task_id, TaskState::Failed, None, Some(e.to_string()))
                    .await;
                return;
            }
        };

        let config_source = config_toml.map(crate::models::ConfigSource::Inline);
        let app_config = match crate::config::resolve_config(config_source).await {
            Ok(c) => c,
            Err(e) => {
                store_clone
                    .update(task_id, TaskState::Failed, None, Some(e.to_string()))
                    .await;
                return;
            }
        };

        let experts = app_config.build_expert_defs();
        let mr_info = crate::models::MRInfo::new(
            "api".to_string(),
            "API Review".to_string(),
            "unknown".to_string(),
            "unknown".to_string(),
        );

        let review_result = tokio::time::timeout(
            std::time::Duration::from_secs(600),
            orchestrator::run_experts(&experts, &mr_info, &diff_raw, &llm_configs, &app_config, None, ""),
        )
        .await;

        match review_result {
            Ok(Ok((reports, _))) => {
                let output = crate::models::ReviewOutput::new(reports);
                let value = serde_json::to_value(&output).unwrap_or_default();
                store_clone
                    .update(task_id, TaskState::Completed, Some(value), None)
                    .await;
            }
            Ok(Err(e)) => {
                store_clone
                    .update(task_id, TaskState::Failed, None, Some(e.to_string()))
                    .await;
            }
            Err(_) => {
                store_clone
                    .update(
                        task_id,
                        TaskState::Failed,
                        None,
                        Some("Task timed out after 600 seconds".to_string()),
                    )
                    .await;
            }
        }
    });

    let status = task_to_status(&TaskEntry {
        task_id,
        state: TaskState::Pending,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        result: None,
        error: None,
        source_meta: SourceMeta::default(),
        progress: None,
        expert_name: None,
    });

    (StatusCode::ACCEPTED, Json(status)).into_response()
}

async fn get_review(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "task store not initialized"})),
            )
                .into_response()
        }
    };
    match store.get(task_id).await {
        Some(entry) => (StatusCode::OK, Json(task_to_status(&entry))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "task not found"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct ListParams {
    status: Option<String>,
    page: Option<u64>,
    per_page: Option<u64>,
}

async fn list_reviews(State(state): State<Arc<AppState>>, Query(params): Query<ListParams>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "task store not initialized"})),
            )
                .into_response()
        }
    };

    let status = params.status.as_deref().and_then(|s| match s {
        "pending" => Some(TaskState::Pending),
        "running" => Some(TaskState::Running),
        "completed" => Some(TaskState::Completed),
        "failed" => Some(TaskState::Failed),
        _ => None,
    });
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100);

    let (items, total) = store.list(status, page, per_page).await;
    let items: Vec<TaskStatus> = items.iter().map(task_to_status).collect();

    Json(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "per_page": per_page,
    }))
    .into_response()
}

async fn delete_review(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "task store not initialized"})),
            )
                .into_response()
        }
    };
    if store.delete(task_id).await {
        (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))).into_response()
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "task not found or cannot be cancelled"})),
        )
            .into_response()
    }
}

fn task_to_status(entry: &TaskEntry) -> TaskStatus {
    let meta = &entry.source_meta;
    TaskStatus {
        task_id: entry.task_id,
        status: match entry.state {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
        },
        created_at: entry.created_at.to_rfc3339(),
        completed_at: entry.completed_at.map(|t| t.to_rfc3339()),
        duration_ms: entry.duration_ms(),
        result: entry.result.clone(),
        error: entry.error.clone(),
        mr_title: meta.mr_title.clone(),
        project: meta.project.clone(),
        repository: meta.repository.clone(),
        branch: meta.branch.clone(),
        target_branch: meta.target_branch.clone(),
        author_name: meta.author_name.clone(),
        author_avatar_url: meta.author_avatar_url.clone(),
        gitlab_mr_url: meta.gitlab_mr_url.clone(),
        commit_sha: meta.commit_sha.clone(),
        progress: entry.progress,
        expert_name: entry.expert_name.clone(),
    }
}

fn source_meta_from_request(source: &ReviewSource) -> SourceMeta {
    match source {
        ReviewSource::GitLabMr { url, .. } => {
            let mut meta = SourceMeta::default();
            // Extract project path from GitLab MR URL: https://gitlab.com/group/project/-/merge_requests/1
            if let Some((path_part, _)) = url.split_once("/-/merge_requests/") {
                if let Some((_proto, rest)) = path_part.split_once("://") {
                    if let Some((_, path)) = rest.split_once('/') {
                        meta.project = Some(path.to_string());
                        meta.repository = Some(path.to_string());
                        meta.gitlab_mr_url = Some(url.clone());
                    }
                }
            }
            meta
        }
        ReviewSource::LocalRepo { path, .. } => SourceMeta {
            project: Some(path.clone()),
            repository: Some(path.clone()),
            ..SourceMeta::default()
        },
        ReviewSource::StaticDiff { .. } => SourceMeta::default(),
    }
}

async fn resolve_source(
    source: ReviewSource,
    _config: &Option<Arc<crate::models::AppConfig>>,
) -> anyhow::Result<String> {
    match source {
        ReviewSource::GitLabMr { url, token } => {
            let client = crate::git_provider::gitlab::client::Client::new(&token, &url)?;
            let diff = client.fetch_diff().await?;
            Ok(diff)
        }
        ReviewSource::LocalRepo { path, base, head } => {
            // Validate repo path before use to prevent directory traversal
            let repo_path = std::path::Path::new(&path);
            if !repo_path.exists() {
                anyhow::bail!("Repository path does not exist: {}", path);
            }
            if !repo_path.is_dir() {
                anyhow::bail!("Repository path is not a directory: {}", path);
            }
            // Validate base and head refs to prevent command injection
            if let Some(ref base_ref) = base {
                crate::git::local::validate_ref(base_ref)?;
            }
            if let Some(ref head_ref) = head {
                crate::git::local::validate_ref(head_ref)?;
            }
            let browser = crate::git::local::LocalGitBrowser::new(&path);
            let diff = browser
                .get_diff(base.as_deref().unwrap_or("main"), head.as_deref(), false, None, None)
                .await?;
            Ok(diff)
        }
        ReviewSource::StaticDiff { diff } => {
            if diff.len() > MAX_STATIC_DIFF_BYTES {
                anyhow::bail!(
                    "Static diff exceeds maximum size of {} MB",
                    MAX_STATIC_DIFF_BYTES / (1024 * 1024)
                );
            }
            Ok(diff)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::api::types::ReviewSource;

    #[tokio::test]
    async fn test_resolve_source_static_diff_within_limit() {
        let diff = "diff content".to_string();
        let source = ReviewSource::StaticDiff { diff: diff.clone() };
        let result = resolve_source(source, &None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), diff);
    }

    #[tokio::test]
    async fn test_resolve_source_static_diff_exceeds_limit() {
        let diff = "x".repeat(MAX_STATIC_DIFF_BYTES + 1);
        let source = ReviewSource::StaticDiff { diff };
        let result = resolve_source(source, &None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds maximum size"));
    }

    #[tokio::test]
    async fn test_resolve_source_local_repo_nonexistent_path() {
        let source = ReviewSource::LocalRepo {
            path: "/tmp/nonexistent_repo_12345".to_string(),
            base: None,
            head: None,
        };
        let result = resolve_source(source, &None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[tokio::test]
    async fn test_resolve_source_local_repo_invalid_base_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        // Initialize a git repo so the path exists and is a dir
        let status = std::process::Command::new("git")
            .args(["-C", &repo_path, "init", "--initial-branch=main"])
            .status()
            .expect("git init failed");
        assert!(status.success());

        let source = ReviewSource::LocalRepo {
            path: repo_path,
            base: Some("--help".to_string()),
            head: None,
        };
        let result = resolve_source(source, &None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must not start with '-'"));
    }

    #[tokio::test]
    async fn test_resolve_source_gitlab_mr_invalid_url() {
        let source = ReviewSource::GitLabMr {
            url: "not-a-valid-url".to_string(),
            token: "test-token".to_string(),
        };
        let result = resolve_source(source, &None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Invalid MR URL format"));
    }

    #[tokio::test]
    async fn test_resolve_source_local_repo_invalid_head_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().to_str().unwrap().to_string();
        let status = std::process::Command::new("git")
            .args(["-C", &repo_path, "init", "--initial-branch=main"])
            .status()
            .expect("git init failed");
        assert!(status.success());

        let source = ReviewSource::LocalRepo {
            path: repo_path,
            base: Some("main".to_string()),
            head: Some("; echo evil".to_string()),
        };
        let result = resolve_source(source, &None).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("forbidden shell metacharacters"));
    }
}
