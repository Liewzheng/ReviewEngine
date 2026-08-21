use crate::server::api::types::ReviewSource;
use crate::server::task_queue::{SourceMeta, TaskState, TaskStore};
use crate::server::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use std::sync::Arc;
use uuid::Uuid;

use super::handlers::{delete_review, get_review, list_reviews, rerun_review, submit_review};
use super::resolve::{resolve_gitlab_token, resolve_source, GITLAB_TOKEN_HEADER, MAX_STATIC_DIFF_BYTES};
use super::task::{task_to_status, ListParams};

#[tokio::test]
async fn test_resolve_source_static_diff_within_limit() {
    let diff = "diff content".to_string();
    let source = ReviewSource::StaticDiff { diff: diff.clone() };
    let result = resolve_source(source, None, &None).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), diff);
}

#[tokio::test]
async fn test_resolve_source_static_diff_exceeds_limit() {
    let diff = "x".repeat(MAX_STATIC_DIFF_BYTES + 1);
    let source = ReviewSource::StaticDiff { diff };
    let result = resolve_source(source, None, &None).await;
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
    let result = resolve_source(source, None, &None).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("does not exist"));
}

#[tokio::test]
async fn test_resolve_source_local_repo_invalid_base_ref() {
    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path().to_str().unwrap().to_string();
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
    let result = resolve_source(source, None, &None).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("must not start with '-'"));
}

#[tokio::test]
async fn test_resolve_source_gitlab_mr_invalid_url() {
    let source = ReviewSource::GitLabMr {
        url: "not-a-valid-url".to_string(),
    };
    let result = resolve_source(source, Some("test-token".to_string()), &None).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid MR URL format"));
}

#[tokio::test]
async fn test_resolve_source_gitlab_mr_requires_token() {
    // Defense in depth: even if a handler contract is bypassed, the resolver
    // refuses a gitlab_mr review without any credential.
    let source = ReviewSource::GitLabMr {
        url: "https://gitlab.com/owner/repo/-/merge_requests/1".to_string(),
    };
    let result = resolve_source(source, None, &None).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("GitLab token required"), "unexpected error: {err}");

    let source = ReviewSource::GitLabMr {
        url: "https://gitlab.com/owner/repo/-/merge_requests/1".to_string(),
    };
    let result = resolve_source(source, Some("   ".to_string()), &None).await;
    assert!(result.is_err(), "a blank token must be rejected");
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
    let result = resolve_source(source, None, &None).await;
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
        parse_error: None,
        raw_dump_path: None,
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

#[tokio::test]
async fn test_cancel_migrates_task_to_cancelled() {
    let store = TaskStore::new();
    let id = store.create(None).await;

    assert!(store.delete(id).await, "delete should cancel a pending task");
    let entry = store.get(id).await.expect("cancelled task record must be kept");
    assert_eq!(entry.state, TaskState::Cancelled);
    assert!(entry.completed_at.is_some());

    let status = task_to_status(&entry);
    assert_eq!(status.status, "cancelled");

    let stats = store.queue_stats().await;
    assert_eq!(stats.failed, 0, "cancelled must not count as failed");

    assert!(!store.delete(id).await);
}

#[tokio::test]
async fn test_delete_review_404_when_task_not_found() {
    let state = state_with_store();
    let missing = Uuid::new_v4();
    let resp = delete_review(State(state), Path(missing)).await.into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "missing task must be 404");
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "task not found");
}

#[tokio::test]
async fn test_delete_review_409_when_terminal_state() {
    for state in [TaskState::Completed, TaskState::Failed, TaskState::Cancelled] {
        let app_state = state_with_store();
        let store = app_state.task_store.clone().unwrap();
        let id = store.create(None).await;
        store.update(id, state.clone(), None, None).await;

        let resp = delete_review(State(app_state), Path(id)).await.into_response();
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "task in {state:?} must be rejected with 409"
        );
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["error"],
            "task is already in a terminal state and cannot be cancelled"
        );
        assert_eq!(store.get(id).await.expect("record kept").state, state);
    }
}

