//! REST API endpoints for asynchronous repository health scans.
//!
//! `POST /` enqueues a scan of a server-local directory and
//! `GET /{task_id}` polls the task status and result, mirroring the
//! `/reviews` task model. Scans reuse the repo-review pipeline: when no
//! LLM is configured they run the static experts only, otherwise the
//! LLM-enhanced 3-pass pipeline.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState};
use crate::server::AppState;

use super::review::task_to_status;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(submit_repo_scan))
        .route("/{task_id}", get(get_repo_scan))
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoScanRequest {
    /// Server-local path of the repository directory to scan.
    pub path: String,
}

/// Validate a server-local scan path: reject parent-directory traversal and
/// require an existing directory. Absolute paths are allowed (the scan runs
/// on the server's filesystem by design).
fn validate_scan_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("path must not be empty".to_string());
    }
    if std::path::Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("path must not contain parent directory traversal ('..')".to_string());
    }
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    if !p.is_dir() {
        return Err(format!("path is not a directory: {path}"));
    }
    Ok(())
}

async fn submit_repo_scan(State(state): State<Arc<AppState>>, Json(body): Json<RepoScanRequest>) -> impl IntoResponse {
    if let Err(e) = validate_scan_path(&body.path) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e}))).into_response();
    }

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

    let meta = SourceMeta {
        project: Some(body.path.clone()),
        repository: Some(body.path.clone()),
        ..SourceMeta::default()
    };
    let task_id = store.create(Some(meta.clone())).await;
    let store_clone = store.clone();
    let path = body.path.clone();
    let llm_configs = state.llm_configs.read().unwrap().clone();
    let config = state.app_config.read().unwrap().clone();
    let progress_map = state.progress_map.clone();

    tokio::spawn(async move {
        while !store_clone.can_start_new_task().await {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        store_clone.update(task_id, TaskState::Running, None, None).await;

        let review_id = task_id.to_string();
        let scan_result = tokio::time::timeout(std::time::Duration::from_secs(600), async {
            if llm_configs.is_empty() {
                // Static-only analysis (no LLM configured).
                crate::actions::repo_review::run_local_repo_review(&path, progress_map, &review_id, config).await
            } else {
                // LLM-enhanced analysis.
                let scanner = crate::repo::RepoScanner::new(&path);
                let entries = scanner.scan()?;
                let llm_client = crate::llm::client::LLMClient::new();
                crate::actions::repo_review::run_repo_review(
                    &llm_client,
                    &llm_configs,
                    &path,
                    &entries,
                    progress_map,
                    &review_id,
                    config,
                )
                .await
            }
        })
        .await;

        match scan_result {
            Ok(Ok(output)) => {
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
        source_meta: meta,
        progress: None,
        expert_name: None,
    });

    (StatusCode::ACCEPTED, Json(status)).into_response()
}

async fn get_repo_scan(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_scan_path_rejects_empty() {
        let err = validate_scan_path("").unwrap_err();
        assert!(err.contains("empty"));
        let err = validate_scan_path("   ").unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn test_validate_scan_path_rejects_parent_dir() {
        let err = validate_scan_path("../etc").unwrap_err();
        assert!(err.contains(".."));
        let err = validate_scan_path("/tmp/../etc").unwrap_err();
        assert!(err.contains(".."));
    }

    #[test]
    fn test_validate_scan_path_rejects_nonexistent() {
        let err = validate_scan_path("/nonexistent-repo-scan-path-xyz-12345").unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn test_validate_scan_path_rejects_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let err = validate_scan_path(file.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn test_validate_scan_path_accepts_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_scan_path(dir.path().to_str().unwrap()).is_ok());
    }
}
