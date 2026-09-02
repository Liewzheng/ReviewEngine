use axum::{http::StatusCode, Json};
use serde_json::Value;
use std::sync::Arc;

use super::super::dispatcher::MrDispatcher;
use crate::server::task_queue::{record_task_outcome, record_task_started, SourceMeta, TaskStore};

/// Parsed payload from a GitLab Merge Request webhook event.
pub struct MrHookPayload {
    pub action: String,
    pub mr_url: String,
    pub mr_iid: u64,
    pub sha: String,
    pub gitlab_token: String,
    /// `project.path_with_namespace` of the MR's project (empty when absent).
    pub path_with_namespace: String,
    pub mr_title: String,
    pub source_branch: String,
    pub target_branch: String,
    pub author_name: String,
    pub author_avatar_url: String,
}

/// Parse and validate an MR webhook body into its essential fields.
pub fn parse_mr_hook_payload(body: &str, gitlab_token: &str) -> Result<MrHookPayload, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse MR hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let action = parsed["object_attributes"]["action"].as_str().unwrap_or("").to_string();
    // System hooks (admin-level) carry the FULL MR URL in
    // `object_attributes.url`; project webhooks lack it and fall back to
    // `project.web_url + /-/merge_requests/{iid}`.
    let object_attr_url = parsed["object_attributes"]["url"].as_str().unwrap_or("").to_string();
    let project_url = parsed["project"]["web_url"].as_str().unwrap_or("").to_string();
    let mr_iid = parsed["object_attributes"]["iid"].as_u64().unwrap_or(0);
    let mr_url = if !object_attr_url.is_empty() {
        object_attr_url
    } else if !project_url.is_empty() && mr_iid > 0 {
        format!("{}/-/merge_requests/{}", project_url, mr_iid)
    } else {
        String::new()
    };
    let sha = parsed["object_attributes"]["last_commit"]["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let path_with_namespace = parsed["project"]["path_with_namespace"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let mr_title = parsed["object_attributes"]["title"].as_str().unwrap_or("").to_string();
    let source_branch = parsed["object_attributes"]["source_branch"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let target_branch = parsed["object_attributes"]["target_branch"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // Author: prefer the MR author block when present, otherwise fall back to
    // the webhook trigger user. MRInfo will back-fill the authoritative author
    // once the review pipeline resolves the MR metadata.
    let author_name = parsed["object_attributes"]["author"]["name"]
        .as_str()
        .or_else(|| parsed["user"]["name"].as_str())
        .unwrap_or("")
        .to_string();
    let author_avatar_url = parsed["object_attributes"]["author"]["avatar_url"]
        .as_str()
        .or_else(|| parsed["user"]["avatar_url"].as_str())
        .unwrap_or("")
        .to_string();

    Ok(MrHookPayload {
        action,
        mr_url,
        mr_iid,
        sha,
        gitlab_token: gitlab_token.to_string(),
        path_with_namespace,
        mr_title,
        source_branch,
        target_branch,
        author_name,
        author_avatar_url,
    })
}

/// Execute a webhook-dispatched MR review on a detached task, recording its
/// lifecycle in the task store when one is available.
///
/// With a store: creates a Running task entry from `source_meta`, resolves the
/// MR metadata to back-fill title/branch/author, runs the review, then marks it
/// Completed (with the full [`ReviewOutput`] result) or Failed (with the error
/// message). Without a store this is exactly the legacy behavior — run, log,
/// and release the dispatcher's dedup on failure.
async fn run_webhook_review(
    task_store: Option<Arc<TaskStore>>,
    dispatcher: &MrDispatcher,
    mr_url: String,
    sha: String,
    gitlab_token: String,
    mr_iid: u64,
    source_meta: SourceMeta,
) {
    let task_id = if let Some(store) = task_store.as_ref() {
        Some(record_task_started(store, source_meta).await)
    } else {
        None
    };

    let outcome = async {
        let (info, diff) = super::super::resolve_review_source(&mr_url, &gitlab_token).await?;
        if let (Some(store), Some(id)) = (task_store.as_ref(), task_id) {
            store
                .fill_source_meta(id, crate::server::task_queue::source_meta_from_mr_info(&info))
                .await;
        }
        super::super::run_review_common(
            &mr_url,
            &gitlab_token,
            Some(dispatcher),
            Some(&mr_url),
            Some(&sha),
            info,
            diff,
        )
        .await
    }
    .await;

    if let Err(e) = &outcome {
        tracing::error!("Review failed for MR !{}: {:?}", mr_iid, e);
        dispatcher.reset(&mr_url).await;
    }

    if let (Some(store), Some(id)) = (task_store.as_ref(), task_id) {
        record_task_outcome(store, id, &outcome).await;
    }
}

/// Spawn a background task that runs the full review for an MR, recording its
/// lifecycle in the task store when one is available.
pub fn spawn_mr_review_task(
    dispatcher: &MrDispatcher,
    mr_url: String,
    sha: String,
    gitlab_token: String,
    mr_iid: u64,
    task_store: Option<Arc<TaskStore>>,
    source_meta: SourceMeta,
) {
    let d = dispatcher.clone();
    tokio::spawn(async move {
        run_webhook_review(task_store, &d, mr_url, sha, gitlab_token, mr_iid, source_meta).await;
    });
}

/// Handle the `InProgress` dispatcher state: wait and then retry.
pub async fn handle_mr_in_progress(
    dispatcher: &MrDispatcher,
    mr_url: &str,
    sha: &str,
    gitlab_token: &str,
    mr_iid: u64,
    task_store: Option<Arc<TaskStore>>,
    source_meta: SourceMeta,
) {
    tracing::info!("MR !{} review in progress, waiting...", mr_iid);
    dispatcher.wait(mr_url).await;
    // After wait, re-check if current SHA needs a new review
    match dispatcher.try_start(mr_url, sha).await {
        super::super::dispatcher::ShouldStart::Go => {
            spawn_mr_review_task(
                dispatcher,
                mr_url.to_string(),
                sha.to_string(),
                gitlab_token.to_string(),
                mr_iid,
                task_store,
                source_meta,
            );
        }
        _ => {
            tracing::info!("No new review needed for MR !{} after wait", mr_iid);
        }
    }
}

/// Dispatch an MR webhook event to start or defer a review based on the
/// dispatcher state.
pub async fn dispatch_mr_event(
    dispatcher: &MrDispatcher,
    mr_url: &str,
    sha: &str,
    gitlab_token: &str,
    mr_iid: u64,
    task_store: Option<Arc<TaskStore>>,
    source_meta: SourceMeta,
) {
    match dispatcher.try_start(mr_url, sha).await {
        super::super::dispatcher::ShouldStart::Go => {
            spawn_mr_review_task(
                dispatcher,
                mr_url.to_string(),
                sha.to_string(),
                gitlab_token.to_string(),
                mr_iid,
                task_store,
                source_meta,
            );
        }
        super::super::dispatcher::ShouldStart::AlreadyReviewed => {
            tracing::info!("Skipping MR !{}: already reviewed at SHA {}", mr_iid, sha);
        }
        super::super::dispatcher::ShouldStart::InProgress => {
            handle_mr_in_progress(dispatcher, mr_url, sha, gitlab_token, mr_iid, task_store, source_meta).await;
        }
    }
}

/// Build a [`SourceMeta`] from the parsed GitLab webhook payload.
///
/// Empty payload fields are omitted so `fill_source_meta` can later back-fill
/// authoritative values from `MRInfo` without clobbering non-empty webhook data.
pub(crate) fn source_meta_from_payload(payload: &MrHookPayload) -> SourceMeta {
    fn non_empty(s: &str) -> Option<String> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
    let project = non_empty(&payload.path_with_namespace);
    SourceMeta {
        mr_title: non_empty(&payload.mr_title),
        project: project.clone(),
        repository: project,
        branch: non_empty(&payload.source_branch),
        target_branch: non_empty(&payload.target_branch),
        author_name: non_empty(&payload.author_name),
        author_avatar_url: non_empty(&payload.author_avatar_url),
        gitlab_mr_url: non_empty(&payload.mr_url),
        commit_sha: non_empty(&payload.sha),
    }
}

/// Rewrite a payload MR URL onto a configured platform's reachable base URL.
///
/// GitLab webhook payloads carry the instance's `external_url` (e.g.
/// `http://localhost:8929` on a dev box, `https://gitlab.islet.space:8443` on
/// a NAS) — often NOT reachable from inside the review container, where
/// `localhost` is the container itself and `:8443` is an external port
/// mapping. The configured platform's `base_url` is the endpoint the server
/// actually reaches (e.g. `http://host.docker.internal:8929`,
/// `https://gitlab.islet.space` on the container-internal 443), so the
/// payload URL's path (query included) is re-hosted onto it.
///
/// Applies to both system hooks (`object_attributes.url`, already a full MR
/// URL) and project-level webhooks (the `project.web_url +
/// /-/merge_requests/{iid}` construction) — `web_url` is the same
/// `external_url` and just as likely to be unreachable.
///
/// **Fail-safe**: an unparseable payload URL or base URL returns the payload
/// URL unchanged (legacy behavior) — this function never panics.
pub(crate) fn rewrite_url_to_platform(url: &str, base_url: &str) -> String {
    let Ok(payload) = reqwest::Url::parse(url.trim()) else {
        return url.to_string();
    };
    let Ok(base) = reqwest::Url::parse(base_url.trim()) else {
        return url.to_string();
    };
    let mut path = payload.path().to_string();
    if let Some(query) = payload.query() {
        path.push('?');
        path.push_str(query);
    }
    // `reqwest::Url` normalizes a host-only URL to a trailing `/`; strip it so
    // the payload's path appends cleanly (trailing-slash base_urls work too).
    format!("{}{}", base.as_str().trim_end_matches('/'), path)
}

/// The review-time reachable base for `platform`: `internal_base_url` when
/// configured (the container-reachable endpoint, e.g. the NAS's internal 443
/// while the payload carries the external :8443), else `base_url`. The
/// fail-safe in [`rewrite_url_to_platform`] keeps the payload URL verbatim
/// when the chosen target does not parse.
fn review_base_url(platform: &crate::models::GitPlatformConfig) -> &str {
    if platform.internal_base_url.is_empty() {
        &platform.base_url
    } else {
        &platform.internal_base_url
    }
}

pub async fn handle_mr_hook(
    body: &str,
    dispatcher: &MrDispatcher,
    gitlab_token: &str,
    platform: Option<crate::models::GitPlatformConfig>,
    task_store: Option<Arc<TaskStore>>,
) -> Result<Json<Value>, StatusCode> {
    let payload = parse_mr_hook_payload(body, gitlab_token)?;

    // Project allowlist gate, before any dispatch: a non-empty allowlist on
    // the matched platform that does not contain this payload's
    // `project.path_with_namespace` ignores the event wholesale. An unmatched
    // payload (or a platform without verification credentials) yields `None`
    // → empty allowlist → every project allowed (legacy behavior).
    let allowed_projects: &[String] = platform.as_ref().map(|p| p.allowed_projects.as_slice()).unwrap_or(&[]);
    if !allowed_projects.is_empty() && !allowed_projects.contains(&payload.path_with_namespace) {
        tracing::info!(
            project = %payload.path_with_namespace,
            "MR hook ignored: project not in allowlist"
        );
        return Ok(Json(serde_json::json!({
            "status": "ignored",
            "reason": "project not in allowlist",
        })));
    }

    tracing::info!("MR !{} webhook received: action={}", payload.mr_iid, payload.action);

    // Only process opened/reopened/updated MRs
    if payload.action == "open" || payload.action == "reopen" || payload.action == "update" {
        if payload.mr_url.is_empty() || payload.gitlab_token.is_empty() {
            tracing::warn!("Skipping review: missing MR URL or GITLAB_TOKEN");
            return Ok(Json(serde_json::json!({
                "status": "skipped",
                "reason": "missing MR URL or GITLAB_TOKEN"
            })));
        }

        if payload.sha.is_empty() {
            tracing::warn!("Skipping review: missing commit SHA");
            return Ok(Json(serde_json::json!({
                "status": "skipped",
                "reason": "missing commit SHA"
            })));
        }

        // The payload's MR URL is GitLab's `external_url`, often unreachable
        // from inside the review container. When a git platform matched this
        // payload, re-host the URL onto the platform's reachable base
        // (`internal_base_url` when configured, else `base_url`); the SAME
        // rewritten URL becomes the dispatch dedup key (consistency).
        // Unmatched → keep the payload URL (legacy behavior).
        let review_url = platform
            .as_ref()
            .map(|p| rewrite_url_to_platform(&payload.mr_url, review_base_url(p)))
            .unwrap_or_else(|| payload.mr_url.clone());

        let source_meta = source_meta_from_payload(&payload);

        dispatch_mr_event(
            dispatcher,
            &review_url,
            &payload.sha,
            &payload.gitlab_token,
            payload.mr_iid,
            task_store,
            source_meta,
        )
        .await;
    }

    Ok(Json(serde_json::json!({
        "status": "received",
        "action": payload.action,
    })))
}

/// True when `note` (already lowercased) begins with a slash command whose
/// first path segment is exactly `cmd` — i.e. `/review` and `/review/123`
/// match, but `/reviewer` / `/reviewxyz` do not. The command must be followed
/// by a path separator (`/`) or the end of the note, so prefix lookalikes
/// never trigger a review (`^/review(/|$)` semantics).
pub fn note_starts_with_command(note: &str, cmd: &str) -> bool {
    let Some(rest) = note.strip_prefix(cmd) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/')
}

/// Extract the merge request iid from the tail of a system-hook note/MR URL
/// like `https://gitlab.example.com/group/proj/-/merge_requests/123`. Matches
/// the LAST `/-/merge_requests/` marker and parses the leading digit run that
/// follows (query strings / trailing segments are ignored). `None` when the
/// marker is absent or carries no numeric id.
pub(crate) fn mr_iid_from_url(url: &str) -> Option<u64> {
    const MARKER: &str = "/-/merge_requests/";
    let idx = url.rfind(MARKER)?;
    let digits: String = url[idx + MARKER.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

pub async fn handle_note_hook(
    body: &str,
    dispatcher: &MrDispatcher,
    gitlab_token: &str,
    platform: Option<crate::models::GitPlatformConfig>,
    task_store: Option<Arc<TaskStore>>,
) -> Result<Json<Value>, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse Note hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let note = parsed["object_attributes"]["note"].as_str().unwrap_or("");
    let note_lower = note.to_lowercase();

    // Check for commands like /review, /describe. Matched on a path-segment
    // boundary so `/reviewer` / `/reviewxyz` never trigger a review.
    if note_starts_with_command(&note_lower, "/review") || note_starts_with_command(&note_lower, "/describe") {
        // Project allowlist gate before any review dispatch.
        let path = parsed["project"]["path_with_namespace"].as_str().unwrap_or("");
        let allowed_projects: &[String] = platform.as_ref().map(|p| p.allowed_projects.as_slice()).unwrap_or(&[]);
        if !allowed_projects.is_empty() && !allowed_projects.iter().any(|p| p == path) {
            tracing::info!(project = %path, "note hook ignored: project not in allowlist");
            return Ok(Json(serde_json::json!({
                "status": "ignored",
                "reason": "project not in allowlist",
            })));
        }
        // System hooks lack `project.web_url`; fall back to `project.homepage`.
        let project_url = parsed["project"]["web_url"]
            .as_str()
            .or_else(|| parsed["project"]["homepage"].as_str())
            .unwrap_or("")
            .to_string();
        let mut mr_iid = parsed["merge_request"]["iid"]
            .as_u64()
            .or_else(|| parsed["object_attributes"]["noteable_iid"].as_u64())
            .unwrap_or(0);
        // System hook notes may omit an iid; extract it from the
        // `object_attributes.url` tail (`/-/merge_requests/{iid}`).
        if mr_iid == 0 {
            if let Some(iid) = parsed["object_attributes"]["url"].as_str().and_then(mr_iid_from_url) {
                mr_iid = iid;
            }
        }
        let mr_url = if !project_url.is_empty() && mr_iid > 0 {
            format!("{}/-/merge_requests/{}", project_url, mr_iid)
        } else {
            String::new()
        };

        if !mr_url.is_empty() && !gitlab_token.is_empty() {
            // Same rewrite as the MR hook: re-host the note's MR URL onto the
            // matched platform's reachable base (`internal_base_url` when
            // configured, else `base_url`; the note URL is built from
            // `project.web_url`/`homepage` — GitLab's external_url).
            // Unmatched → payload URL unchanged (legacy behavior).
            let url = platform
                .as_ref()
                .map(|p| rewrite_url_to_platform(&mr_url, review_base_url(p)))
                .unwrap_or(mr_url);
            let token = gitlab_token.to_string();
            let sha = format!("note_{}", uuid::Uuid::new_v4());

            let path = parsed["project"]["path_with_namespace"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let project = (!path.is_empty()).then_some(path.clone());
            let source_meta = SourceMeta {
                project: project.clone(),
                repository: project,
                gitlab_mr_url: Some(url.clone()),
                commit_sha: Some(sha.clone()),
                ..SourceMeta::default()
            };

            match dispatcher.try_start(&url, &sha).await {
                super::super::dispatcher::ShouldStart::Go => {
                    let d = dispatcher.clone();
                    let u = url;
                    let s = sha;
                    let note_iid = mr_iid;
                    tokio::spawn(async move {
                        run_webhook_review(task_store, &d, u, s, token, note_iid, source_meta).await;
                    });
                }
                _ => {
                    tracing::info!("Note review skipped or already in progress");
                }
            }
        }
    }

    Ok(Json(serde_json::json!({
        "status": "received",
        "note_preview": &note[..note.len().min(100)],
    })))
}

pub async fn handle_push_hook(body: &str) -> Result<Json<Value>, StatusCode> {
    let _parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse Push hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    tracing::info!("Push hook received");

    Ok(Json(serde_json::json!({
        "status": "received",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::task_queue::{record_task_outcome, record_task_started, TaskState, TaskStore};

    const MR_URL: &str = "http://gitlab.internal:8929/group/proj/-/merge_requests/7";
    const SHA: &str = "abc123";

    fn full_source_meta() -> SourceMeta {
        SourceMeta {
            mr_title: Some("Fix login bug".to_string()),
            project: Some("group/proj".to_string()),
            repository: Some("group/proj".to_string()),
            branch: Some("feature/login".to_string()),
            target_branch: Some("main".to_string()),
            author_name: Some("alice".to_string()),
            author_avatar_url: Some("http://avatar".to_string()),
            gitlab_mr_url: Some(MR_URL.to_string()),
            commit_sha: Some(SHA.to_string()),
        }
    }

    fn sample_mr_payload() -> &'static str {
        r#"{
            "object_attributes": {
                "action": "open",
                "iid": 7,
                "title": "Fix login bug",
                "source_branch": "feature/login",
                "target_branch": "main",
                "url": "http://gitlab.internal:8929/group/proj/-/merge_requests/7",
                "last_commit": {"id": "abc123"},
                "author": {"name": "real-author", "avatar_url": "http://author-avatar"}
            },
            "project": {
                "path_with_namespace": "group/proj",
                "web_url": "http://gitlab.internal:8929/group/proj"
            },
            "user": {"name": "trigger-user", "avatar_url": "http://trigger-avatar"}
        }"#
    }

    #[tokio::test]
    async fn record_task_started_creates_running_entry_with_full_meta() {
        let store = TaskStore::new();
        let meta = full_source_meta();
        let id = record_task_started(&store, meta).await;

        let entry = store.get(id).await.expect("task must exist");
        assert_eq!(entry.state, TaskState::Running, "started review must be running");
        assert!(entry.started_at.is_some(), "started review must record started_at");
        assert_eq!(entry.source_meta.gitlab_mr_url.as_deref(), Some(MR_URL));
        assert_eq!(entry.source_meta.commit_sha.as_deref(), Some(SHA));
        assert_eq!(entry.source_meta.mr_title.as_deref(), Some("Fix login bug"));
        assert_eq!(entry.source_meta.project.as_deref(), Some("group/proj"));
        assert_eq!(entry.source_meta.branch.as_deref(), Some("feature/login"));
        assert_eq!(entry.source_meta.target_branch.as_deref(), Some("main"));
        assert_eq!(entry.source_meta.author_name.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn record_task_outcome_success_stores_review_output() {
        let store = TaskStore::new();
        let id = record_task_started(&store, full_source_meta()).await;
        let report = crate::models::ExpertReport {
            expert_name: "security".to_string(),
            findings: vec![],
            markdown: "## security review".to_string(),
            raw_llm_response: "raw".to_string(),
            parse_error: None,
            raw_dump_path: None,
        };
        let output = crate::models::ReviewOutput::new(vec![report]);
        let outcome: anyhow::Result<crate::models::ReviewOutput> = Ok(output);

        record_task_outcome(&store, id, &outcome).await;

        let entry = store.get(id).await.expect("task must exist");
        assert_eq!(entry.state, TaskState::Completed);
        assert!(entry.completed_at.is_some(), "completed task must record completed_at");
        let result = entry.result.expect("completed webhook task must carry a result");
        let reports = result["reports"].as_array().expect("result must contain reports");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["expert_name"], "security");
        assert!(entry.error.is_none());
    }

    #[tokio::test]
    async fn record_task_outcome_failure_marks_failed_with_error() {
        let store = TaskStore::new();
        let id = record_task_started(&store, full_source_meta()).await;
        let outcome: anyhow::Result<crate::models::ReviewOutput> = Err(anyhow::anyhow!("provider unreachable"));

        record_task_outcome(&store, id, &outcome).await;

        let entry = store.get(id).await.expect("task must exist");
        assert_eq!(entry.state, TaskState::Failed);
        assert!(entry.completed_at.is_some(), "failed task must record completed_at");
        assert!(entry.result.is_none());
        let error = entry.error.expect("failed task must carry an error message");
        assert!(error.contains("provider unreachable"), "got error: {error}");
    }

    /// The dispatcher dedups by URL+SHA before `spawn_mr_review_task`, so a
    /// single actually-started review must record exactly one entry through
    /// the full start → outcome cycle (never two).
    #[tokio::test]
    async fn full_cycle_records_exactly_one_entry() {
        let store = TaskStore::new();
        let id = record_task_started(&store, full_source_meta()).await;
        let outcome: anyhow::Result<crate::models::ReviewOutput> = Ok(crate::models::ReviewOutput::new(vec![]));
        record_task_outcome(&store, id, &outcome).await;

        let (items, total) = store.list(None, 1, 100, None, None, None, None, None).await;
        assert_eq!(total, 1, "one dispatch must record exactly one entry");
        assert_eq!(items[0].task_id, id);
        assert_eq!(items[0].state, TaskState::Completed);
        assert_eq!(items[0].source_meta.commit_sha.as_deref(), Some(SHA));
    }

    #[test]
    fn parse_mr_hook_payload_extracts_display_metadata() {
        let payload = parse_mr_hook_payload(sample_mr_payload(), "glpat-test").expect("payload must parse");

        assert_eq!(payload.action, "open");
        assert_eq!(payload.mr_iid, 7);
        assert_eq!(
            payload.mr_url,
            "http://gitlab.internal:8929/group/proj/-/merge_requests/7"
        );
        assert_eq!(payload.sha, "abc123");
        assert_eq!(payload.mr_title, "Fix login bug");
        assert_eq!(payload.path_with_namespace, "group/proj");
        assert_eq!(payload.source_branch, "feature/login");
        assert_eq!(payload.target_branch, "main");
        // MR author block is preferred over the trigger user.
        assert_eq!(payload.author_name, "real-author");
        assert_eq!(payload.author_avatar_url, "http://author-avatar");
    }

    #[test]
    fn parse_mr_hook_payload_falls_back_to_trigger_user_for_author() {
        let body = r#"{
            "object_attributes": {
                "action": "open",
                "iid": 7,
                "title": "Fix login bug",
                "source_branch": "feature/login",
                "target_branch": "main",
                "url": "http://gitlab.internal:8929/group/proj/-/merge_requests/7",
                "last_commit": {"id": "abc123"}
            },
            "project": {
                "path_with_namespace": "group/proj",
                "web_url": "http://gitlab.internal:8929/group/proj"
            },
            "user": {"name": "trigger-user", "avatar_url": "http://trigger-avatar"}
        }"#;
        let payload = parse_mr_hook_payload(body, "glpat-test").expect("payload must parse");

        assert_eq!(payload.author_name, "trigger-user");
        assert_eq!(payload.author_avatar_url, "http://trigger-avatar");
    }

    #[test]
    fn source_meta_from_payload_includes_title_project_author() {
        let payload = parse_mr_hook_payload(sample_mr_payload(), "glpat-test").unwrap();
        let meta = source_meta_from_payload(&payload);

        assert_eq!(meta.mr_title.as_deref(), Some("Fix login bug"));
        assert_eq!(meta.project.as_deref(), Some("group/proj"));
        assert_eq!(meta.repository.as_deref(), Some("group/proj"));
        assert_eq!(meta.branch.as_deref(), Some("feature/login"));
        assert_eq!(meta.target_branch.as_deref(), Some("main"));
        assert_eq!(meta.author_name.as_deref(), Some("real-author"));
        assert_eq!(meta.author_avatar_url.as_deref(), Some("http://author-avatar"));
        assert_eq!(meta.gitlab_mr_url.as_deref(), Some(MR_URL));
        assert_eq!(meta.commit_sha.as_deref(), Some(SHA));
    }
}