#[tokio::test]
async fn test_delete_review_200_when_running_or_pending() {
    for initial in [TaskState::Pending, TaskState::Running] {
        let app_state = state_with_store();
        let store = app_state.task_store.clone().unwrap();
        let id = store.create(None).await;
        if initial == TaskState::Running {
            store.update(id, TaskState::Running, None, None).await;
        }

        let resp = delete_review(State(app_state), Path(id)).await.into_response();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "task in {initial:?} must cancel with 200"
        );
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "deleted");
        assert_eq!(store.get(id).await.expect("record kept").state, TaskState::Cancelled);
    }
}

#[tokio::test]
async fn test_rerun_returns_new_task_id() {
    let state = state_with_store();
    let store = state.task_store.clone().unwrap();

    let request = crate::server::api::types::ReviewRequest {
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

    let resp = rerun_review(State(state), Path(original_id), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "rerun must return 202");
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let new_id = Uuid::parse_str(json["task_id"].as_str().unwrap()).unwrap();
    assert_ne!(new_id, original_id, "rerun must create a fresh task id");

    let new_entry = store.get(new_id).await.expect("new task must exist");
    assert_eq!(new_entry.state, TaskState::Pending);
    let replayed: crate::server::api::types::ReviewRequest =
        serde_json::from_value(new_entry.request.unwrap()).unwrap();
    assert!(
        matches!(replayed.source, ReviewSource::StaticDiff { .. }),
        "rerun must replay the original source parameters"
    );
}

#[tokio::test]
async fn test_rerun_rejects_still_running_task() {
    let state = state_with_store();
    let store = state.task_store.clone().unwrap();

    let request = crate::server::api::types::ReviewRequest {
        source: ReviewSource::StaticDiff {
            diff: "diff".to_string(),
        },
        config: None,
        llm_configs: None,
        webhook: None,
    };
    let request_json = serde_json::to_value(&request).unwrap();

    let queued_id = store
        .create_with_request(Some(SourceMeta::default()), Some(request_json.clone()))
        .await;
    let resp = rerun_review(State(state.clone()), Path(queued_id), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "queued task rerun must be 409");
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "task is still queued");

    let running_id = store
        .create_with_request(Some(SourceMeta::default()), Some(request_json))
        .await;
    store.update(running_id, TaskState::Running, None, None).await;
    let resp = rerun_review(State(state), Path(running_id), HeaderMap::new())
        .await
        .into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "running task rerun must be 409");
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "task is still running");
}

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

    assert_eq!(json["task_id"], id.to_string());
    assert_eq!(json["status"], "completed");
    assert_eq!(json["commit_sha"], "abc123");
    assert_eq!(json["gitlab_mr_url"], "http://gitlab/mr/1");

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

    assert_eq!(item["task_id"], id.to_string());
    assert_eq!(item["mr_title"], "Fix login bug");
    assert_eq!(item["author_name"], "alice");
    assert_eq!(item["target_branch"], "main");
    assert_eq!(item["gitlab_mr_url"], "http://gitlab/mr/1");

    assert_eq!(item["id"], id.to_string());
    assert_eq!(item["mrTitle"], "Fix login bug");
    assert_eq!(item["author"]["name"], "alice");
    assert_eq!(item["author"]["avatarUrl"], "http://avatar");
    assert_eq!(item["targetBranch"], "main");
    assert_eq!(item["status"], "completed");
    assert_eq!(item["durationMs"], item["duration_ms"]);
    assert_eq!(item["createdAt"], item["created_at"]);
    assert_eq!(item["gitlabMrUrl"], "http://gitlab/mr/1");

    assert!(item.get("experts").is_none(), "list items must not carry experts");
    assert!(
        item.get("rawApiResponse").is_none(),
        "list items must not carry rawApiResponse"
    );
    assert!(item.get("rawComment").is_none(), "list items must not carry rawComment");
}

