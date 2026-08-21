use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use std::sync::Arc;
use uuid::Uuid;

use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState};
use crate::server::AppState;

use super::super::types::{ReviewRequest, ReviewSource};
use super::resolve;
use super::task::enqueue_review;
use super::task::{build_review_detail, build_review_list_item, merge_camel_case_fields, task_to_status, ListParams};

fn error_response(status: StatusCode, message: impl Into<String>) -> axum::response::Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

/// Resolve the GitLab credential for a `gitlab_mr` review, per
/// docs/rest-api.md §1: the `X-Gitlab-Token` request header wins, then the
/// server-side configured token (CLI `--gitlab-token` / `GITLAB_TOKEN` /
/// `PUT /config`). `None` when the source needs no credential. A `gitlab_mr`
/// source with no resolvable credential is a `400`.
fn resolve_gitlab_credential(
    source: &ReviewSource,
    headers: &HeaderMap,
) -> Result<Option<String>, (StatusCode, String)> {
    if !matches!(source, ReviewSource::GitLabMr { .. }) {
        return Ok(None);
    }
    let header = headers.get(resolve::GITLAB_TOKEN_HEADER).and_then(|v| v.to_str().ok());
    resolve::resolve_gitlab_token(header).map(Some).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "gitlab token required for gitlab_mr reviews: pass the X-Gitlab-Token request header or configure a server-side GitLab token".to_string(),
        )
    })
}

/// Validate a `gitlab_mr` source URL synchronously — a pure parse, no
/// network and no credential use — so a malformed MR URL fails fast with
/// 422 at enqueue time instead of being accepted (202) and only failing
/// inside the async review task. The 202 flow for valid URLs is unchanged.
fn validate_gitlab_mr_url(source: &ReviewSource) -> Result<(), (StatusCode, String)> {
    if let ReviewSource::GitLabMr { url } = source {
        if let Err(e) = crate::git_provider::gitlab::client::Client::parse_mr_url(url) {
            return Err((StatusCode::UNPROCESSABLE_ENTITY, format!("invalid gitlab_mr url: {e}")));
        }
    }
    Ok(())
}

/// Validate the optional webhook callback URL (SSRF protection, async DNS).
async fn validate_webhook(webhook: Option<&str>) -> Result<(), (StatusCode, String)> {
    if let Some(url) = webhook {
        if let Err(reason) = crate::server::api::callback::validate_callback_url(url).await {
            return Err((StatusCode::BAD_REQUEST, format!("invalid webhook url: {reason}")));
        }
    }
    Ok(())
}

/// Fail-closed credential check (docs/rest-api.md §1 请求体不得携带凭证): a
/// `token` field inside a `gitlab_mr` source is rejected outright instead of
/// being silently ignored, so clients cannot accidentally persist
/// credentials into the task store. The token value is never echoed back.
fn reject_body_token(raw: &serde_json::Value) -> Result<(), (StatusCode, String)> {
    let source = raw.get("source");
    let is_gitlab_mr = source.and_then(|s| s.get("type")).and_then(|t| t.as_str()) == Some("gitlab_mr");
    let carries_token = source
        .and_then(|s| s.as_object())
        .is_some_and(|obj| obj.contains_key("token"));
    if is_gitlab_mr && carries_token {
        return Err((
            StatusCode::BAD_REQUEST,
            "request body must not carry credentials: pass the GitLab token via the X-Gitlab-Token request header instead".to_string(),
        ));
    }
    Ok(())
}

