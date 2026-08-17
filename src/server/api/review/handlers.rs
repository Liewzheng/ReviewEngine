use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::server::log_collector::{push_global_entry, LogMetadata};
use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState, TaskStore};
use crate::server::AppState;

use super::task::enqueue_review;
use super::resolve::run_review;
use super::task::{
    build_review_detail, build_review_list_item, merge_camel_case_fields, source_meta_from_request,
    task_status_str, task_to_status, ListParams,
};
use super::super::types::{
    ExpertResultDetail, ReviewDetail, ReviewDetailAuthor, ReviewListItem, ReviewRequest, ReviewSource, TaskStatus,
};

pub(crate) async fn submit_review(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReviewRequest>,
) -> impl IntoResponse {
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

    let request_json = match serde_json::to_value(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to serialize review request"})),
            )
                .into_response()
        }
    };
    let task_id = enqueue_review(&state, &store, body, request_json).await;

    let status = task_to_status(&TaskEntry {
        task_id,
        state: TaskState::Pending,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        result: None,
        error: None,
        request: None,
        source_meta: SourceMeta::default(),
        progress: None,
        expert_name: None,
    });

    (StatusCode::ACCEPTED, Json(status)).into_response()
}

pub(crate) async fn get_review(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
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
    match store.get(task_id).await {
        Some(entry) => {
            let mut status_value = serde_json::to_value(task_to_status(&entry)).unwrap_or_default();
            if let Ok(detail_value) = serde_json::to_value(build_review_detail(&entry)) {
                merge_camel_case_fields(&mut status_value, &detail_value);
            }
            (StatusCode::OK, Json(status_value)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "task not found"})),
        )
            .into_response(),
    }
}

pub(crate) async fn rerun_review(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
) -> impl IntoResponse {
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

    let existing = match store.get(task_id).await {
        Some(entry) => entry,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "task not found"})),
            )
                .into_response()
        }
    };

    if existing.state == TaskState::Pending {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "task is still queued"})),
        )
            .into_response();
    }
    if existing.state == TaskState::Running {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "task is still running"})),
        )
            .into_response();
    }

    let request_json = match existing.request {
        Some(r) => r,
        None => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "original request parameters are not available"})),
            )
                .into_response()
        }
    };

    let request = match serde_json::from_value::<ReviewRequest>(request_json.clone()) {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "stored request parameters are not replayable"})),
            )
                .into_response()
        }
    };

    let new_task_id = enqueue_review(&state, &store, request, request_json).await;
    (StatusCode::ACCEPTED, Json(serde_json::json!({"task_id": new_task_id}))).into_response()
}

pub(crate) async fn list_reviews(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
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
        "pending" => Some(TaskState::Pending),
        "running" => Some(TaskState::Running),
        "completed" => Some(TaskState::Completed),
        "failed" => Some(TaskState::Failed),
        "cancelled" => Some(TaskState::Cancelled),
        _ => None,
    });
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(20).min(100);

    let date_from = params.date_from.as_deref().and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    });
    let date_to = params.date_to.as_deref().and_then(|s| {
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
    });

    let (items, total) = store
        .list(
            status,
            page,
            per_page,
            params.q.as_deref(),
            params.project.as_deref(),
            params.repository.as_deref(),
            date_from,
            date_to,
        )
        .await;
    let items: Vec<serde_json::Value> = items
        .iter()
        .map(|entry| {
            let mut status_value = serde_json::to_value(task_to_status(entry)).unwrap_or_default();
            if let Ok(item_value) = serde_json::to_value(build_review_list_item(entry)) {
                merge_camel_case_fields(&mut status_value, &item_value);
            }
            status_value
        })
        .collect();

    Json(serde_json::json!({
        "items": items,
        "total": total,
        "page": page,
        "per_page": per_page,
    }))
    .into_response()
}

pub(crate) async fn delete_review(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
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
    let existing = match store.get(task_id).await {
        Some(entry) => entry,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "task not found"})),
            )
                .into_response()
        }
    };
    if matches!(
        existing.state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    ) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "task is already in a terminal state and cannot be cancelled"})),
        )
            .into_response();
    }
    if store.delete(task_id).await {
        (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))).into_response()
    } else {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "task is already in a terminal state and cannot be cancelled"})),
        )
            .into_response()
    }
}
