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

use super::review::{resolve_history_entry, task_to_status};

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
    let llm_configs = state.ordered_llm_configs();
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
        request: None,
        source_meta: meta,
        progress: None,
        expert_name: None,
        llm_summary: None,
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
    // RENG-82: scans are written through to `reviews` too, so one started
    // before a restart still has a history row while the in-memory record is
    // gone. Memory first (it carries the live progress/expert fields), the
    // history row second — a memory miss is not a missing scan.
    match store.get(task_id).await {
        Some(entry) => (StatusCode::OK, Json(task_to_status(&entry))).into_response(),
        None => match resolve_history_entry(&state, task_id).await {
            Ok(Some(entry)) => (StatusCode::OK, Json(task_to_status(&entry))).into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "task not found"})),
            )
                .into_response(),
            Err(response) => *response,
        },
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

    /// RENG-82: a scan started before a restart is still in the `reviews`
    /// history, so its detail is served from there instead of 404ing on the
    /// (now empty) in-memory store — memory first, history second.
    #[tokio::test]
    async fn get_repo_scan_falls_back_to_history_when_memory_is_empty() {
        use crate::server::task_queue::TaskStore;

        let db = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        db.migrate().await.unwrap();

        // Created and finished through the task store (write-through INSERT +
        // terminal UPDATE), as submit_repo_scan does.
        let mut store = TaskStore::new();
        store.set_db(db.clone());
        let id = store
            .create(Some(SourceMeta {
                project: Some("/tmp/repo".to_string()),
                repository: Some("/tmp/repo".to_string()),
                ..SourceMeta::default()
            }))
            .await;
        store
            .update(
                id,
                TaskState::Completed,
                Some(serde_json::json!({"summary": "ok"})),
                None,
            )
            .await;

        // Restart wiring: same DB, brand-new (empty) in-memory store.
        let mut restarted = TaskStore::new();
        restarted.set_db(db.clone());
        let mut state = AppState::new(vec![]);
        state.task_store = Some(Arc::new(restarted));
        state.db = Some(db);
        let state = Arc::new(state);
        assert!(
            state.task_store.as_ref().unwrap().get(id).await.is_none(),
            "the scan must not be in memory"
        );

        let resp = get_repo_scan(State(state), Path(id)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["task_id"], id.to_string());
        assert_eq!(json["status"], "completed");
        assert_eq!(json["result"]["summary"], "ok");
    }
}
