use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::post, Json};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use super::dispatcher::MrDispatcher;

type HmacSha256 = Hmac<Sha256>;

/// Shared state for GitHub webhook handling.
#[derive(Clone)]
pub struct GitHubWebhookState {
    pub webhook_secret: String,
    pub dispatcher: MrDispatcher,
    pub token: String,
}

/// Register GitHub webhook routes on the given router.
pub fn routes() -> axum::Router<GitHubWebhookState> {
    axum::Router::new().route("/webhook/github", post(handle_webhook))
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

/// Handle incoming GitHub webhook events.
async fn handle_webhook(
    State(state): State<GitHubWebhookState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl axum::response::IntoResponse {
    // Verify X-Hub-Signature-256
    let signature_raw = headers.get("X-Hub-Signature-256");

    if signature_raw.is_none() {
        tracing::warn!("GitHub webhook missing X-Hub-Signature-256 header");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "missing signature header"})),
        )
            .into_response();
    }

    let signature_str = signature_raw.and_then(|v| v.to_str().ok()).unwrap_or("");

    let signature = if let Some(s) = signature_str.strip_prefix("sha256=") {
        s
    } else {
        tracing::warn!("GitHub webhook signature does not start with sha256=: {signature_str}");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "invalid signature format"})),
        )
            .into_response();
    };

    if hex::decode(signature).is_err() {
        tracing::warn!("GitHub webhook signature is not valid hex");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "invalid signature encoding"})),
        )
            .into_response();
    }

    if !verify_signature(&state.webhook_secret, &body, signature) {
        tracing::warn!("GitHub webhook HMAC signature mismatch — check GITHUB_WEBHOOK_SECRET");
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "invalid signature"})),
        )
            .into_response();
    }

    // Parse event type
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let result = match event {
        "ping" => {
            tracing::info!("GitHub ping event received");
            Ok(Json(serde_json::json!({ "status": "ok" })))
        }
        "pull_request" => handle_pull_request(&body, &state.dispatcher, &state.token).await,
        "issue_comment" => handle_issue_comment(&body, &state.dispatcher, &state.token).await,
        "push" => {
            tracing::info!("GitHub push event received");
            Ok(Json(serde_json::json!({ "status": "received" })))
        }
        _ => {
            tracing::debug!("Ignoring unsupported GitHub event: {}", event);
            Ok(Json(serde_json::json!({ "status": "ignored" })))
        }
    };

    match result {
        Ok(json) => (StatusCode::OK, json).into_response(),
        Err(status) => (status, Json(serde_json::json!({"error": "request failed"}))).into_response(),
    }
}

async fn handle_pull_request(
    body: &str,
    dispatcher: &MrDispatcher,
    github_token: &str,
) -> Result<Json<Value>, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse PR webhook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let action = parsed["action"].as_str().unwrap_or("");
    let pr_number = parsed["pull_request"]["number"].as_u64().unwrap_or(0);
    let repo_full = parsed["repository"]["full_name"].as_str().unwrap_or("");
    let pr_url = if !repo_full.is_empty() && pr_number > 0 {
        format!("https://github.com/{}/pull/{}", repo_full, pr_number)
    } else {
        String::new()
    };
    let sha = parsed["pull_request"]["head"]["sha"].as_str().unwrap_or("");

    tracing::info!("GitHub PR #{} webhook: action={}", pr_number, action);

    let github_token = github_token.to_string();

    if action == "opened" || action == "reopened" || action == "synchronize" {
        if pr_url.is_empty() || github_token.is_empty() || sha.is_empty() {
            tracing::warn!("Skipping: missing PR URL, GITHUB_TOKEN, or SHA");
            return Ok(Json(serde_json::json!({"status": "skipped"})));
        }

        match dispatcher.try_start(&pr_url, sha).await {
            super::dispatcher::ShouldStart::Go => {
                let d = dispatcher.clone();
                let u = pr_url.clone();
                let s = sha.to_string();
                let token = github_token.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_review_for_pr(&u, &token, Some(&d), &u, &s).await {
                        tracing::error!("Review failed for PR #{}: {:?}", pr_number, e);
                        d.reset(&u).await;
                    }
                });
            }
            super::dispatcher::ShouldStart::AlreadyReviewed => {
                tracing::info!("Skipping PR #{}: already reviewed at SHA {}", pr_number, sha);
            }
            super::dispatcher::ShouldStart::InProgress => {
                tracing::info!("PR #{} review in progress, waiting...", pr_number);
                dispatcher.wait(&pr_url).await;
                match dispatcher.try_start(&pr_url, sha).await {
                    super::dispatcher::ShouldStart::Go => {
                        let d = dispatcher.clone();
                        let u = pr_url.clone();
                        let s = sha.to_string();
                        let token = github_token.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_review_for_pr(&u, &token, Some(&d), &u, &s).await {
                                tracing::error!("Review failed for PR #{}: {:?}", pr_number, e);
                                d.reset(&u).await;
                            }
                        });
                    }
                    _ => {
                        tracing::info!("No new review needed for PR #{} after wait", pr_number);
                    }
                }
            }
        }
    }

    Ok(Json(serde_json::json!({ "status": "received", "action": action })))
}

async fn handle_issue_comment(
    body: &str,
    dispatcher: &MrDispatcher,
    github_token: &str,
) -> Result<Json<Value>, StatusCode> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| {
        tracing::error!("Failed to parse issue_comment webhook: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let note = parsed["comment"]["body"].as_str().unwrap_or("");
    let note_lower = note.to_lowercase();

    if note_lower.starts_with("/review") || note_lower.starts_with("/describe") {
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
            match dispatcher.try_start(&pr_url, &sha).await {
                super::dispatcher::ShouldStart::Go => {
                    let d = dispatcher.clone();
                    let u = pr_url;
                    let s = sha;
                    let token = github_token;
                    tokio::spawn(async move {
                        if let Err(e) = run_review_for_pr(&u, &token, Some(&d), &u, &s).await {
                            tracing::error!("Review from comment failed: {:?}", e);
                            d.reset(&u).await;
                        }
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

/// Run a full review for a GitHub PR and publish results.
async fn run_review_for_pr(
    pr_url: &str,
    github_token: &str,
    dispatcher: Option<&MrDispatcher>,
    dispatch_key: &str,
    sha: &str,
) -> anyhow::Result<()> {
    super::run_review_common(pr_url, github_token, dispatcher, Some(dispatch_key), Some(sha)).await
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
}
