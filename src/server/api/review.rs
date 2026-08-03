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

use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState, TaskStore};
use crate::server::AppState;
use crate::team::orchestrator;

use super::types::{
    ExpertResultDetail, ReviewDetail, ReviewDetailAuthor, ReviewListItem, ReviewRequest, ReviewSource, TaskStatus,
};

const MAX_STATIC_DIFF_BYTES: usize = 5 * 1024 * 1024; // 5 MB

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(submit_review))
        .route("/", get(list_reviews))
        .route("/{task_id}", get(get_review))
        .route("/{task_id}", delete(delete_review))
        .route("/{task_id}/rerun", post(rerun_review))
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

/// Enqueue a review task and spawn its background execution.
///
/// Shared by `POST /reviews` and `POST /reviews/{task_id}/rerun` so both
/// creation paths behave identically. The serialized `request_json` is stored
/// on the task so a later rerun can replay the same inputs.
async fn enqueue_review(
    state: &Arc<AppState>,
    store: &TaskStore,
    request: ReviewRequest,
    request_json: serde_json::Value,
) -> Uuid {
    let source_meta = source_meta_from_request(&request.source);
    let task_id = store.create_with_request(Some(source_meta), Some(request_json)).await;
    let store_clone = store.clone();
    let source = request.source;
    let config_toml = request.config;
    // Request-explicit providers win; when the request omits `llm_configs`
    // (or sends an empty list), fall back to the server-side configuration
    // (`state.llm_configs`, seeded from env `LLM_CONFIG` or file `[[llm]]`),
    // mirroring the webhook path. Without this the UI's POST — which never
    // sends `llm_configs` — would run the expert team with zero providers and
    // every LLM-backed expert would fail with "LLM config 'default' has no
    // api_base set".
    let llm_configs = match request.llm_configs {
        Some(configs) if !configs.is_empty() => configs,
        _ => state.llm_configs.read().unwrap().clone(),
    };
    let webhook = request.webhook;
    let cfg = state.app_config.read().unwrap().clone();

    tokio::spawn(async move {
        while !store_clone.can_start_new_task().await {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        store_clone.update(task_id, TaskState::Running, None, None).await;

        // Single exit point: persist the outcome, then fire the webhook callback.
        match run_review(source, &cfg, config_toml, llm_configs).await {
            Ok((value, summary)) => {
                store_clone
                    .update(task_id, TaskState::Completed, Some(value), None)
                    .await;
                super::callback::spawn_callback(webhook, task_id, "completed", Some(summary), None);
            }
            Err(e) => {
                let message = e.to_string();
                store_clone
                    .update(task_id, TaskState::Failed, None, Some(message.clone()))
                    .await;
                super::callback::spawn_callback(webhook, task_id, "failed", None, Some(message));
            }
        }
    });
    task_id
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
        Some(entry) => {
            let mut status_value = serde_json::to_value(task_to_status(&entry)).unwrap_or_default();
            // Merge the camelCase structured detail on top of the existing
            // TaskStatus fields so the frontend `ReviewDetail` type is served
            // without breaking the snake_case contract.
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

/// Re-run a settled review task with its original request parameters.
///
/// Creates a brand-new task in the queue with the same source, config, LLM
/// configs, and webhook as the original, returning the new task id (202).
/// Rejects unknown tasks (404) and tasks still queued or running (409).
async fn rerun_review(State(state): State<Arc<AppState>>, Path(task_id): Path<Uuid>) -> impl IntoResponse {
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

    // A queued or running task cannot be re-run until it settles.
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

#[derive(Deserialize)]
pub struct ListParams {
    status: Option<String>,
    page: Option<u64>,
    per_page: Option<u64>,
    q: Option<String>,
    project: Option<String>,
    repository: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
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
            // Merge the lightweight camelCase `ReviewListItem` fields on top of
            // the snake_case `TaskStatus` so the frontend list renders without
            // defensive fallbacks (snake_case keys stay intact).
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

/// Canonical task status string, consistent across the reviews list/detail and
/// the dashboard `recentReviews`. Mirrors the frontend `ReviewStatus` vocabulary.
pub(crate) fn task_status_str(state: &TaskState) -> &'static str {
    match state {
        TaskState::Pending => "pending",
        TaskState::Running => "running",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}

/// Merge the camelCase keys of `extra` into `base` in place, preserving every
/// original snake_case `TaskStatus` key (backward compatibility with consumers
/// that read the legacy fields).
fn merge_camel_case_fields(base: &mut serde_json::Value, extra: &serde_json::Value) {
    if let (Some(base_obj), Some(extra_obj)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            base_obj.insert(k.clone(), v.clone());
        }
    }
}

pub(crate) fn task_to_status(entry: &TaskEntry) -> TaskStatus {
    let meta = &entry.source_meta;
    TaskStatus {
        task_id: entry.task_id,
        status: task_status_str(&entry.state),
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

/// Build the structured, camelCase `ReviewDetail` from a task entry.
///
/// `experts[]` is derived from the stored `ReviewOutput` per-expert reports;
/// per-expert scores reuse the same [`expert_score`](crate::scoring::review::expert_score)
/// used by the lead consolidator. `raw_comment` carries the aggregated report's
/// markdown (the full MR comment) when an aggregator ran.
fn build_review_detail(entry: &TaskEntry) -> ReviewDetail {
    let meta = &entry.source_meta;
    let status = task_status_str(&entry.state);

    let (experts, raw_comment) = match &entry.result {
        Some(result) => match serde_json::from_value::<crate::models::ReviewOutput>(result.clone()) {
            Ok(output) => {
                let experts = output
                    .reports
                    .iter()
                    .map(|report| ExpertResultDetail {
                        expert_id: report.expert_name.clone(),
                        expert_name: report.expert_name.clone(),
                        status: "success".to_string(),
                        score: Some(crate::scoring::review::expert_score(&report.findings)),
                        summary: if report.markdown.is_empty() {
                            format!("{} finding(s)", report.findings.len())
                        } else {
                            report.markdown.clone()
                        },
                        details: if report.raw_llm_response.is_empty() {
                            None
                        } else {
                            Some(report.raw_llm_response.clone())
                        },
                    })
                    .collect();
                let raw_comment = output.aggregated.as_ref().map(|agg| agg.markdown.clone());
                (experts, raw_comment)
            }
            Err(_) => (Vec::new(), None),
        },
        None => (Vec::new(), None),
    };

    ReviewDetail {
        id: entry.task_id.to_string(),
        mr_title: meta.mr_title.clone(),
        project: meta.project.clone(),
        repository: meta.repository.clone(),
        branch: meta.branch.clone(),
        target_branch: meta.target_branch.clone(),
        author: ReviewDetailAuthor {
            name: meta.author_name.clone(),
            avatar_url: meta.author_avatar_url.clone(),
        },
        status: status.to_string(),
        duration_ms: entry.duration_ms(),
        created_at: entry.created_at.to_rfc3339(),
        completed_at: entry.completed_at.map(|t| t.to_rfc3339()),
        commit_sha: meta.commit_sha.clone(),
        experts,
        raw_comment,
        raw_api_response: entry.result.clone(),
        gitlab_mr_url: meta.gitlab_mr_url.clone(),
    }
}

/// Build the lightweight, camelCase `ReviewListItem` for a task entry.
///
/// Mirrors `build_review_detail` but omits the heavy per-expert fields that
/// only the detail view consumes (`experts`, `rawComment`, `rawApiResponse`).
fn build_review_list_item(entry: &TaskEntry) -> ReviewListItem {
    let meta = &entry.source_meta;
    ReviewListItem {
        id: entry.task_id.to_string(),
        mr_title: meta.mr_title.clone(),
        project: meta.project.clone(),
        repository: meta.repository.clone(),
        branch: meta.branch.clone(),
        target_branch: meta.target_branch.clone(),
        author: ReviewDetailAuthor {
            name: meta.author_name.clone(),
            avatar_url: meta.author_avatar_url.clone(),
        },
        status: task_status_str(&entry.state).to_string(),
        duration_ms: entry.duration_ms(),
        created_at: entry.created_at.to_rfc3339(),
        gitlab_mr_url: meta.gitlab_mr_url.clone(),
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

/// Execute a review task end to end: resolve the diff source, build the
/// configuration, and run the expert team. On success returns the stored
/// result value plus a short human-readable summary for the webhook callback.
async fn run_review(
    source: ReviewSource,
    cfg: &Option<Arc<crate::models::AppConfig>>,
    config_toml: Option<String>,
    llm_configs: Vec<crate::models::LLMConfig>,
) -> anyhow::Result<(serde_json::Value, String)> {
    let diff_raw = resolve_source(source, cfg).await?;

    let config_source = config_toml.map(crate::models::ConfigSource::Inline);
    let app_config = crate::config::resolve_config(config_source).await?;

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

    let (reports, _, dropped_findings, consolidated) = match review_result {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("Task timed out after 600 seconds"),
    };

    let output = crate::models::ReviewOutput::new(reports)
        .with_dropped_findings(dropped_findings)
        .with_consolidated(consolidated);
    let findings: usize = output.reports.iter().map(|r| r.findings.len()).sum();
    let summary = format!("{} expert report(s), {} finding(s)", output.reports.len(), findings);
    let value = serde_json::to_value(&output).unwrap_or_default();
    Ok((value, summary))
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

    // ─── cancelled / rerun / structured detail ─────────────────────

    fn make_finding(severity: crate::models::Severity) -> crate::models::Finding {
        crate::models::Finding {
            file: "src/main.rs".to_string(),
            line: Some(1),
            line_end: None,
            severity,
            confidence: 8,
            category: "security".to_string(),
            title: "Test finding".to_string(),
            summary: "detail".to_string(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: String::new(),
            effort: crate::models::Effort::Small,
            expert_name: "security".to_string(),
            expert_role: "Security Expert".to_string(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    fn make_report(name: &str, findings: Vec<crate::models::Finding>) -> crate::models::ExpertReport {
        crate::models::ExpertReport {
            expert_name: name.to_string(),
            findings,
            markdown: format!("## {} review\n", name),
            raw_llm_response: format!("raw {}", name),
        }
    }

    fn source_meta_with_commit() -> SourceMeta {
        SourceMeta {
            mr_title: Some("Fix login bug".to_string()),
            project: Some("group/repo".to_string()),
            repository: Some("group/repo".to_string()),
            branch: Some("feature/x".to_string()),
            target_branch: Some("main".to_string()),
            author_name: Some("alice".to_string()),
            author_avatar_url: Some("http://avatar".to_string()),
            gitlab_mr_url: Some("http://gitlab/mr/1".to_string()),
            commit_sha: Some("abc123".to_string()),
        }
    }

    fn state_with_store() -> Arc<AppState> {
        let store = Arc::new(TaskStore::new());
        let mut state = AppState::new(vec![]);
        state.task_store = Some(store);
        Arc::new(state)
    }

    /// Unit 1: DELETE migrates a queued task to `Cancelled` (record kept), the
    /// serialized status is "cancelled", and cancelled is not counted as failed.
    #[tokio::test]
    async fn test_cancel_migrates_task_to_cancelled() {
        let store = TaskStore::new();
        let id = store.create(None).await;

        assert!(store.delete(id).await, "delete should cancel a pending task");
        let entry = store.get(id).await.expect("cancelled task record must be kept");
        assert_eq!(entry.state, TaskState::Cancelled);
        assert!(entry.completed_at.is_some());

        // Serialization + list filtering agree on the "cancelled" string.
        let status = task_to_status(&entry);
        assert_eq!(status.status, "cancelled");

        // Cancelled tasks are not counted among failed in queue stats.
        let stats = store.queue_stats().await;
        assert_eq!(stats.failed, 0, "cancelled must not count as failed");

        // A second delete on an already-cancelled task is a no-op (not Pending/Running).
        assert!(!store.delete(id).await);
    }

    /// Unit 2: rerunning a settled task returns a new, distinct `task_id` (202)
    /// and the new task replays the original request parameters.
    #[tokio::test]
    async fn test_rerun_returns_new_task_id() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();

        let request = ReviewRequest {
            source: ReviewSource::StaticDiff {
                diff: "diff content".to_string(),
            },
            config: None,
            llm_configs: None,
            webhook: None,
        };
        let request_json = serde_json::to_value(&request).unwrap();
        let original_id = store
            .create_with_request(Some(SourceMeta::default()), Some(request_json))
            .await;
        store
            .update(
                original_id,
                TaskState::Completed,
                Some(serde_json::json!({"reports": []})),
                None,
            )
            .await;

        let resp = rerun_review(State(state), Path(original_id)).await.into_response();
        assert_eq!(resp.status(), StatusCode::ACCEPTED, "rerun must return 202");
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let new_id = Uuid::parse_str(json["task_id"].as_str().unwrap()).unwrap();
        assert_ne!(new_id, original_id, "rerun must create a fresh task id");

        let new_entry = store.get(new_id).await.expect("new task must exist");
        assert_eq!(new_entry.state, TaskState::Pending);
        let replayed: ReviewRequest = serde_json::from_value(new_entry.request.unwrap()).unwrap();
        assert!(
            matches!(replayed.source, ReviewSource::StaticDiff { .. }),
            "rerun must replay the original source parameters"
        );
    }

    /// Unit 2 error path: a task still queued or running cannot be re-run.
    /// The 409 message distinguishes the two (unit 9).
    #[tokio::test]
    async fn test_rerun_rejects_still_running_task() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();

        let request = ReviewRequest {
            source: ReviewSource::StaticDiff {
                diff: "diff".to_string(),
            },
            config: None,
            llm_configs: None,
            webhook: None,
        };
        let request_json = serde_json::to_value(&request).unwrap();

        // Queued (Pending) task → "task is still queued".
        let queued_id = store
            .create_with_request(Some(SourceMeta::default()), Some(request_json.clone()))
            .await;
        let resp = rerun_review(State(state.clone()), Path(queued_id))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "queued task rerun must be 409");
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "task is still queued");

        // Running task → "task is still running".
        let running_id = store
            .create_with_request(Some(SourceMeta::default()), Some(request_json))
            .await;
        store.update(running_id, TaskState::Running, None, None).await;
        let resp = rerun_review(State(state), Path(running_id)).await.into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "running task rerun must be 409");
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "task is still running");
    }

    /// Unit 3: `GET /reviews/{task_id}` merges the structured camelCase
    /// `ReviewDetail` on top of the existing snake_case `TaskStatus` fields.
    #[tokio::test]
    async fn test_get_review_exposes_camelcase_detail_and_keeps_snakecase() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();

        let output = crate::models::ReviewOutput::new(vec![make_report(
            "security",
            vec![make_finding(crate::models::Severity::High)],
        )]);
        let result = serde_json::to_value(&output).unwrap();
        let id = store.create(Some(source_meta_with_commit())).await;
        store.update(id, TaskState::Completed, Some(result), None).await;

        let resp = get_review(State(state), Path(id)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Backward compatibility: snake_case TaskStatus fields survive untouched.
        assert_eq!(json["task_id"], id.to_string());
        assert_eq!(json["status"], "completed");
        assert_eq!(json["commit_sha"], "abc123");
        assert_eq!(json["gitlab_mr_url"], "http://gitlab/mr/1");

        // New camelCase ReviewDetail fields are present and correctly named.
        assert_eq!(json["id"], id.to_string());
        assert_eq!(json["mrTitle"], "Fix login bug");
        assert_eq!(json["commitSha"], "abc123");
        assert_eq!(json["gitlabMrUrl"], "http://gitlab/mr/1");
        assert_eq!(json["author"]["name"], "alice");
        assert_eq!(json["rawApiResponse"]["reports"][0]["expert_name"], "security");

        let experts = json["experts"].as_array().expect("experts must be an array");
        assert_eq!(experts.len(), 1);
        let expert = &experts[0];
        assert_eq!(expert["expertId"], "security");
        assert_eq!(expert["expertName"], "security");
        assert_eq!(expert["status"], "success");
        assert!(
            expert["score"].is_number(),
            "expert score must be derived from findings"
        );
        assert!(expert["summary"].as_str().unwrap().contains("security"));
        assert_eq!(expert["details"].as_str().unwrap(), "raw security");
    }

    /// Unit 4: `GET /reviews` list items merge camelCase `ReviewListItem` fields
    /// on top of the snake_case `TaskStatus` keys, with both naming schemes
    /// agreeing, and without the heavy detail-only fields.
    #[tokio::test]
    async fn test_list_reviews_merges_camelcase_and_keeps_snakecase() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();

        let id = store.create(Some(source_meta_with_commit())).await;
        store
            .update(id, TaskState::Completed, Some(serde_json::json!({"reports": []})), None)
            .await;

        let params = ListParams {
            status: None,
            page: None,
            per_page: None,
            q: None,
            project: None,
            repository: None,
            date_from: None,
            date_to: None,
        };
        let resp = list_reviews(State(state), Query(params)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let item = &json["items"][0];

        // Backward compatibility: snake_case TaskStatus keys are preserved.
        assert_eq!(item["task_id"], id.to_string());
        assert_eq!(item["mr_title"], "Fix login bug");
        assert_eq!(item["author_name"], "alice");
        assert_eq!(item["target_branch"], "main");
        assert_eq!(item["gitlab_mr_url"], "http://gitlab/mr/1");

        // camelCase ReviewListItem keys are merged and agree with snake_case.
        assert_eq!(item["id"], id.to_string());
        assert_eq!(item["mrTitle"], "Fix login bug");
        assert_eq!(item["author"]["name"], "alice");
        assert_eq!(item["author"]["avatarUrl"], "http://avatar");
        assert_eq!(item["targetBranch"], "main");
        assert_eq!(item["status"], "completed");
        assert_eq!(item["durationMs"], item["duration_ms"]);
        assert_eq!(item["createdAt"], item["created_at"]);
        assert_eq!(item["gitlabMrUrl"], "http://gitlab/mr/1");

        // Heavy detail-only fields must not leak into list items.
        assert!(item.get("experts").is_none(), "list items must not carry experts");
        assert!(
            item.get("rawApiResponse").is_none(),
            "list items must not carry rawApiResponse"
        );
        assert!(item.get("rawComment").is_none(), "list items must not carry rawComment");
    }

    /// Unit 6: absent metadata serializes as `null` in both naming schemes —
    /// a task with no source metadata must not show `""`/`"unknown"` in camelCase
    /// while the snake_case side is `null`.
    #[tokio::test]
    async fn test_absent_metadata_is_null_in_both_naming_schemes() {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();

        // Pending task with default SourceMeta: no title/project/branch/commit,
        // and no completed_at so duration_ms is also absent.
        let id = store.create(None).await;

        // Detail
        let resp = get_review(State(state.clone()), Path(id)).await.into_response();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["mrTitle"].is_null(),
            "mrTitle must be null, got {:?}",
            json["mrTitle"]
        );
        assert_eq!(json["mr_title"], serde_json::Value::Null);
        assert!(json["author"]["name"].is_null(), "author.name must be null");
        assert_eq!(json["author_name"], serde_json::Value::Null);
        assert!(json["commitSha"].is_null(), "commitSha must be null");
        assert_eq!(json["commit_sha"], serde_json::Value::Null);
        assert!(json["durationMs"].is_null(), "durationMs must be null");
        assert_eq!(json["duration_ms"], serde_json::Value::Null);

        // List
        let params = ListParams {
            status: None,
            page: None,
            per_page: None,
            q: None,
            project: None,
            repository: None,
            date_from: None,
            date_to: None,
        };
        let resp = list_reviews(State(state), Query(params)).await.into_response();
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let item = &json["items"][0];
        assert!(item["mrTitle"].is_null(), "list mrTitle must be null");
        assert!(item["author"]["name"].is_null(), "list author.name must be null");
        assert!(item["durationMs"].is_null(), "list durationMs must be null");
    }
}
