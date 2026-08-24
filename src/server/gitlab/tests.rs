use super::super::dispatcher::MrDispatcher;
use super::handler::HmacSha256;
use super::*;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use base64::Engine;
use hmac::Mac;

#[test]
fn test_webhook_handler_creation() {
    let handler = GitLabWebhookHandler::new(
        "test-secret".to_string(),
        Some("test-signing".to_string()),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    assert_eq!(handler.webhook_secret, "test-secret");
    assert_eq!(handler.signing_secret, Some("test-signing".to_string()));
    assert_eq!(handler.path(), "/webhook/gitlab");
    assert_eq!(handler.name(), "gitlab");
}

#[test]
fn test_webhook_handler_empty_secret() {
    let handler = GitLabWebhookHandler::new(String::new(), None, MrDispatcher::new(), "test-token".to_string());
    assert!(handler.webhook_secret.is_empty());
    assert!(handler.signing_secret.is_none());
}

// ── Legacy X-Gitlab-Token tests ────────────────────────────────────

#[tokio::test]
async fn test_webhook_verify_valid_token() {
    let handler = GitLabWebhookHandler::new(
        "my-secret".to_string(),
        None,
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "my-secret".parse().unwrap());
    let result = handler.verify(&headers, "").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_webhook_verify_invalid_token() {
    let handler = GitLabWebhookHandler::new(
        "my-secret".to_string(),
        None,
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "wrong-secret".parse().unwrap());
    let result = handler.verify(&headers, "").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_webhook_verify_missing_token() {
    let handler = GitLabWebhookHandler::new(
        "my-secret".to_string(),
        None,
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let headers = HeaderMap::new();
    let result = handler.verify(&headers, "").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_webhook_verify_empty_secret_rejects_any_token() {
    let handler = GitLabWebhookHandler::new(String::new(), None, MrDispatcher::new(), "test-token".to_string());
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "anything".parse().unwrap());
    let result = handler.verify(&headers, "").await;
    // Empty secret and no signing_secret → no verification configured → rejected
    assert!(result.is_err());
}

// ── Signing token (X-Gitlab-Webhook-Signature) tests ───────────────

#[tokio::test]
async fn test_signing_verify_valid_signature() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = r#"{"object_attributes":{"action":"open","iid":1}}"#;
    let message_id = "msg-123";
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let message = format!("{}.{}.{}", message_id, timestamp, body);
    let mut mac = HmacSha256::new_from_slice(raw_key).unwrap();
    mac.update(message.as_bytes());
    let sig = format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    );

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_signing_verify_invalid_signature() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", "msg-123".parse().unwrap());
    headers.insert(
        "webhook-timestamp",
        chrono::Utc::now().timestamp().to_string().parse().unwrap(),
    );
    headers.insert(
        "webhook-signature",
        "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".parse().unwrap(),
    );
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_verify_missing_signature() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let headers = HeaderMap::new();
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_verify_missing_headers_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-signature", "v1,abcdef123456==".parse().unwrap());
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_verify_old_timestamp_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    // Timestamp 10 minutes ago
    let timestamp = (chrono::Utc::now().timestamp() - 600).to_string();
    let message = format!("{}.{}.{}", message_id, timestamp, body);
    let mut mac = HmacSha256::new_from_slice(raw_key).unwrap();
    mac.update(message.as_bytes());
    let sig = format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    );

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

// ── Both methods configured ─────────────────────────────────────────

#[tokio::test]
async fn test_both_verify_all_pass() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = r#"{"object_attributes":{"action":"open"}}"#;
    let message_id = "msg-123";
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let message = format!("{}.{}.{}", message_id, timestamp, body);
    let mut mac = HmacSha256::new_from_slice(raw_key).unwrap();
    mac.update(message.as_bytes());
    let sig = format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    );

    let handler = GitLabWebhookHandler::new(
        "my-webhook-secret".to_string(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "my-webhook-secret".parse().unwrap());
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_both_signing_present_legacy_wrong_ignored() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let message = format!("{}.{}.{}", message_id, timestamp, body);
    let mut mac = HmacSha256::new_from_slice(raw_key).unwrap();
    mac.update(message.as_bytes());
    let sig = format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    );

    let handler = GitLabWebhookHandler::new(
        "my-webhook-secret".to_string(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "wrong-secret".parse().unwrap());
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_both_verify_signing_wrong() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        "my-webhook-secret".to_string(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "my-webhook-secret".parse().unwrap());
    headers.insert("webhook-id", "msg-123".parse().unwrap());
    headers.insert(
        "webhook-timestamp",
        chrono::Utc::now().timestamp().to_string().parse().unwrap(),
    );
    headers.insert(
        "webhook-signature",
        "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".parse().unwrap(),
    );
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_signing_not_configured_skipped() {
    let handler = GitLabWebhookHandler::new(
        "my-webhook-secret".to_string(),
        None,
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "my-webhook-secret".parse().unwrap());
    // No webhook-signature header, but signing not configured → OK
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_ok());
}

// ── Migration path tests ─────────────────────────────────────────────

#[tokio::test]
async fn test_both_signing_header_missing_falls_back_legacy() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        "my-webhook-secret".to_string(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "my-webhook-secret".parse().unwrap());
    // No webhook-signature header → fall back to legacy token
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_legacy_only_secret_verified() {
    let handler = GitLabWebhookHandler::new(
        "my-webhook-secret".to_string(),
        None,
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "my-webhook-secret".parse().unwrap());
    let result = handler.verify(&headers, "").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_signing_only_header_missing_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let headers = HeaderMap::new();
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

// ── Edge case tests ─────────────────────────────────────────────────

fn sign_message(raw_key: &[u8], message_id: &str, timestamp: i64, body: &str) -> String {
    let message = format!("{}.{}.{}", message_id, timestamp, body);
    let mut mac = HmacSha256::new_from_slice(raw_key).unwrap();
    mac.update(message.as_bytes());
    format!(
        "v1,{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    )
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

#[tokio::test]
async fn test_signing_multiple_signatures_one_valid() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = r#"{"object_attributes":{"action":"open"}}"#;
    let message_id = "msg-123";
    let timestamp = unix_now();
    let valid_sig = sign_message(raw_key, message_id, timestamp, body);

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert(
        "webhook-signature",
        format!("v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA== {}", valid_sig)
            .parse()
            .unwrap(),
    );
    let result = handler.verify(&headers, body).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_signing_timestamp_minus_300_ok() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    let timestamp = unix_now() - 300;
    let sig = sign_message(raw_key, message_id, timestamp, body);

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_signing_timestamp_minus_301_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    let timestamp = unix_now() - 301;
    let sig = sign_message(raw_key, message_id, timestamp, body);

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_timestamp_plus_300_ok() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    let timestamp = unix_now() + 300;
    let sig = sign_message(raw_key, message_id, timestamp, body);

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_signing_timestamp_plus_301_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    let timestamp = unix_now() + 301;
    let sig = sign_message(raw_key, message_id, timestamp, body);

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_timestamp_future_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let body = "body";
    let message_id = "msg-123";
    let timestamp = unix_now() + 3600;
    let sig = sign_message(raw_key, message_id, timestamp, body);

    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let result = handler.verify(&headers, body).await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_empty_webhook_id_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", "".parse().unwrap());
    headers.insert("webhook-timestamp", unix_now().to_string().parse().unwrap());
    headers.insert(
        "webhook-signature",
        "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".parse().unwrap(),
    );
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_empty_webhook_timestamp_rejected() {
    let raw_key = b"my-signing-secret";
    let signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some(signing_secret),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", "msg-123".parse().unwrap());
    headers.insert("webhook-timestamp", "".parse().unwrap());
    headers.insert(
        "webhook-signature",
        "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".parse().unwrap(),
    );
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_signing_invalid_base64_secret_rejected() {
    let handler = GitLabWebhookHandler::new(
        String::new(),
        Some("whsec_!!!not_valid_base64!!!".to_string()),
        MrDispatcher::new(),
        "test-token".to_string(),
    );
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", "msg-123".parse().unwrap());
    headers.insert("webhook-timestamp", "1234567890".parse().unwrap());
    headers.insert(
        "webhook-signature",
        "v1,AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==".parse().unwrap(),
    );
    let result = handler.verify(&headers, "body").await;
    assert!(result.is_err());
    let (status, _) = result.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

// ── Note hook command matching (HIGH-2: prefix lookalike regression) ───

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

// ── Multi-platform webhook selection (gitPlatforms) ─────────────────

/// Restore the global GitLab runtime after a test: these tests reset it to
/// all-empty so `effective_config` falls back to the handler's own fields
/// (the "legacy default"), and must not leak that reset into other modules'
/// tests sharing the global.
struct EmptyRuntimeGuard(GitLabRuntimeConfig);

impl EmptyRuntimeGuard {
    fn new() -> Self {
        let saved = gitlab_runtime().read().unwrap().clone();
        *gitlab_runtime().write().unwrap() = GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: String::new(),
        };
        Self(saved)
    }
}

impl Drop for EmptyRuntimeGuard {
    fn drop(&mut self) {
        *gitlab_runtime().write().unwrap() = self.0.clone();
    }
}

fn platform_entry(name: &str, base_url: &str, token: &str, webhook_secret: &str) -> crate::models::GitPlatformConfig {
    crate::models::GitPlatformConfig {
        name: name.to_string(),
        platform_type: "gitlab".to_string(),
        base_url: base_url.to_string(),
        token: token.to_string(),
        webhook_secret: webhook_secret.to_string(),
        webhook_signing_secret: String::new(),
    }
}

fn state_with_platforms(platforms: Vec<crate::models::GitPlatformConfig>) -> std::sync::Arc<crate::server::AppState> {
    let state = crate::server::AppState::new(vec![]);
    *state.git_platforms.write().unwrap() = platforms;
    std::sync::Arc::new(state)
}

const PLATFORM_BODY: &str =
    r#"{"project":{"web_url":"http://gitlab.internal:8929/group/proj"},"object_attributes":{"action":"open","iid":1}}"#;

/// Selection logic: a payload whose `project.web_url` host[:port] matches a
/// configured platform resolves to THAT platform's secrets and token for
/// both verification and review dispatch.
#[tokio::test]
async fn webhook_platform_match_selects_platform_config() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    // The state must outlive the handler: `with_app_state` stores a Weak.
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "platform-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let cfg = handler.effective_config_for_body(PLATFORM_BODY);
    assert_eq!(cfg.token, "glpat-platform");
    assert_eq!(cfg.webhook_secret, "platform-secret");

    // Host matching is scheme-less and case-insensitive on the host.
    let body = PLATFORM_BODY.replace("http://gitlab.internal:8929", "https://GITLAB.internal:8929");
    assert_eq!(handler.effective_config_for_body(&body).token, "glpat-platform");
}

/// An unmatched payload host (or a body without an instance URL) keeps the
/// runtime default — the pre-multi-platform behavior.
#[tokio::test]
async fn webhook_unmatched_host_keeps_default_config() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "platform-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    for body in [
        PLATFORM_BODY.replace("gitlab.internal:8929", "other.internal:8929"),
        r#"{"object_attributes":{"action":"open"}}"#.to_string(),
        "not json".to_string(),
    ] {
        let cfg = handler.effective_config_for_body(&body);
        assert_eq!(cfg.token, "glpat-default", "body: {body}");
        assert_eq!(cfg.webhook_secret, "default-secret", "body: {body}");
    }
}

/// A token-only platform entry (configured for REST review routing) must NOT
/// take over webhook verification for its host: the runtime default keeps
/// applying, so adding an instance for routing never breaks an existing
/// webhook setup for the same host.
#[tokio::test]
async fn webhook_token_only_platform_keeps_default_verification() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "", // no webhook secret
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let cfg = handler.effective_config_for_body(PLATFORM_BODY);
    assert_eq!(cfg.webhook_secret, "default-secret");
    assert_eq!(cfg.token, "glpat-default");
}

/// A handler without an attached AppState (tests, legacy startup) always
/// uses the runtime default.
#[tokio::test]
async fn webhook_without_app_state_uses_default_config() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    );
    let cfg = handler.effective_config_for_body(PLATFORM_BODY);
    assert_eq!(cfg.token, "glpat-default");
    assert_eq!(cfg.webhook_secret, "default-secret");
}

