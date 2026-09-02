use axum::{http::StatusCode, Json};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;

use super::dispatcher::MrDispatcher;
use super::task_queue::{record_task_outcome, record_task_started, SourceMeta, TaskStore};
use super::webhook::WebhookHandler;

use async_trait::async_trait;

use axum::http::HeaderMap;

type HmacSha256 = Hmac<Sha256>;

/// GitHub webhook handler.
#[derive(Clone)]
pub struct GitHubWebhookHandler {
    pub webhook_secret: String,
    pub dispatcher: MrDispatcher,
    pub token: String,
    /// Optional task store for recording webhook-dispatched review lifecycle.
    /// Populated via [`Self::with_app_state`] in server startup; tests and
    /// legacy paths leave it `None` and fall back to the legacy run-only behavior.
    task_store: Option<Arc<TaskStore>>,
}

impl GitHubWebhookHandler {
    /// Create a new GitHub webhook handler.
    pub fn new(webhook_secret: String, dispatcher: MrDispatcher, token: String) -> Self {
        Self {
            webhook_secret,
            dispatcher,
            token,
            task_store: None,
        }
    }

    /// Attach the server's shared state so the handler can record review tasks.
    pub fn with_app_state(mut self, state: &Arc<crate::server::AppState>) -> Self {
        self.task_store = state.task_store.clone();
        self
    }
}

#[async_trait]
impl WebhookHandler for GitHubWebhookHandler {
    fn path(&self) -> &'static str {
        "/webhook/github"
    }

    fn name(&self) -> &'static str {
        "github"
    }

    async fn verify(&self, headers: &HeaderMap, body: &str) -> Result<(), (StatusCode, Json<Value>)> {
        let signature_raw = headers.get("X-Hub-Signature-256");

        if signature_raw.is_none() {
            tracing::warn!("GitHub webhook missing X-Hub-Signature-256 header");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "missing signature header"})),
            ));
        }

        let signature_str = signature_raw.and_then(|v| v.to_str().ok()).unwrap_or("");

        let signature = if let Some(s) = signature_str.strip_prefix("sha256=") {
            s
        } else {
            tracing::warn!("GitHub webhook signature does not start with sha256=: {signature_str}");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid signature format"})),
            ));
        };

        if hex::decode(signature).is_err() {
            tracing::warn!("GitHub webhook signature is not valid hex");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid signature encoding"})),
            ));
        }

        if !verify_signature(&self.webhook_secret, body, signature) {
            tracing::warn!("GitHub webhook HMAC signature mismatch — check GITHUB_WEBHOOK_SECRET");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid signature"})),
            ));
        }

        Ok(())
    }

    async fn handle_event(&self, headers: &HeaderMap, body: &str) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        let event = headers
            .get("X-GitHub-Event")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let result = match event {
            "ping" => {
                tracing::info!("GitHub ping event received");
                Ok(Json(serde_json::json!({ "status": "ok" })))
            }
            "pull_request" => handle_pull_request(&body, &self.dispatcher, &self.token, self.task_store.clone())
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            "issue_comment" => handle_issue_comment(&body, &self.dispatcher, &self.token, self.task_store.clone())
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            "push" => {
                tracing::info!("GitHub push event received");
                Ok(Json(serde_json::json!({ "status": "received" })))
            }
            _ => {
                tracing::debug!("Ignoring unsupported GitHub event: {}", event);
                Ok(Json(serde_json::json!({ "status": "ignored" })))
            }
        };

        result
    }
}

/// Verify the X-Hub-Signature-256 header.
fn verify_signature(secret: &str, body: &str, signature: &str) -> bool {
    let decoded = match hex::decode(signature) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body.as_bytes());
    mac.verify_slice(&decoded).is_ok()
}

