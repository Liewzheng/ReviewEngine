use axum::{http::StatusCode, Json};
use serde_json::Value;
use std::sync::Arc;

use super::super::dispatcher::MrDispatcher;
use crate::server::api::review::discussion::DiscussionTap;
use crate::server::task_queue::{record_task_outcome, record_task_started, SourceMeta, TaskStore};
use crate::store::traits::{DiscussionNote, DiscussionStore};
use crate::store::SqlxStore;

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
    tap: Option<DiscussionTap>,
) {
    let task_id = if let Some(store) = task_store.as_ref() {
        Some(record_task_started(store, source_meta).await)
    } else {
        None
    };

    let outcome = async {
        let (mut info, diff) = super::super::resolve_review_source(&mr_url, &gitlab_token).await?;
        if let (Some(store), Some(id)) = (task_store.as_ref(), task_id) {
            store
                .fill_source_meta(id, crate::server::task_queue::source_meta_from_mr_info(&info))
                .await;
            // §7.2 discussion-context injection: best-effort, `None`
            // degrades to the 0.9 prompt. Requires the live task row
            // (`review_contexts.task_id` FK), hence tied to the task store.
            if let Some(tap) = tap.as_ref() {
                if let Some(section) = tap
                    .inject(id, &info.project_path, u64::from(info.mr_iid), &gitlab_token, &mr_url)
                    .await
                {
                    info.discussion_context = Some(section);
                }
            }
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
    tap: Option<DiscussionTap>,
) {
    let d = dispatcher.clone();
    tokio::spawn(async move {
        run_webhook_review(task_store, &d, mr_url, sha, gitlab_token, mr_iid, source_meta, tap).await;
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
    tap: Option<DiscussionTap>,
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
                tap,
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
    tap: Option<DiscussionTap>,
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
                tap,
            );
        }
        super::super::dispatcher::ShouldStart::AlreadyReviewed => {
            tracing::info!("Skipping MR !{}: already reviewed at SHA {}", mr_iid, sha);
        }
        super::super::dispatcher::ShouldStart::InProgress => {
            handle_mr_in_progress(
                dispatcher,
                mr_url,
                sha,
                gitlab_token,
                mr_iid,
                task_store,
                source_meta,
                tap,
            )
            .await;
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
pub(crate) fn review_base_url(platform: &crate::models::GitPlatformConfig) -> &str {
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
    db: Option<Arc<SqlxStore>>,
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

        // §7.2 discussion tap: the DB handle plus platform identity, built
        // against the (rewritten) review URL so the API fallback and the
        // self-echo guard target the reachable instance. `None` without a
        // DB → 0.9 behaviour.
        let tap = db.map(|db| DiscussionTap::new(db, platform.as_ref(), &review_url));

        dispatch_mr_event(
            dispatcher,
            &review_url,
            &payload.sha,
            &payload.gitlab_token,
            payload.mr_iid,
            task_store,
            source_meta,
            tap,
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
/// match, but `/reviewer` / `reviewxyz` do not. The command must be followed
/// by a path separator (`/`) or the end of the note, so prefix lookalikes
/// never trigger a review (`^/review(/|$)` semantics).
pub fn note_starts_with_command(note: &str, cmd: &str) -> bool {
    let Some(rest) = note.strip_prefix(cmd) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/')
}

/// True when a note body (already lowercased) is a `/review` or `/describe`
/// command. Command notes are user intent: they always ingest (§7.1) and
/// survive the self-echo filter of the discussion-context tap (§7.2).
pub(crate) fn is_command_note(body_lower: &str) -> bool {
    note_starts_with_command(body_lower, "/review") || note_starts_with_command(body_lower, "/describe")
}

/// True when `body` is one of our own published review reports (self-echo
/// guard (a) of §7.1).
pub(crate) fn is_self_report(body: &str) -> bool {
    body.starts_with(crate::publisher::REVIEW_REPORT_PREFIX)
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

// ─── 0.10.0 Note ingestion (design/persistence.md §7.1) ───

/// `object_attributes.created_at` from a note webhook. GitLab sends either
/// RFC 3339 or its legacy `"2026-09-03 10:00:00 UTC"` format; both decode to
/// UTC. `None` when absent/unparseable (the caller falls back to now()).
pub(crate) fn parse_note_created_at(raw: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = raw?;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S UTC")
        .ok()
        .map(|naive| naive.and_utc())
}

/// `scheme://host[:port]` of a URL — the instance root for instance-level
/// API calls derived from a payload URL.
pub(crate) fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(format!("{scheme}://{host}"))
    }
}

/// Self-echo guard (b) of §7.1: the GitLab user id this service's token
/// posts as, per platform key ("default" for the unmatched/runtime-token
/// path). Resolved LAZILY (GET /user) and cached for the process lifetime;
/// only successes are cached, so a transient API failure retries on the
/// next call instead of permanently disabling the guard. The platform set
/// is runtime-mutable (PUT /config), which is why this is not resolved once
/// at startup. Shared with the §7.2 discussion-context tap (same guard
/// applies to API-fallback notes). `None` when the token or instance base
/// is empty, or the lookup fails — the guard is inactive, never an error.
static SELF_USER_IDS: std::sync::OnceLock<tokio::sync::Mutex<std::collections::HashMap<String, u64>>> =
    std::sync::OnceLock::new();

pub(crate) async fn self_user_id_cached(platform_name: &str, gitlab_token: &str, instance_base: &str) -> Option<u64> {
    if gitlab_token.is_empty() || instance_base.is_empty() {
        return None;
    }
    let cache = SELF_USER_IDS.get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(id) = cache.lock().await.get(platform_name) {
        return Some(*id);
    }
    let client = crate::git_provider::gitlab::client::Client::for_instance(gitlab_token, instance_base);
    match client.get_current_user_id().await {
        Ok(id) => {
            cache.lock().await.insert(platform_name.to_string(), id);
            Some(id)
        }
        Err(e) => {
            tracing::warn!(
                "could not resolve the service's own GitLab user id ({platform_name}): {e:#}; \
                 self-echo guard (b) inactive for this call"
            );
            None
        }
    }
}

/// Resolve the instance base + platform key for a note webhook payload, then
/// delegate to [`self_user_id_cached`]. The instance to ask: the matched
/// platform's reachable base (internal when configured, mirroring the review
/// URL rewrite), else the origin of the payload's own URLs.
async fn resolve_self_user_id(
    platform: Option<&crate::models::GitPlatformConfig>,
    gitlab_token: &str,
    parsed: &Value,
) -> Option<u64> {
    if gitlab_token.is_empty() {
        return None;
    }
    let key = platform
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "default".to_string());
    let instance_base = match platform {
        Some(p) => review_base_url(p).to_string(),
        None => {
            let url = parsed["project"]["web_url"]
                .as_str()
                .or_else(|| parsed["project"]["homepage"].as_str())
                .or_else(|| parsed["object_attributes"]["url"].as_str())?;
            url_origin(url)?
        }
    };
    self_user_id_cached(&key, gitlab_token, &instance_base).await
}

/// Persist one note-webhook payload into `mr_discussions` (§7.1).
/// Best-effort: every failure is logged and swallowed — note ingestion is an
/// enhancement, never a reason to fail the hook. Skips: non-note payloads,
/// non-MR notes (Commit/Issue/Snippet), system notes, and our own output
/// (self-echo guard) — except `/review` / `/describe` command notes, which
/// are user intent and always ingest.
async fn ingest_note(
    db: &SqlxStore,
    parsed: &Value,
    platform: Option<&crate::models::GitPlatformConfig>,
    gitlab_token: &str,
) {
    if parsed["object_kind"].as_str() != Some("note") {
        return;
    }
    let attrs = &parsed["object_attributes"];
    if attrs["noteable_type"].as_str() != Some("MergeRequest") {
        return;
    }
    // System notes ("added 1 commit") are noise, not discussion (§7.1).
    if attrs["system"].as_bool() == Some(true) {
        return;
    }
    let Some(note_id) = attrs["id"].as_u64() else {
        return;
    };
    let project = parsed["project"]["path_with_namespace"].as_str().unwrap_or("");
    // `merge_request.iid`, falling back to the `object_attributes.url` tail
    // (system-hook notes may omit the merge_request block).
    let mr_iid = parsed["merge_request"]["iid"]
        .as_u64()
        .or_else(|| attrs["url"].as_str().and_then(mr_iid_from_url));
    let Some(mr_iid) = mr_iid.filter(|_| !project.is_empty()) else {
        tracing::debug!("note hook ingestion skipped: no MR iid or project path");
        return;
    };

    let body = attrs["note"].as_str().unwrap_or("");
    let is_command = is_command_note(&body.to_lowercase());
    if !is_command {
        // (a) our own published review report.
        if is_self_report(body) {
            tracing::debug!("note hook ingestion skipped: self-published review report");
            return;
        }
        // (b) a note authored by the service's own GitLab account.
        if let Some(self_id) = resolve_self_user_id(platform, gitlab_token, parsed).await {
            if parsed["user"]["id"].as_u64() == Some(self_id) {
                tracing::debug!("note hook ingestion skipped: note authored by the service itself");
                return;
            }
        }
    }

    let author = parsed["user"]["username"]
        .as_str()
        .or_else(|| parsed["user"]["name"].as_str())
        .unwrap_or("");
    let created_at = parse_note_created_at(attrs["created_at"].as_str()).unwrap_or_else(|| {
        tracing::warn!("note {note_id}: unparseable created_at, using ingestion time");
        chrono::Utc::now()
    });
    let note = DiscussionNote {
        platform: platform
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "default".to_string()),
        project: project.to_string(),
        mr_iid,
        note_id,
        author: author.to_string(),
        body: body.to_string(),
        created_at,
    };
    if let Err(e) = db.upsert_note(&note).await {
        tracing::error!("failed to persist MR discussion note {note_id} for {project} !{mr_iid}: {e:#}");
    }
}

pub async fn handle_note_hook(
    body: &str,
    dispatcher: &MrDispatcher,
    gitlab_token: &str,
    platform: Option<crate::models::GitPlatformConfig>,
    task_store: Option<Arc<TaskStore>>,
    db: Option<Arc<SqlxStore>>,
) -> Result<Json<Value>, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse Note hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    // 0.10.0 (design/persistence.md §7.1): persist the note into
    // mr_discussions BEFORE the command check below — command notes are
    // discussion history too. Ingestion is best-effort: a failure is logged,
    // never fails the hook, and db=None keeps the 0.9 behaviour exactly.
    if let Some(db) = &db {
        ingest_note(db, &parsed, platform.as_ref(), gitlab_token).await;
    }

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
                    // §7.2 discussion tap (same wiring as the MR hook).
                    let tap = db.clone().map(|db| DiscussionTap::new(db, platform.as_ref(), &u));
                    tokio::spawn(async move {
                        run_webhook_review(task_store, &d, u, s, token, note_iid, source_meta, tap).await;
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

    // ─── 0.10.0 note ingestion (design/persistence.md §7.1) ───

    async fn fresh_db() -> Arc<SqlxStore> {
        let db = Arc::new(SqlxStore::new_in_memory().await.unwrap());
        db.migrate().await.unwrap();
        db
    }

    fn note_payload(note_id: u64, body: &str, user_id: u64, username: &str) -> String {
        serde_json::json!({
            "object_kind": "note",
            "object_attributes": {
                "id": note_id,
                "note": body,
                "noteable_type": "MergeRequest",
                "created_at": "2026-09-03 10:00:00 UTC",
                "url": format!("http://gitlab.internal/group/proj/-/merge_requests/7#note_{note_id}"),
                "system": false
            },
            "merge_request": {"iid": 7},
            "project": {"path_with_namespace": "group/proj", "web_url": "http://gitlab.internal/group/proj"},
            "user": {"id": user_id, "username": username, "name": username}
        })
        .to_string()
    }

    async fn note_count(db: &SqlxStore) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM mr_discussions")
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

    /// Fire a note hook whose response is discarded (Json is #[must_use]).
    async fn fire_note(
        payload: &str,
        dispatcher: &MrDispatcher,
        token: &str,
        platform: Option<crate::models::GitPlatformConfig>,
        db: &Arc<SqlxStore>,
    ) {
        let _ = handle_note_hook(payload, dispatcher, token, platform, None, Some(db.clone()))
            .await
            .unwrap();
    }

    /// (a) a plain MR note is persisted with all fields, project from
    /// `path_with_namespace`, author from `user.username`, platform
    /// "default" when no platform matched.
    #[tokio::test]
    async fn note_ingestion_persists_fields() {
        let db = fresh_db().await;
        let dispatcher = MrDispatcher::new();
        let resp = handle_note_hook(
            &note_payload(1234, "LGTM, ship it", 42, "alice"),
            &dispatcher,
            "",
            None,
            None,
            Some(db.clone()),
        )
        .await
        .expect("hook must succeed");
        assert_eq!(resp["status"], "received");

        let notes = db.list_notes("default", "group/proj", 7).await.unwrap();
        assert_eq!(notes.len(), 1);
        let note = &notes[0];
        assert_eq!(note.note_id, 1234);
        assert_eq!(note.body, "LGTM, ship it");
        assert_eq!(note.author, "alice");
        assert_eq!(note.platform, "default");
        assert_eq!(note.project, "group/proj");
        assert_eq!(note.mr_iid, 7);
        assert_eq!(
            note.created_at,
            chrono::DateTime::parse_from_rfc3339("2026-09-03T10:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            "GitLab legacy timestamp format decodes to UTC"
        );
        // ingested_at is stamped on insert.
        let ingested: String = sqlx::query_scalar("SELECT ingested_at FROM mr_discussions WHERE note_id = 1234")
            .fetch_one(db.pool())
            .await
            .unwrap();
        crate::store::decode_ts(&ingested).unwrap();
    }

    /// Matched platform supplies the `platform` column; the MR iid falls
    /// back to the `object_attributes.url` tail when merge_request is absent.
    #[tokio::test]
    async fn note_ingestion_platform_name_and_iid_fallback() {
        let db = fresh_db().await;
        let dispatcher = MrDispatcher::new();
        let platform = crate::models::GitPlatformConfig {
            name: "internal".to_string(),
            base_url: "http://gitlab.internal".to_string(),
            ..Default::default()
        };
        let mut payload: Value = serde_json::from_str(&note_payload(55, "looks fine", 42, "bob")).unwrap();
        payload.as_object_mut().unwrap().remove("merge_request");
        fire_note(&payload.to_string(), &dispatcher, "", Some(platform), &db).await;
        let notes = db.list_notes("internal", "group/proj", 7).await.unwrap();
        assert_eq!(notes.len(), 1, "iid recovered from the note URL tail");
        assert_eq!(notes[0].platform, "internal");
    }

    /// (b) webhook redelivery dedups on the primary key; (c) an edited note
    /// (same note_id) updates the body in place.
    #[tokio::test]
    async fn note_ingestion_idempotent_redelivery_and_edit() {
        let db = fresh_db().await;
        let dispatcher = MrDispatcher::new();

        let body = note_payload(1234, "first version", 42, "alice");
        fire_note(&body, &dispatcher, "", None, &db).await;
        fire_note(&body, &dispatcher, "", None, &db).await;
        assert_eq!(note_count(&db).await, 1, "redelivery must dedup");

        let edited = note_payload(1234, "edited body", 42, "alice");
        fire_note(&edited, &dispatcher, "", None, &db).await;
        assert_eq!(note_count(&db).await, 1, "edit must update in place");
        let notes = db.list_notes("default", "group/proj", 7).await.unwrap();
        assert_eq!(notes[0].body, "edited body");
    }

    /// (d) non-MR notes (Commit/Issue/Snippet) are ignored.
    #[tokio::test]
    async fn note_ingestion_ignores_non_mr_notes() {
        let db = fresh_db().await;
        let dispatcher = MrDispatcher::new();
        for noteable in ["Commit", "Issue", "Snippet"] {
            let mut payload: Value = serde_json::from_str(&note_payload(9, "note", 42, "alice")).unwrap();
            payload["object_attributes"]["noteable_type"] = serde_json::json!(noteable);
            fire_note(&payload.to_string(), &dispatcher, "", None, &db).await;
        }
        assert_eq!(note_count(&db).await, 0);
    }

    /// (e) system notes ("added 1 commit") are noise and skipped.
    #[tokio::test]
    async fn note_ingestion_skips_system_notes() {
        let db = fresh_db().await;
        let dispatcher = MrDispatcher::new();
        let mut payload: Value = serde_json::from_str(&note_payload(9, "added 1 commit", 42, "alice")).unwrap();
        payload["object_attributes"]["system"] = serde_json::json!(true);
        fire_note(&payload.to_string(), &dispatcher, "", None, &db).await;
        assert_eq!(note_count(&db).await, 0);
    }

    /// (f) self-echo guard: our published report prefix and our own author
    /// id are skipped; a /review command note is user intent and ingests
    /// even when it hits both guards.
    #[tokio::test]
    async fn note_ingestion_self_echo_guard_and_command_exception() {
        let db = fresh_db().await;
        let dispatcher = MrDispatcher::new();

        // (a) report prefix.
        let report = format!("{}\nreview body", crate::publisher::REVIEW_REPORT_PREFIX);
        fire_note(&note_payload(1, &report, 42, "review-bot"), &dispatcher, "", None, &db).await;
        assert_eq!(note_count(&db).await, 0, "our own report must not be ingested");

        // (b) self-author. Seed the per-platform cache directly (unique
        // platform name — no network, no cross-test interference).
        let platform = crate::models::GitPlatformConfig {
            name: "self-echo-test".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            ..Default::default()
        };
        SELF_USER_IDS
            .get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
            .await
            .insert("self-echo-test".to_string(), 4242);

        fire_note(
            &note_payload(2, "inline comment by the bot", 4242, "review-bot"),
            &dispatcher,
            "glpat-self-echo-test",
            Some(platform.clone()),
            &db,
        )
        .await;
        assert_eq!(note_count(&db).await, 0, "note by our own user id must be skipped");

        // A DIFFERENT user on the same platform ingests fine (cache hit, no
        // network).
        fire_note(
            &note_payload(3, "human comment", 777, "carol"),
            &dispatcher,
            "glpat-self-echo-test",
            Some(platform.clone()),
            &db,
        )
        .await;
        assert_eq!(note_count(&db).await, 1);

        // Command exception: a /review note from our own account is still
        // user intent → ingested. Empty token keeps the command branch from
        // dispatching (the guard exception bypasses self-id resolution
        // entirely, so the token value is irrelevant to this assertion).
        fire_note(
            &note_payload(4, "/review", 4242, "review-bot"),
            &dispatcher,
            "",
            Some(platform),
            &db,
        )
        .await;
        let notes = db.list_notes("self-echo-test", "group/proj", 7).await.unwrap();
        assert_eq!(notes.len(), 2, "human comment + command note");
        assert!(
            notes.iter().any(|n| n.body == "/review"),
            "command note must be ingested"
        );
    }

    /// (g) db=None: the hook behaves exactly like 0.9 — no ingestion, no
    /// error.
    #[tokio::test]
    async fn note_hook_without_db_is_0_9_behaviour() {
        let dispatcher = MrDispatcher::new();
        let resp = handle_note_hook(&note_payload(1, "LGTM", 42, "alice"), &dispatcher, "", None, None, None)
            .await
            .expect("hook must succeed without a DB");
        assert_eq!(resp["status"], "received");
    }

    #[test]
    fn parse_note_created_at_accepts_gitlab_formats() {
        let legacy = parse_note_created_at(Some("2026-09-03 10:00:00 UTC")).unwrap();
        assert_eq!(legacy.to_rfc3339(), "2026-09-03T10:00:00+00:00");
        let rfc = parse_note_created_at(Some("2026-09-03T10:00:00Z")).unwrap();
        assert_eq!(legacy, rfc);
        assert!(parse_note_created_at(Some("not a date")).is_none());
        assert!(parse_note_created_at(None).is_none());
    }
}