#[tokio::test]
async fn test_absent_metadata_is_null_in_both_naming_schemes() {
    let state = state_with_store();
    let store = state.task_store.clone().unwrap();

    let id = store.create(None).await;

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

// ─── credential transport (docs/rest-api.md §1) ─────────────────

/// Snapshot/restore guard for the global GitLab runtime token. Combined with
/// the crate-wide [`crate::server::gitlab::RUNTIME_TEST_LOCK`] this keeps the
/// shared global from leaking between parallel test modules.
struct GitLabRuntimeGuard(crate::server::gitlab::GitLabRuntimeConfig);

impl GitLabRuntimeGuard {
    fn new() -> Self {
        Self(crate::server::gitlab::gitlab_runtime().read().unwrap().clone())
    }
}

impl Drop for GitLabRuntimeGuard {
    fn drop(&mut self) {
        let mut rt = crate::server::gitlab::gitlab_runtime().write().unwrap();
        *rt = self.0.clone();
    }
}

fn gitlab_mr_body() -> serde_json::Value {
    serde_json::json!({
        // Parseable but unreachable: the URL passes enqueue-time validation
        // (and exercises host:port MR URLs end-to-end), then the enqueued
        // task fails fast on the loopback fetch (port 9, discard — refused,
        // no external network) — the handler contract is what is tested.
        "source": {"type": "gitlab_mr", "url": "http://127.0.0.1:9/owner/repo/-/merge_requests/1"}
    })
}

fn headers_with_gitlab_token(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(GITLAB_TOKEN_HEADER, HeaderValue::from_str(token).unwrap());
    headers
}

async fn response_json(resp: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, json)
}

#[tokio::test]
async fn submit_gitlab_mr_with_header_token_returns_202() {
    let state = state_with_store();
    let resp = submit_review(
        State(state),
        headers_with_gitlab_token("glpat-header-token"),
        Ok(Json(gitlab_mr_body())),
    )
    .await
    .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "header token must be accepted, got {json}"
    );
    assert!(json["task_id"].is_string());
}

#[tokio::test]
async fn submit_gitlab_mr_with_invalid_url_returns_422() {
    let cases = [
        "not-a-valid-url",                                        // no scheme
        "https://gitlab.example.com/g/p/-/merge_requests/abc",    // non-integer iid
        "https://user@gitlab.example.com/g/p/-/merge_requests/1", // userinfo
        "http://localhost:/g/p/-/merge_requests/1",               // empty port
        "http://localhost:0/g/p/-/merge_requests/1",              // port below range
        "http://localhost:abc/g/p/-/merge_requests/1",            // non-numeric port
        "http://localhost:99999/g/p/-/merge_requests/1",          // port above range
        "https://git..lab.example.com/g/p/-/merge_requests/1",    // `..` in host
        "http://[::1]:8929/g/p/-/merge_requests/1",               // IPv6 literal
    ];
    for url in cases {
        let state = state_with_store();
        let store = state.task_store.clone().unwrap();
        let body = serde_json::json!({"source": {"type": "gitlab_mr", "url": url}});
        let resp = submit_review(
            State(state),
            headers_with_gitlab_token("glpat-header-token"),
            Ok(Json(body)),
        )
        .await
        .into_response();
        let (status, json) = response_json(resp).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "url {url} must be rejected with 422, got {json}"
        );
        let error = json["error"].as_str().unwrap();
        assert!(
            error.starts_with("invalid gitlab_mr url:"),
            "error must carry the parse failure: {error}"
        );
        let (_, total) = store.list(None, 1, 20, None, None, None, None, None).await;
        assert_eq!(total, 0, "an invalid url must not enqueue a task");
    }
}

