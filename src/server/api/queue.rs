//! REST API endpoints for the queue monitor page.
//!
//! Provides queue statistics and task listings for the frontend
//! queue monitor view.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::server::task_queue::{TaskEntry, TaskState};
use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/stats", get(get_queue_stats))
        .route("/tasks", get(get_queue_tasks))
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
        _ => None,
    });

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).min(100);

    let (items, total) = store.list(status, page, per_page).await;
    let tasks: Vec<serde_json::Value> = items.iter().map(task_to_queue_task).collect();

    Json(serde_json::json!({
        "items": tasks,
        "total": total,
        "page": page,
        "per_page": per_page,
    }))
    .into_response()
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
        },
        "progress": entry.progress,
        "expertName": entry.expert_name,
        "elapsedMs": entry.elapsed_ms().unwrap_or(0),
        "createdAt": entry.created_at.to_rfc3339(),
        "startedAt": entry.started_at.map(|t| t.to_rfc3339()),
        "errorMessage": entry.error,
    })
}