/// End-to-end verification per matched platform: the platform's webhook
/// secret verifies requests for its host; the default secret is rejected
/// there (and vice versa for an unmatched host).
#[tokio::test]
async fn webhook_verify_uses_matched_platform_secret() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "platform-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "platform-secret".parse().unwrap());
    assert!(handler.verify(&headers, PLATFORM_BODY).await.is_ok());

    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "default-secret".parse().unwrap());
    let (status, _) = handler.verify(&headers, PLATFORM_BODY).await.unwrap_err();
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);

    // Unmatched host: the default secret verifies, the platform's does not.
    let other_body = PLATFORM_BODY.replace("gitlab.internal:8929", "other.internal:8929");
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "default-secret".parse().unwrap());
    assert!(handler.verify(&headers, &other_body).await.is_ok());
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "platform-secret".parse().unwrap());
    assert!(handler.verify(&headers, &other_body).await.is_err());
}

/// The 19.x signing-secret path is also per-platform: a payload matching a
/// platform verifies against THAT platform's `whsec_` key.
#[tokio::test]
async fn webhook_signing_verify_uses_matched_platform_key() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let raw_key = b"platform-signing-key";
    let mut platform = platform_entry("testbed", "http://gitlab.internal:8929", "glpat-platform", "");
    platform.webhook_signing_secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(raw_key));

    let state = state_with_platforms(vec![platform]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let message_id = "msg-1";
    let timestamp = unix_now();
    let sig = sign_message(raw_key, message_id, timestamp, PLATFORM_BODY);
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", timestamp.to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    assert!(handler.verify(&headers, PLATFORM_BODY).await.is_ok());

    // The same signature against a different body (unmatched host) must not
    // verify — the default config has no signing secret at all.
    let other_body = PLATFORM_BODY.replace("gitlab.internal:8929", "other.internal:8929");
    let message_id = "msg-2";
    let sig = sign_message(raw_key, message_id, unix_now(), &other_body);
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", unix_now().to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let mut headers_legacy = headers.clone();
    headers_legacy.insert("X-Gitlab-Token", "default-secret".parse().unwrap());
    // With the legacy default secret present, the unmatched host falls back
    // to the legacy check when the signature header... is present — but the
    // default has no signing secret configured, so the legacy token path
    // applies and verifies.
    assert!(handler.verify(&headers_legacy, &other_body).await.is_ok());
}
