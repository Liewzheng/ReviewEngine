//! REST API endpoints for the queue monitor page.
//!
//! Provides queue statistics and task listings for the frontend
//! queue monitor view.

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

use crate::server::task_queue::{TaskEntry, TaskState};
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stats", get(get_queue_stats))
        .route("/tasks", get(get_queue_tasks))
        .route("/tasks/{task_id}", delete(delete_queue_task))
        .route("/tasks/{task_id}/retry", post(post_retry_task))
        .route("/pause", post(post_pause))
        .route("/resume", post(post_resume))
        .route("/max-concurrent", post(post_max_concurrent))
}

async fn get_queue_stats(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => {
            return Json(serde_json::json!({
                "active": 0,
                "queued": 0,
                "failed": 0,
                "totalDepth": 0,
                "maxConcurrent": 8,
                "queueCapacity": 16,
                "failedLast24h": 0,
                "totalLast24h": 0,
                "isPaused": false,
            }))
            .into_response()
        }
    };

    let stats = store.queue_stats().await;
    Json(serde_json::json!({
        "active": stats.active,
        "queued": stats.queued,
        "failed": stats.failed,
        "totalDepth": stats.total_depth,
        "maxConcurrent": stats.max_concurrent,
        "queueCapacity": stats.queue_capacity,
        "failedLast24h": stats.failed_last_24h,
        "totalLast24h": stats.total_last_24h,
        "isPaused": stats.is_paused,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct QueueTaskParams {
    status: Option<String>,
    page: Option<u64>,
    per_page: Option<u64>,
}

async fn get_queue_tasks(
    State(state): State<Arc<AppState>>,
    Query(params): Query<QueueTaskParams>,
) -> impl IntoResponse {
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
        "running" => Some(TaskState::Running),
        "queued" => Some(TaskState::Pending),
        "failed" => Some(TaskState::Failed),
        "completed" => Some(TaskState::Completed),
        "cancelled" => Some(TaskState::Cancelled),
        _ => None,
    });

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).min(100);

    let (items, total) = store.list(status, page, per_page, None, None, None, None, None).await;
    let tasks: Vec<serde_json::Value> = items.iter().map(task_to_queue_task).collect();

    Json(serde_json::json!({
        "items": tasks,
        "total": total,
        "page": page,
        "per_page": per_page,
    }))
    .into_response()
}

async fn delete_queue_task(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
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

async fn post_retry_task(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
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
    if store.retry(task_id).await {
        (StatusCode::OK, Json(serde_json::json!({"status": "retried"}))).into_response()
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "task not found or not in failed state"})),
        )
            .into_response()
    }
}

async fn post_pause(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
    store.pause().await;
    Json(serde_json::json!({"status": "paused"})).into_response()
}

async fn post_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
    store.resume().await;
    Json(serde_json::json!({"status": "resumed"})).into_response()
}

#[derive(Deserialize)]
pub struct MaxConcurrentRequest {
    max_concurrent: usize,
}

async fn post_max_concurrent(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MaxConcurrentRequest>,
) -> impl IntoResponse {
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
    store.set_max_concurrent(body.max_concurrent).await;
    Json(serde_json::json!({"maxConcurrent": body.max_concurrent})).into_response()
}

fn task_to_queue_task(entry: &TaskEntry) -> serde_json::Value {
    let meta = &entry.source_meta;
    serde_json::json!({
        "id": entry.task_id.to_string(),
        "mrTitle": meta.mr_title.as_deref().unwrap_or("Untitled"),
        "project": meta.project.as_deref().unwrap_or("unknown"),
        "repository": meta.repository.as_deref().unwrap_or("unknown"),
        "status": match entry.state {
            TaskState::Pending => "queued",
            TaskState::Running => "running",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        },
        "progress": entry.progress,
        "expertName": entry.expert_name,
        "elapsedMs": entry.elapsed_ms().unwrap_or(0),
        "createdAt": entry.created_at.to_rfc3339(),
        "startedAt": entry.started_at.map(|t| t.to_rfc3339()),
        "errorMessage": entry.error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    /// AppState with the store deliberately cleared — the only way the
    /// `None` fallback paths are reachable now that `AppState::new`
    /// initialises a store eagerly.
    fn state_without_store() -> Arc<AppState> {
        let mut state = AppState::new(vec![]);
        state.task_store = None;
        Arc::new(state)
    }

    fn state_with_store() -> Arc<AppState> {
        Arc::new(AppState::new(vec![]))
    }

    async fn body_of(response: impl IntoResponse) -> serde_json::Value {
        let resp = response.into_response();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// `None` fallback: the stats endpoint keeps serving the documented
    /// defaults instead of 503 — a pure guard, unreachable in production
    /// where the store is always present.
    #[tokio::test]
    async fn stats_without_store_falls_back_to_defaults() {
        let json = body_of(get_queue_stats(State(state_without_store())).await).await;
        assert_eq!(json["active"], 0);
        assert_eq!(json["queued"], 0);
        assert_eq!(json["failed"], 0);
        assert_eq!(json["totalDepth"], 0);
        assert_eq!(json["maxConcurrent"], 8);
        assert_eq!(json["queueCapacity"], 16);
        assert_eq!(json["failedLast24h"], 0);
        assert_eq!(json["totalLast24h"], 0);
        assert_eq!(json["isPaused"], false);
    }

    /// With a store the stats reflect the real task store — the fix that makes
    /// the queue page show actual activity instead of hardcoded zeros.
    #[tokio::test]
    async fn stats_with_store_reflects_real_entries() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();
        let id = store.create(None).await;
        store.update(id, TaskState::Running, None, None).await;
        let failed = store.create(None).await;
        store
            .update(failed, TaskState::Failed, None, Some("boom".to_string()))
            .await;

        let json = body_of(get_queue_stats(State(state)).await).await;
        assert_eq!(json["active"], 1);
        assert_eq!(json["queued"], 0);
        assert_eq!(json["failed"], 1);
        assert_eq!(json["totalDepth"], 1);
        assert_eq!(json["maxConcurrent"], 8);
        assert_eq!(json["queueCapacity"], 16);
        assert_eq!(json["failedLast24h"], 1);
        assert_eq!(json["totalLast24h"], 2);
    }

    #[tokio::test]
    async fn tasks_without_store_returns_503() {
        let resp = get_queue_tasks(
            State(state_without_store()),
            Query(QueueTaskParams {
                status: None,
                page: None,
                per_page: None,
            }),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = body_of(resp).await;
        assert_eq!(json["error"], "task store not initialized");
    }

    #[tokio::test]
    async fn tasks_with_store_returns_real_entries() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();
        let id = store.create(None).await;
        store.update(id, TaskState::Completed, None, None).await;

        let json = body_of(
            get_queue_tasks(
                State(state),
                Query(QueueTaskParams {
                    status: None,
                    page: None,
                    per_page: None,
                }),
            )
            .await,
        )
        .await;
        assert_eq!(json["total"], 1);
        assert_eq!(json["items"][0]["id"], id.to_string());
        assert_eq!(json["items"][0]["status"], "completed");
    }
}
