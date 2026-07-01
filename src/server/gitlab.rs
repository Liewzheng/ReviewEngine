use axum::{extract::State, http::StatusCode, routing::post, Json};
use serde_json::Value;

use super::dispatcher::MrDispatcher;

/// Shared state for GitLab webhook handling.
#[derive(Clone)]
pub struct GitLabWebhookState {
    pub webhook_secret: String,
    pub dispatcher: MrDispatcher,
}

/// Register GitLab webhook routes on the given router.
pub fn routes() -> axum::Router<GitLabWebhookState> {
    axum::Router::new().route("/webhook/gitlab", post(handle_webhook))
}

/// Handle incoming GitLab webhook events.
async fn handle_webhook(
    State(state): State<GitLabWebhookState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Result<Json<Value>, StatusCode> {
    // Verify X-Gitlab-Token
    let token = headers
        .get("X-Gitlab-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if token != state.webhook_secret {
        tracing::warn!("Webhook received with invalid token");
        return Err(StatusCode::FORBIDDEN);
    }

    // Parse event type from headers
    let event = headers
        .get("X-Gitlab-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match event {
        "Merge Request Hook" => handle_mr_hook(&body, &state.dispatcher).await,
        "Note Hook" => handle_note_hook(&body, &state.dispatcher).await,
        "Push Hook" => handle_push_hook(&body).await,
        _ => {
            tracing::debug!("Ignoring unsupported event: {}", event);
            Ok(Json(serde_json::json!({ "status": "ignored" })))
        }
    }
}

/// Parsed payload from a GitLab Merge Request webhook event.
struct MrHookPayload {
    action: String,
    mr_url: String,
    mr_iid: u64,
    sha: String,
    gitlab_token: String,
}

/// Parse and validate an MR webhook body into its essential fields.
fn parse_mr_hook_payload(body: &str) -> Result<MrHookPayload, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse MR hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let action = parsed["object_attributes"]["action"].as_str().unwrap_or("").to_string();
    let project_url = parsed["project"]["web_url"].as_str().unwrap_or("").to_string();
    let mr_iid = parsed["object_attributes"]["iid"].as_u64().unwrap_or(0);
    let mr_url = if !project_url.is_empty() && mr_iid > 0 {
        format!("{}/-/merge_requests/{}", project_url, mr_iid)
    } else {
        String::new()
    };
    let sha = parsed["object_attributes"]["last_commit"]["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let gitlab_token = std::env::var("GITLAB_TOKEN").unwrap_or_default();

    Ok(MrHookPayload {
        action,
        mr_url,
        mr_iid,
        sha,
        gitlab_token,
    })
}

/// Spawn a background task that runs the full review for an MR.
fn spawn_mr_review_task(dispatcher: &MrDispatcher, mr_url: String, sha: String, gitlab_token: String, mr_iid: u64) {
    let d = dispatcher.clone();
    tokio::spawn(async move {
        if let Err(e) = run_review_for_mr(&mr_url, &gitlab_token, Some(&d), Some(&mr_url), Some(&sha)).await {
            tracing::error!("Review failed for MR !{}: {:?}", mr_iid, e);
            d.reset(&mr_url).await;
        }
    });
}

/// Handle the `InProgress` dispatcher state: wait and then retry.
async fn handle_mr_in_progress(dispatcher: &MrDispatcher, mr_url: &str, sha: &str, gitlab_token: &str, mr_iid: u64) {
    tracing::info!("MR !{} review in progress, waiting...", mr_iid);
    dispatcher.wait(mr_url).await;
    // After wait, re-check if current SHA needs a new review
    match dispatcher.try_start(mr_url, sha).await {
        super::dispatcher::ShouldStart::Go => {
            spawn_mr_review_task(
                dispatcher,
                mr_url.to_string(),
                sha.to_string(),
                gitlab_token.to_string(),
                mr_iid,
            );
        }
        _ => {
            tracing::info!("No new review needed for MR !{} after wait", mr_iid);
        }
    }
}

/// Dispatch an MR webhook event to start or defer a review based on the
/// dispatcher state.
async fn dispatch_mr_event(dispatcher: &MrDispatcher, mr_url: &str, sha: &str, gitlab_token: &str, mr_iid: u64) {
    match dispatcher.try_start(mr_url, sha).await {
        super::dispatcher::ShouldStart::Go => {
            spawn_mr_review_task(
                dispatcher,
                mr_url.to_string(),
                sha.to_string(),
                gitlab_token.to_string(),
                mr_iid,
            );
        }
        super::dispatcher::ShouldStart::AlreadyReviewed => {
            tracing::info!("Skipping MR !{}: already reviewed at SHA {}", mr_iid, sha);
        }
        super::dispatcher::ShouldStart::InProgress => {
            handle_mr_in_progress(dispatcher, mr_url, sha, gitlab_token, mr_iid).await;
        }
    }
}

async fn handle_mr_hook(body: &str, dispatcher: &MrDispatcher) -> Result<Json<Value>, StatusCode> {
    let payload = parse_mr_hook_payload(body)?;

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

        dispatch_mr_event(
            dispatcher,
            &payload.mr_url,
            &payload.sha,
            &payload.gitlab_token,
            payload.mr_iid,
        )
        .await;
    }

    Ok(Json(serde_json::json!({
        "status": "received",
        "action": payload.action,
    })))
}