/// Parsed payload from a GitHub Pull Request webhook event.
pub struct PrHookPayload {
    pub action: String,
    pub pr_url: String,
    pub pr_number: u64,
    pub sha: String,
    pub title: String,
    pub repo_full_name: String,
    pub head_branch: String,
    pub target_branch: String,
    pub author_login: String,
    pub author_avatar_url: String,
}

/// Parse and validate a GitHub PR webhook body into its essential fields.
pub fn parse_pr_hook_payload(body: &str) -> Result<PrHookPayload, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse PR hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let action = parsed["action"].as_str().unwrap_or("").to_string();
    let pr_number = parsed["pull_request"]["number"].as_u64().unwrap_or(0);
    let repo_full_name = parsed["repository"]["full_name"].as_str().unwrap_or("").to_string();
    let pr_url = if !repo_full_name.is_empty() && pr_number > 0 {
        format!("https://github.com/{}/pull/{}", repo_full_name, pr_number)
    } else {
        String::new()
    };
    let sha = parsed["pull_request"]["head"]["sha"].as_str().unwrap_or("").to_string();
    let title = parsed["pull_request"]["title"].as_str().unwrap_or("").to_string();
    let head_branch = parsed["pull_request"]["head"]["ref"].as_str().unwrap_or("").to_string();
    let target_branch = parsed["pull_request"]["base"]["ref"].as_str().unwrap_or("").to_string();
    let author_login = parsed["pull_request"]["user"]["login"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let author_avatar_url = parsed["pull_request"]["user"]["avatar_url"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok(PrHookPayload {
        action,
        pr_url,
        pr_number,
        sha,
        title,
        repo_full_name,
        head_branch,
        target_branch,
        author_login,
        author_avatar_url,
    })
}

/// Build a [`SourceMeta`] from the parsed GitHub PR webhook payload.
pub(crate) fn source_meta_from_pr_payload(payload: &PrHookPayload) -> SourceMeta {
    fn non_empty(s: &str) -> Option<String> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
    let project = non_empty(&payload.repo_full_name);
    SourceMeta {
        mr_title: non_empty(&payload.title),
        project: project.clone(),
        repository: project,
        branch: non_empty(&payload.head_branch),
        target_branch: non_empty(&payload.target_branch),
        author_name: non_empty(&payload.author_login),
        author_avatar_url: non_empty(&payload.author_avatar_url),
        gitlab_mr_url: non_empty(&payload.pr_url),
        commit_sha: non_empty(&payload.sha),
    }
}

/// Execute a webhook-dispatched PR review on a detached task, recording its
/// lifecycle in the task store when one is available.
async fn run_webhook_pr_review(
    task_store: Option<Arc<TaskStore>>,
    dispatcher: &MrDispatcher,
    pr_url: String,
    sha: String,
    github_token: String,
    pr_number: u64,
    source_meta: SourceMeta,
) {
    let task_id = if let Some(store) = task_store.as_ref() {
        Some(record_task_started(store, source_meta).await)
    } else {
        None
    };

    let outcome = async {
        let (info, diff) = super::resolve_review_source(&pr_url, &github_token).await?;
        if let (Some(store), Some(id)) = (task_store.as_ref(), task_id) {
            store
                .fill_source_meta(id, crate::server::task_queue::source_meta_from_mr_info(&info))
                .await;
        }
        super::run_review_common(
            &pr_url,
            &github_token,
            Some(dispatcher),
            Some(&pr_url),
            Some(&sha),
            info,
            diff,
        )
        .await
    }
    .await;

    if let Err(e) = &outcome {
        tracing::error!("Review failed for PR #{}: {:?}", pr_number, e);
        dispatcher.reset(&pr_url).await;
    }

    if let (Some(store), Some(id)) = (task_store.as_ref(), task_id) {
        record_task_outcome(store, id, &outcome).await;
    }
}

async fn handle_pull_request(
    body: &str,
    dispatcher: &MrDispatcher,
    github_token: &str,
    task_store: Option<Arc<TaskStore>>,
) -> Result<Json<Value>, StatusCode> {
    let payload = parse_pr_hook_payload(body)?;

    tracing::info!("GitHub PR #{} webhook: action={}", payload.pr_number, payload.action);

    let github_token = github_token.to_string();

    if payload.action == "opened" || payload.action == "reopened" || payload.action == "synchronize" {
        if payload.pr_url.is_empty() || github_token.is_empty() || payload.sha.is_empty() {
            tracing::warn!("Skipping: missing PR URL, GITHUB_TOKEN, or SHA");
            return Ok(Json(serde_json::json!({"status": "skipped"})));
        }

        let source_meta = source_meta_from_pr_payload(&payload);

        match dispatcher.try_start(&payload.pr_url, &payload.sha).await {
            super::dispatcher::ShouldStart::Go => {
                let d = dispatcher.clone();
                let u = payload.pr_url.clone();
                let s = payload.sha.clone();
                let token = github_token.clone();
                let ts = task_store.clone();
                let note_iid = payload.pr_number;
                tokio::spawn(async move {
                    run_webhook_pr_review(ts, &d, u, s, token, note_iid, source_meta).await;
                });
            }
            super::dispatcher::ShouldStart::AlreadyReviewed => {
                tracing::info!(
                    "Skipping PR #{}: already reviewed at SHA {}",
                    payload.pr_number,
                    payload.sha
                );
            }
            super::dispatcher::ShouldStart::InProgress => {
                tracing::info!("PR #{} review in progress, waiting...", payload.pr_number);
                dispatcher.wait(&payload.pr_url).await;
                match dispatcher.try_start(&payload.pr_url, &payload.sha).await {
                    super::dispatcher::ShouldStart::Go => {
                        let d = dispatcher.clone();
                        let u = payload.pr_url.clone();
                        let s = payload.sha.clone();
                        let token = github_token.clone();
                        let ts = task_store.clone();
                        let note_iid = payload.pr_number;
                        tokio::spawn(async move {
                            run_webhook_pr_review(ts, &d, u, s, token, note_iid, source_meta).await;
                        });
                    }
                    _ => {
                        tracing::info!("No new review needed for PR #{} after wait", payload.pr_number);
                    }
                }
            }
        }
    }

    Ok(Json(
        serde_json::json!({ "status": "received", "action": payload.action }),
    ))
}

/// True when `note` (already lowercased) begins with a slash command whose
/// first path segment is exactly `cmd` — i.e. `/review` and `/review/123`
/// match, but `/reviewer` / `/reviewxyz` do not. The command must be followed
/// by a path separator (`/`) or the end of the note, so prefix lookalikes
/// never trigger a review (`^/review(/|$)` semantics).
fn note_starts_with_command(note: &str, cmd: &str) -> bool {
    let Some(rest) = note.strip_prefix(cmd) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/')
}

async fn handle_issue_comment(
    body: &str,
    dispatcher: &MrDispatcher,
    github_token: &str,
    task_store: Option<Arc<TaskStore>>,
) -> Result<Json<Value>, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse issue_comment webhook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let note = parsed["comment"]["body"].as_str().unwrap_or("");
    let note_lower = note.to_lowercase();

    // Check for commands like /review, /describe. Matched on a path-segment
    // boundary so `/reviewer` / `/reviewxyz` never trigger a review.
    if note_starts_with_command(&note_lower, "/review") || note_starts_with_command(&note_lower, "/describe") {
        let repo_full = parsed["repository"]["full_name"].as_str().unwrap_or("");
        let pr_number = parsed["issue"]["number"].as_u64().unwrap_or(0);
        let pr_url = if !repo_full.is_empty() && pr_number > 0 {
            format!("https://github.com/{}/pull/{}", repo_full, pr_number)
        } else {
            String::new()
        };
        let github_token = github_token.to_string();
        let sha = format!("cmd_{}", uuid::Uuid::new_v4());

        if !pr_url.is_empty() && !github_token.is_empty() {
            let project = (!repo_full.is_empty()).then_some(repo_full.to_string());
            let source_meta = SourceMeta {
                project: project.clone(),
                repository: project,
                gitlab_mr_url: Some(pr_url.clone()),
                commit_sha: Some(sha.clone()),
                ..SourceMeta::default()
            };

            match dispatcher.try_start(&pr_url, &sha).await {
                super::dispatcher::ShouldStart::Go => {
                    let d = dispatcher.clone();
                    let u = pr_url;
                    let s = sha;
                    let token = github_token;
                    let ts = task_store.clone();
                    tokio::spawn(async move {
                        run_webhook_pr_review(ts, &d, u, s, token, pr_number, source_meta).await;
                    });
                }
                _ => {
                    tracing::info!("Comment review skipped or already in progress");
                }
            }
        }
    }

    Ok(Json(serde_json::json!({ "status": "received" })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_signature_valid() {
        let secret = "my-secret";
        let body = r#"{"action":"opened","number":1}"#;
        // Compute expected HMAC using same algorithm
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let expected = hex::encode(mac.finalize().into_bytes());
        assert!(verify_signature(secret, body, &expected));
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let body = r#"{"action":"opened"}"#;
        let mut mac = HmacSha256::new_from_slice(b"other-secret").unwrap();
        mac.update(body.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        assert!(!verify_signature("my-secret", body, &sig));
    }

    #[test]
    fn test_verify_signature_invalid_hex() {
        assert!(!verify_signature("secret", "body", "not-hex"));
    }

    #[test]
    fn test_verify_signature_empty_secret() {
        let body = "test";
        assert!(!verify_signature("", body, "abc123"));
    }

    #[test]
    fn test_verify_signature_empty_body() {
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(b"");
        let sig = hex::encode(mac.finalize().into_bytes());
        assert!(verify_signature("secret", "", &sig));
    }

    #[test]
    fn test_verify_signature_tampered_body() {
        let secret = "my-secret";
        let body = r#"{"action":"opened"}"#;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        // Different body should not match
        assert!(!verify_signature(secret, r#"{"action":"closed"}"#, &sig));
    }

    #[test]
    fn test_handler_creation() {
        let handler =
            GitHubWebhookHandler::new("test-secret".to_string(), MrDispatcher::new(), "test-token".to_string());
        assert_eq!(handler.path(), "/webhook/github");
        assert_eq!(handler.name(), "github");
    }

    // ── Payload parsing and source metadata ────────────────────────────

    fn sample_pr_payload() -> &'static str {
        r#"{
            "action": "opened",
            "pull_request": {
                "number": 42,
                "title": "Fix login bug",
                "head": {
                    "sha": "abc123def456",
                    "ref": "feature/login"
                },
                "base": {
                    "ref": "main"
                },
                "user": {
                    "login": "alice",
                    "avatar_url": "http://avatar/alice"
                }
            },
            "repository": {
                "full_name": "owner/repo"
            }
        }"#
    }

    #[test]
    fn test_parse_pr_hook_payload_extracts_display_metadata() {
        let payload = parse_pr_hook_payload(sample_pr_payload()).expect("payload must parse");

        assert_eq!(payload.action, "opened");
        assert_eq!(payload.pr_number, 42);
        assert_eq!(payload.pr_url, "https://github.com/owner/repo/pull/42");
        assert_eq!(payload.sha, "abc123def456");
        assert_eq!(payload.title, "Fix login bug");
        assert_eq!(payload.repo_full_name, "owner/repo");
        assert_eq!(payload.head_branch, "feature/login");
        assert_eq!(payload.author_login, "alice");
        assert_eq!(payload.author_avatar_url, "http://avatar/alice");
    }

    #[test]
    fn test_source_meta_from_pr_payload_includes_title_project_author() {
        let payload = parse_pr_hook_payload(sample_pr_payload()).unwrap();
        let meta = source_meta_from_pr_payload(&payload);

        assert_eq!(meta.mr_title.as_deref(), Some("Fix login bug"));
        assert_eq!(meta.project.as_deref(), Some("owner/repo"));
        assert_eq!(meta.repository.as_deref(), Some("owner/repo"));
        assert_eq!(meta.branch.as_deref(), Some("feature/login"));
        assert_eq!(meta.target_branch.as_deref(), Some("main"));
        assert_eq!(meta.author_name.as_deref(), Some("alice"));
        assert_eq!(meta.author_avatar_url.as_deref(), Some("http://avatar/alice"));
        assert_eq!(
            meta.gitlab_mr_url.as_deref(),
            Some("https://github.com/owner/repo/pull/42")
        );
        assert_eq!(meta.commit_sha.as_deref(), Some("abc123def456"));
    }

    #[tokio::test]
    async fn record_task_started_creates_running_entry_with_full_meta() {
        use crate::server::task_queue::{record_task_started, TaskStore};

        let store = TaskStore::new();
        let meta = source_meta_from_pr_payload(&parse_pr_hook_payload(sample_pr_payload()).unwrap());
        let id = record_task_started(&store, meta).await;

        let entry = store.get(id).await.expect("task must exist");
        assert_eq!(entry.state, crate::server::task_queue::TaskState::Running);
        assert_eq!(entry.source_meta.mr_title.as_deref(), Some("Fix login bug"));
        assert_eq!(entry.source_meta.project.as_deref(), Some("owner/repo"));
        assert_eq!(entry.source_meta.author_name.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn record_task_outcome_success_stores_review_output() {
        use crate::server::task_queue::{record_task_outcome, record_task_started, TaskStore};

        let store = TaskStore::new();
        let meta = source_meta_from_pr_payload(&parse_pr_hook_payload(sample_pr_payload()).unwrap());
        let id = record_task_started(&store, meta).await;
        let report = crate::models::ExpertReport {
            expert_name: "security".to_string(),
            findings: vec![],
            markdown: "## security review".to_string(),
            raw_llm_response: "raw".to_string(),
            parse_error: None,
            raw_dump_path: None,
        };
        let outcome: anyhow::Result<crate::models::ReviewOutput> = Ok(crate::models::ReviewOutput::new(vec![report]));

        record_task_outcome(&store, id, &outcome).await;

        let entry = store.get(id).await.expect("task must exist");
        assert_eq!(entry.state, crate::server::task_queue::TaskState::Completed);
        let result = entry.result.expect("result must exist");
        let reports = result["reports"].as_array().expect("reports must be an array");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["expert_name"], "security");
    }

    // ── Issue-comment command matching (HIGH: prefix lookalike regression) ─

    #[test]
    fn test_note_command_matches_exact_and_path_prefix() {
        // `/review` alone triggers.
        assert!(note_starts_with_command("/review", "/review"));
        // `/review/123` — a path segment after the command — triggers.
        assert!(note_starts_with_command("/review/123", "/review"));
        assert!(note_starts_with_command("/review/123 details", "/review"));
        // Prefix lookalikes must NOT trigger.
        assert!(!note_starts_with_command("/reviewer", "/review"));
        assert!(!note_starts_with_command("/reviewer/456", "/review"));
        assert!(!note_starts_with_command("/reviewxyz", "/review"));
        // A command followed by a space is not a path-segment boundary: no trigger.
        assert!(!note_starts_with_command("/review @someone", "/review"));
        // `/describe` shares the same boundary semantics.
        assert!(note_starts_with_command("/describe", "/describe"));
        assert!(note_starts_with_command("/describe/foo", "/describe"));
        assert!(!note_starts_with_command("/describefoo", "/describe"));
        // Not a command at all.
        assert!(!note_starts_with_command("review this", "/review"));
        assert!(!note_starts_with_command("", "/review"));
        assert!(!note_starts_with_command("needs-review", "/review"));
    }
}