#[tokio::test]
async fn submit_gitlab_mr_with_body_token_returns_400() {
    let state = state_with_store();
    let mut body = gitlab_mr_body();
    body["source"]["token"] = serde_json::json!("glpat-body-token");
    let resp = submit_review(State(state), HeaderMap::new(), Ok(Json(body)))
        .await
        .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "body token must be rejected, got {json}"
    );
    let error = json["error"].as_str().unwrap();
    assert!(
        error.contains("X-Gitlab-Token"),
        "error must point at the header transport: {error}"
    );
    assert!(
        !error.contains("glpat-body-token"),
        "the submitted credential must never be echoed: {error}"
    );
    // Fail-closed also applies when a valid header accompanies the body token.
    let state = state_with_store();
    let mut body = gitlab_mr_body();
    body["source"]["token"] = serde_json::json!(serde_json::Value::Null);
    let resp = submit_review(
        State(state),
        headers_with_gitlab_token("glpat-header-token"),
        Ok(Json(body)),
    )
    .await
    .into_response();
    let (status, _) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a `token` key must be rejected even when null and a header is present"
    );
}

#[tokio::test]
async fn submit_gitlab_mr_without_any_token_returns_400() {
    let _lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = String::new();

    let state = state_with_store();
    let resp = submit_review(State(state), HeaderMap::new(), Ok(Json(gitlab_mr_body())))
        .await
        .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "missing credential must be 400, got {json}"
    );
    assert!(
        json["error"].as_str().unwrap().contains("X-Gitlab-Token"),
        "error must explain the credential rule: {json}"
    );
}

#[tokio::test]
async fn submit_gitlab_mr_falls_back_to_server_config_token() {
    let _lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-server-config".to_string();

    let state = state_with_store();
    let resp = submit_review(State(state), HeaderMap::new(), Ok(Json(gitlab_mr_body())))
        .await
        .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "server-side configured token must satisfy the credential rule, got {json}"
    );
}

#[tokio::test]
async fn gitlab_token_resolution_precedence() {
    let _lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-config".to_string();

    // Header wins over the server-side configured token.
    assert_eq!(
        resolve_gitlab_token(Some("glpat-header")),
        Some("glpat-header".to_string())
    );
    // Missing / blank / whitespace header falls back to the config token.
    assert_eq!(resolve_gitlab_token(None), Some("glpat-config".to_string()));
    assert_eq!(resolve_gitlab_token(Some("")), Some("glpat-config".to_string()));
    assert_eq!(resolve_gitlab_token(Some("   ")), Some("glpat-config".to_string()));

    // Neither present → None (callers return 400).
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = String::new();
    assert_eq!(resolve_gitlab_token(None), None);
    assert_eq!(resolve_gitlab_token(Some("")), None);
}

#[tokio::test]
async fn gitlab_token_is_never_persisted_in_task_store() {
    let state = state_with_store();
    let store = state.task_store.clone().unwrap();
    let resp = submit_review(
        State(state),
        headers_with_gitlab_token("glpat-persistence-secret"),
        Ok(Json(gitlab_mr_body())),
    )
    .await
    .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(status, StatusCode::ACCEPTED, "got {json}");
    let task_id = Uuid::parse_str(json["task_id"].as_str().unwrap()).unwrap();

    let entry = store.get(task_id).await.expect("task must be stored");
    let persisted = serde_json::to_string(&entry.request).unwrap();
    assert!(
        !persisted.contains("glpat-persistence-secret"),
        "the credential must not be persisted: {persisted}"
    );
    assert!(
        !persisted.contains("\"token\""),
        "the stored request must carry no token field: {persisted}"
    );
    // The stored request still replays the non-credential parameters.
    let replayed: crate::server::api::types::ReviewRequest = serde_json::from_value(entry.request.unwrap()).unwrap();
    assert!(matches!(replayed.source, ReviewSource::GitLabMr { .. }));
}

// ─── rerun credential re-resolution ─────────────────────────────

/// Store a completed gitlab_mr task whose persisted request carries no
/// credential (the only possible shape after submit-time stripping).
async fn completed_gitlab_mr_task(state: &Arc<AppState>) -> Uuid {
    let store = state.task_store.clone().unwrap();
    let id = store
        .create_with_request(Some(SourceMeta::default()), Some(gitlab_mr_body()))
        .await;
    store
        .update(id, TaskState::Failed, None, Some("boom".to_string()))
        .await;
    id
}