/// Run a full review for an MR and publish results.
async fn run_review_for_mr(
    mr_url: &str,
    gitlab_token: &str,
    dispatcher: Option<&MrDispatcher>,
    dispatch_key: Option<&str>,
    sha: Option<&str>,
) -> anyhow::Result<()> {
    super::run_review_common(mr_url, gitlab_token, dispatcher, dispatch_key, sha).await
}

async fn handle_note_hook(body: &str, dispatcher: &MrDispatcher) -> Result<Json<Value>, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse Note hook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let note = parsed["object_attributes"]["note"].as_str().unwrap_or("");
    let note_lower = note.to_lowercase();

    // Check for commands like /review, /describe
    if note_lower.starts_with("/review") || note_lower.starts_with("/describe") {
        let project_url = parsed["project"]["web_url"].as_str().unwrap_or("").to_string();
        let mr_iid = parsed["merge_request"]["iid"]
            .as_u64()
            .or_else(|| parsed["object_attributes"]["noteable_iid"].as_u64())
            .unwrap_or(0);
        let mr_url = if !project_url.is_empty() && mr_iid > 0 {
            format!("{}/-/merge_requests/{}", project_url, mr_iid)
        } else {
            String::new()
        };
        let gitlab_token = std::env::var("GITLAB_TOKEN").unwrap_or_default();

        if !mr_url.is_empty() && !gitlab_token.is_empty() {
            let url = mr_url;
            let token = gitlab_token;
            let sha = format!("note_{}", uuid::Uuid::new_v4());

            match dispatcher.try_start(&url, &sha).await {
                super::dispatcher::ShouldStart::Go => {
                    let d = dispatcher.clone();
                    let u = url;
                    let s = sha;
                    tokio::spawn(async move {
                        if let Err(e) = run_review_for_mr(&u, &token, Some(&d), Some(&u), Some(&s)).await {
                            tracing::error!("Review from note failed: {:?}", e);
                            d.reset(&u).await;
                        }
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

async fn handle_push_hook(body: &str) -> Result<Json<Value>, StatusCode> {
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

    #[test]
    fn test_webhook_state_creation() {
        let state = GitLabWebhookState {
            webhook_secret: "test-secret".to_string(),
            dispatcher: MrDispatcher::new(),
        };
        assert_eq!(state.webhook_secret, "test-secret");
    }

    #[test]
    fn test_webhook_state_empty_secret() {
        let state = GitLabWebhookState {
            webhook_secret: String::new(),
            dispatcher: MrDispatcher::new(),
        };
        assert!(state.webhook_secret.is_empty());
    }
}