pub(crate) async fn submit_review(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "task store not initialized"),
    };

    let Json(raw) = match body {
        Ok(json) => json,
        Err(rejection) => {
            return error_response(rejection.status(), rejection.body_text());
        }
    };

    if let Err((status, msg)) = reject_body_token(&raw) {
        return error_response(status, msg);
    }

    let request: ReviewRequest = match serde_json::from_value(raw) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::UNPROCESSABLE_ENTITY, format!("invalid review request: {e}")),
    };

    // Fail fast: a malformed gitlab_mr URL is an unprocessable entity, not a
    // queued task that fails asynchronously.
    if let Err((status, msg)) = validate_gitlab_mr_url(&request.source) {
        return error_response(status, msg);
    }

    if let Err((status, msg)) = validate_webhook(request.webhook.as_deref()).await {
        return error_response(status, msg);
    }

    let gitlab_token = match resolve_gitlab_credential(&request.source, &headers) {
        Ok(token) => token,
        Err((status, msg)) => return error_response(status, msg),
    };

    // The persisted request parameters are serialized from the credential-free
    // struct, so the token can never land in the task store; rerun re-resolves
    // credentials from its own header / the server config.
    let request_json = match serde_json::to_value(&request) {
        Ok(v) => v,
        Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to serialize review request"),
    };
    let task_id = enqueue_review(&state, &store, request, request_json, gitlab_token).await;

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

pub(crate) async fn get_review(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s,
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "task store not initialized"),
    };
    match store.get(task_id).await {
        Some(entry) => {
            let mut status_value = serde_json::to_value(task_to_status(&entry)).unwrap_or_default();
            if let Ok(detail_value) = serde_json::to_value(build_review_detail(&entry)) {
                merge_camel_case_fields(&mut status_value, &detail_value);
            }
            (StatusCode::OK, Json(status_value)).into_response()
        }
        None => error_response(StatusCode::NOT_FOUND, "task not found"),
    }
}

pub(crate) async fn rerun_review(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<Uuid>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s.clone(),
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "task store not initialized"),
    };

    let existing = match store.get(task_id).await {
        Some(entry) => entry,
        None => return error_response(StatusCode::NOT_FOUND, "task not found"),
    };

    if existing.state == TaskState::Pending {
        return error_response(StatusCode::CONFLICT, "task is still queued");
    }
    if existing.state == TaskState::Running {
        return error_response(StatusCode::CONFLICT, "task is still running");
    }

    let request_json = match existing.request {
        Some(r) => r,
        None => {
            return error_response(StatusCode::CONFLICT, "original request parameters are not available");
        }
    };

    let request = match serde_json::from_value::<ReviewRequest>(request_json.clone()) {
        Ok(r) => r,
        Err(_) => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "stored request parameters are not replayable",
            )
        }
    };

    // A rerun is a fresh enqueue: re-validate a stored gitlab_mr URL the
    // same way (legacy tasks persisted before enqueue-time validation fail
    // fast here instead of queuing a task that is doomed to fail).
    if let Err((status, msg)) = validate_gitlab_mr_url(&request.source) {
        return error_response(status, msg);
    }

    // The stored parameters carry no credential (it is never persisted), so
    // rerun re-resolves it under the same rule as submit: the rerun request's
    // own X-Gitlab-Token header first, then the server-side configured token.
    let gitlab_token = match resolve_gitlab_credential(&request.source, &headers) {
        Ok(token) => token,
        Err((status, msg)) => return error_response(status, msg),
    };

    // Re-validate the stored webhook URL: policy is enforced at enqueue time,
    // and a rerun is a fresh enqueue.
    if let Err((status, msg)) = validate_webhook(request.webhook.as_deref()).await {
        return error_response(status, msg);
    }

    let new_task_id = enqueue_review(&state, &store, request, request_json, gitlab_token).await;
    (StatusCode::ACCEPTED, Json(serde_json::json!({"task_id": new_task_id}))).into_response()
}

pub(crate) async fn list_reviews(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s,
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "task store not initialized"),
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

pub(crate) async fn delete_review(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
    let store = match &state.task_store {
        Some(s) => s,
        None => return error_response(StatusCode::SERVICE_UNAVAILABLE, "task store not initialized"),
    };
    let existing = match store.get(task_id).await {
        Some(entry) => entry,
        None => return error_response(StatusCode::NOT_FOUND, "task not found"),
    };
    if matches!(
        existing.state,
        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
    ) {
        return error_response(
            StatusCode::CONFLICT,
            "task is already in a terminal state and cannot be cancelled",
        );
    }
    if store.delete(task_id).await {
        (StatusCode::OK, Json(serde_json::json!({"status": "deleted"}))).into_response()
    } else {
        error_response(
            StatusCode::CONFLICT,
            "task is already in a terminal state and cannot be cancelled",
        )
    }
}