#[tokio::test]
async fn rerun_reresolves_token_from_header() {
    let _lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = String::new();

    let state = state_with_store();
    let original_id = completed_gitlab_mr_task(&state).await;

    let resp = rerun_review(
        State(state),
        Path(original_id),
        headers_with_gitlab_token("glpat-rerun-token"),
    )
    .await
    .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(status, StatusCode::ACCEPTED, "rerun with header must pass, got {json}");
}

#[tokio::test]
async fn rerun_falls_back_to_server_config_token() {
    let _lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-config".to_string();

    let state = state_with_store();
    let original_id = completed_gitlab_mr_task(&state).await;

    let resp = rerun_review(State(state), Path(original_id), HeaderMap::new())
        .await
        .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "rerun must fall back to the server-side configured token, got {json}"
    );
}

#[tokio::test]
async fn rerun_without_any_token_returns_400() {
    let _lock = crate::server::gitlab::RUNTIME_TEST_LOCK.lock().await;
    let _guard = GitLabRuntimeGuard::new();
    crate::server::gitlab::gitlab_runtime().write().unwrap().token = String::new();

    let state = state_with_store();
    let store = state.task_store.clone().unwrap();
    let original_id = completed_gitlab_mr_task(&state).await;

    let resp = rerun_review(State(state), Path(original_id), HeaderMap::new())
        .await
        .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "rerun without credential must be 400, got {json}"
    );
    assert!(
        json["error"].as_str().unwrap().contains("X-Gitlab-Token"),
        "error must explain the credential rule: {json}"
    );

    let entry = store.get(original_id).await.unwrap();
    assert_eq!(entry.state, TaskState::Failed, "the original task must be untouched");
}

// ─── webhook SSRF validation at enqueue time ────────────────────

#[tokio::test]
async fn submit_rejects_invalid_webhook_urls_with_400() {
    let cases = [
        // Cloud metadata / link-local — blocked under both schemes.
        "https://169.254.169.254/latest/meta-data",
        "http://169.254.169.254/hook",
        // Unspecified address.
        "http://0.0.0.0:9000/hook",
        // IPv6 link-local.
        "http://[fe80::1]/hook",
        // http to a public host requires the loopback/private exemption.
        "http://93.184.216.34/hook",
        // Non-http(s) schemes.
        "ftp://example.com/hook",
        "file:///etc/passwd",
        "gopher://127.0.0.1/",
        // Unparseable.
        "not-a-url",
    ];
    for webhook in cases {
        let state = state_with_store();
        let mut body = serde_json::json!({"source": {"type": "static_diff", "diff": "d"}});
        body["webhook"] = serde_json::json!(webhook);
        let resp = submit_review(State(state), HeaderMap::new(), Ok(Json(body)))
            .await
            .into_response();
        let (status, json) = response_json(resp).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "webhook {webhook} must be rejected with 400, got {json}"
        );
        let error = json["error"].as_str().unwrap();
        assert!(
            error.starts_with("invalid webhook url:"),
            "error must carry the documented prefix: {error}"
        );
    }
}

#[tokio::test]
async fn submit_accepts_loopback_webhook() {
    let state = state_with_store();
    // gitlab_mr + header: the enqueued task fails fast on the loopback
    // fetch (connection refused, no external network), then the callback
    // POST to an unused loopback port fails closed.
    let mut body = gitlab_mr_body();
    body["webhook"] = serde_json::json!("http://127.0.0.1:9/hook");
    let resp = submit_review(
        State(state),
        headers_with_gitlab_token("glpat-header-token"),
        Ok(Json(body)),
    )
    .await
    .into_response();
    let (status, json) = response_json(resp).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "loopback http webhook must pass enqueue validation, got {json}"
    );
}
