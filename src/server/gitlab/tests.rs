use super::super::dispatcher::{MrDispatcher, ShouldStart};
use super::handler::payload_instance_url;
use super::handler::system_hook_event_name;
use super::handler::unique_verification_platform;
use super::handler::HmacSha256;
use super::hooks::{mr_iid_from_url, rewrite_url_to_platform};
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
        internal_base_url: String::new(),
        token: token.to_string(),
        webhook_secret: webhook_secret.to_string(),
        webhook_signing_secret: String::new(),
        allowed_projects: Vec::new(),
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
/// runtime default whenever the unique-verification fallback cannot decide:
/// with MULTIPLE verification-capable platforms there is no single entry to
/// fall back to, so the pre-multi-platform default applies (never guess).
#[tokio::test]
async fn webhook_unmatched_host_keeps_default_config() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    // Two verification-capable platforms on unrelated hosts: an unmatched URL
    // (or no URL at all) is ambiguous → runtime default, not either platform.
    let state = state_with_platforms(vec![
        platform_entry(
            "testbed",
            "http://gitlab.internal:8929",
            "glpat-platform",
            "platform-secret",
        ),
        platform_entry(
            "other",
            "http://gitlab-other.internal:8929",
            "glpat-other",
            "other-secret",
        ),
    ]);
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
/// there (and vice versa for an unmatched host — with TWO verification
/// platforms the unique-verification fallback cannot guess, so the default
/// applies there).
#[tokio::test]
async fn webhook_verify_uses_matched_platform_secret() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    // Two verification-capable platforms on unrelated hosts: PLATFORM_BODY
    // strictly matches "testbed"; the unmatched host is ambiguous → default.
    let state = state_with_platforms(vec![
        platform_entry(
            "testbed",
            "http://gitlab.internal:8929",
            "glpat-platform",
            "platform-secret",
        ),
        platform_entry(
            "other",
            "http://gitlab-other.internal:8929",
            "glpat-other",
            "other-secret",
        ),
    ]);
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

    // Unmatched host with a SINGLE verification-capable platform: the unique
    // verification platform fallback takes over, so the same platform signing
    // key verifies the re-hosted body — the host-mismatch path (base_url vs
    // payload localhost) that the fallback exists to fix. The legacy default
    // token in the header is irrelevant: the platform config has signing
    // configured (and no legacy secret), so the signature path verifies.
    let other_body = PLATFORM_BODY.replace("gitlab.internal:8929", "other.internal:8929");
    let message_id = "msg-2";
    let sig = sign_message(raw_key, message_id, unix_now(), &other_body);
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", message_id.parse().unwrap());
    headers.insert("webhook-timestamp", unix_now().to_string().parse().unwrap());
    headers.insert("webhook-signature", sig.parse().unwrap());
    let mut headers_legacy = headers.clone();
    headers_legacy.insert("X-Gitlab-Token", "default-secret".parse().unwrap());
    assert!(handler.verify(&headers_legacy, &other_body).await.is_ok());
}

// ── Unique verification platform fallback (System Hooks Test button) ──

/// GitLab System Hooks' **Test** button sends a SAMPLE payload with no URL at
/// all (`event_name`/`project_id`/`changes`/`refs` only — no `project`
/// object, no `repository`, nothing to match). With exactly ONE
/// verification-capable platform configured, that platform unambiguously
/// takes over both verification config and `matched_platform` — the fix for
/// the Test button 403.
#[tokio::test]
async fn no_url_payload_falls_back_to_unique_verification_platform() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://host.docker.internal:8929",
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

    // Real captured shape of the System Hooks Test button payload.
    let body = r#"{"event_name":"merge_request","project_id":123,"changes":[],"refs":[]}"#;
    assert_eq!(payload_instance_url(body), None);

    let cfg = handler.effective_config_for_body(body);
    assert_eq!(cfg.webhook_secret, "platform-secret");
    assert_eq!(cfg.token, "glpat-platform");

    let platform = handler.matched_platform(body).unwrap();
    assert_eq!(platform.name, "testbed");
    assert_eq!(platform.base_url, "http://host.docker.internal:8929");

    // End-to-end: the platform's secret verifies the Test payload.
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Token", "platform-secret".parse().unwrap());
    assert!(handler.verify(&headers, body).await.is_ok());
}

/// No-URL payload + TWO verification-capable platforms → the fallback cannot
/// guess: the runtime default applies and `matched_platform` is `None`.
#[tokio::test]
async fn no_url_payload_two_verification_platforms_keeps_default() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![
        platform_entry("a", "http://gitlab-a.internal:8929", "glpat-a", "secret-a"),
        platform_entry("b", "http://gitlab-b.internal:8929", "glpat-b", "secret-b"),
    ]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let body = r#"{"event_name":"merge_request","project_id":123}"#;
    let cfg = handler.effective_config_for_body(body);
    assert_eq!(cfg.webhook_secret, "default-secret");
    assert_eq!(cfg.token, "glpat-default");
    assert!(handler.matched_platform(body).is_none());
}

/// No-URL payload + a single token-only platform (no webhook verification
/// credentials) → the fallback ignores it (it cannot verify): runtime default.
#[tokio::test]
async fn no_url_payload_token_only_platform_keeps_default() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![platform_entry(
        "routing-only",
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

    let body = r#"{"event_name":"merge_request","project_id":123}"#;
    let cfg = handler.effective_config_for_body(body);
    assert_eq!(cfg.webhook_secret, "default-secret");
    assert_eq!(cfg.token, "glpat-default");
    assert!(handler.matched_platform(body).is_none());
}

/// A payload WITH an instance URL still resolves strictly by URL first (the
/// fallback must never override a URL match — multi-platform URL semantics
/// unchanged), and the single-platform host mismatch — payload carries
/// `localhost`, `base_url` is `host.docker.internal` — is exactly the case the
/// unique fallback fixes.
#[tokio::test]
async fn url_payload_prefers_strict_match_and_host_mismatch_falls_back() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://host.docker.internal:8929",
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

    // URL matches the platform's base_url → strict match wins over the
    // fallback (regression: multi-platform URL semantics unchanged).
    let matching_body = PLATFORM_BODY.replace("gitlab.internal:8929", "host.docker.internal:8929");
    let cfg = handler.effective_config_for_body(&matching_body);
    assert_eq!(cfg.webhook_secret, "platform-secret");
    assert_eq!(handler.matched_platform(&matching_body).unwrap().name, "testbed");

    // Host mismatch: the payload's external_url is `localhost`, the platform's
    // base_url is `host.docker.internal` → URL matches no platform → the
    // unique verification platform still takes over (verification AND
    // allowlist/rewrite resolution).
    let localhost_body = PLATFORM_BODY.replace("gitlab.internal:8929", "localhost:8929");
    let cfg = handler.effective_config_for_body(&localhost_body);
    assert_eq!(cfg.webhook_secret, "platform-secret");
    assert_eq!(handler.matched_platform(&localhost_body).unwrap().name, "testbed");
}

/// `unique_verification_platform` boundary: exactly one verification-capable
/// entry wins regardless of surrounding token-only entries; zero or multiple
/// verification-capable entries (or an empty list) yield `None`.
#[test]
fn unique_verification_platform_requires_exactly_one() {
    let verifying = |name: &str| platform_entry(name, "http://gitlab.internal:8929", "tok", "wh-secret");
    let token_only = |name: &str| platform_entry(name, "http://gitlab.internal:8929", "tok", "");
    // Exactly one verification-capable entry → it (token-only entries ignored).
    let hit = unique_verification_platform(&[token_only("routing"), verifying("verifying"), token_only("routing-2")]);
    assert_eq!(hit.map(|p| p.name), Some("verifying".to_string()));
    // Two verification-capable entries → ambiguous → None.
    assert!(unique_verification_platform(&[verifying("a"), verifying("b")]).is_none());
    // Zero verification-capable entries (all token-only) → None.
    assert!(unique_verification_platform(&[token_only("a"), token_only("b")]).is_none());
    // Empty list → None.
    assert!(unique_verification_platform(&[]).is_none());
}

// ── System Hooks (admin-level) ─────────────────────────────────────

/// System-hook MR payload shape: `event_name` lives in the body, there is NO
/// `project.web_url`, the instance URL is in `project.homepage`, and the full
/// MR URL in `object_attributes.url`. Action `close` so a routing test never
/// spawns a network review.
const SYSTEM_MR_HOOK_BODY: &str = r#"{
  "event_name": "merge_request",
  "project": {
    "homepage": "http://gitlab.internal:8929/group/proj",
    "path_with_namespace": "group/proj"
  },
  "object_attributes": {
    "action": "close",
    "iid": 42,
    "url": "http://gitlab.internal:8929/group/proj/-/merge_requests/42"
  }
}"#;

/// Real captured GitLab 19.2.4 system-hook MR payload shape: **`event_type`
/// replaces `event_name`** (no `event_name` field at all). Same rest of the
/// shape as `SYSTEM_MR_HOOK_BODY` — no `project.web_url`, instance URL in
/// `project.homepage`, full MR URL in `object_attributes.url`. Action `close`
/// so a routing test never spawns a network review.
const SYSTEM_MR_HOOK_BODY_EVENT_TYPE: &str = r#"{
  "event_type": "merge_request",
  "project": {
    "homepage": "http://gitlab.internal:8929/group/proj",
    "path_with_namespace": "group/proj"
  },
  "object_attributes": {
    "action": "close",
    "iid": 42,
    "url": "http://gitlab.internal:8929/group/proj/-/merge_requests/42"
  }
}"#;

/// `X-Gitlab-Event: System Hook` headers for routing tests.
fn system_hook_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Event", "System Hook".parse().unwrap());
    headers
}

/// System-hook MR payloads (no `project.web_url`) resolve the MR URL straight
/// from `object_attributes.url`; the legacy `project.web_url + iid`
/// construction still applies when it is absent (project-level regression).
#[test]
fn system_hook_mr_payload_uses_object_attributes_url() {
    let payload = parse_mr_hook_payload(SYSTEM_MR_HOOK_BODY, "tok").unwrap();
    assert_eq!(
        payload.mr_url,
        "http://gitlab.internal:8929/group/proj/-/merge_requests/42"
    );
    assert_eq!(payload.mr_iid, 42);
    assert_eq!(payload.action, "close");
    assert_eq!(payload.path_with_namespace, "group/proj");

    let legacy = r#"{"project":{"web_url":"http://gitlab.internal:8929/group/proj"},"object_attributes":{"action":"open","iid":7}}"#;
    let payload = parse_mr_hook_payload(legacy, "tok").unwrap();
    assert_eq!(
        payload.mr_url,
        "http://gitlab.internal:8929/group/proj/-/merge_requests/7"
    );
    assert_eq!(payload.mr_iid, 7);
}

/// Instance URL fallback chain: `project.web_url` → `project.homepage` →
/// `repository.homepage` → `object_attributes.url`; `None` when nothing is
/// carried (or the body is not JSON).
#[test]
fn payload_instance_url_fallback_chain() {
    // web_url wins when present.
    let body = r#"{"project":{"web_url":"https://a.example/g/p","homepage":"https://b.example/g/p"},"object_attributes":{"url":"https://c.example/x"}}"#;
    assert_eq!(payload_instance_url(body).as_deref(), Some("https://a.example/g/p"));
    // No web_url → project.homepage (system-hook MR shape).
    let body = r#"{"project":{"homepage":"https://b.example/g/p"},"object_attributes":{"url":"https://c.example/x"}}"#;
    assert_eq!(payload_instance_url(body).as_deref(), Some("https://b.example/g/p"));
    // No project section at all → repository.homepage (push shape).
    let body = r#"{"repository":{"homepage":"https://d.example/g/p"}}"#;
    assert_eq!(payload_instance_url(body).as_deref(), Some("https://d.example/g/p"));
    // Last resort: object_attributes.url.
    let body = r#"{"object_attributes":{"url":"https://c.example/g/p/-/merge_requests/1"}}"#;
    assert_eq!(
        payload_instance_url(body).as_deref(),
        Some("https://c.example/g/p/-/merge_requests/1")
    );
    // Empty / missing everywhere → None.
    assert_eq!(
        payload_instance_url(r#"{"project":{"path_with_namespace":"g/p"}}"#),
        None
    );
    assert_eq!(payload_instance_url(""), None);
    assert_eq!(payload_instance_url("not json"), None);
}

/// `system_hook_event_name` precedence: `event_name` wins (legacy/docs
/// shape), `event_type` is the GitLab 19.2+ fallback; empty string when
/// neither is present, the value is not a string, or the body is not JSON.
#[test]
fn system_hook_event_name_prefers_event_name_falls_back_to_event_type() {
    // event_name wins when both are present.
    let both = r#"{"event_name":"merge_request","event_type":"merge_request"}"#;
    assert_eq!(system_hook_event_name(both), "merge_request");

    // Old-version / docs shape: only event_name.
    let event_name_only = r#"{"event_name":"push","repository":{"homepage":"http://gitlab.internal:8929/g/p"}}"#;
    assert_eq!(system_hook_event_name(event_name_only), "push");

    // GitLab 19.2+ real shape: only event_type.
    let event_type_only = r#"{"event_type":"note","project":{"path_with_namespace":"g/p"}}"#;
    assert_eq!(system_hook_event_name(event_type_only), "note");

    // Neither event_name nor event_type → empty (unknown → ignored).
    assert_eq!(
        system_hook_event_name(r#"{"project":{"path_with_namespace":"g/p"}}"#),
        ""
    );
    // event_type present but not a string → empty.
    assert_eq!(system_hook_event_name(r#"{"event_type":123}"#), "");
    // Non-JSON body → empty.
    assert_eq!(system_hook_event_name("not json"), "");
}

/// The iid fallback for system-hook notes: extracted from the
/// `object_attributes.url` tail `/-/merge_requests/{iid}`.
#[test]
fn mr_iid_extracted_from_system_hook_url_tail() {
    assert_eq!(
        mr_iid_from_url("http://gitlab.internal:8929/group/proj/-/merge_requests/42"),
        Some(42)
    );
    // Query strings and trailing segments are tolerated.
    assert_eq!(
        mr_iid_from_url("http://gitlab.internal:8929/group/proj/-/merge_requests/42?note_id=9"),
        Some(42)
    );
    assert_eq!(mr_iid_from_url("http://gitlab.internal:8929/group/proj"), None);
    assert_eq!(
        mr_iid_from_url("http://gitlab.internal:8929/group/proj/-/merge_requests/"),
        None
    );
    assert_eq!(mr_iid_from_url(""), None);
}

/// A `X-Gitlab-Event: System Hook` payload routes by `event_name`:
/// `merge_request`/`note`/`push` hit their branches, anything else is
/// ignored. No platform configured → empty allowlist → everything allowed.
#[tokio::test]
async fn system_hook_routes_mr_note_push_and_unknown() {
    let handler = GitLabWebhookHandler::new(String::new(), None, MrDispatcher::new(), "test-token".to_string());

    // merge_request → MR branch (action close → received, no dispatch).
    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_MR_HOOK_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["action"], "close");

    // note → Note branch (non-command note → received with preview).
    let note_body = r#"{"event_name":"note","project":{"path_with_namespace":"g/p"},"object_attributes":{"note":"just a comment"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), note_body).await.unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["note_preview"], "just a comment");

    // push → Push branch.
    let push_body = r#"{"event_name":"push","repository":{"homepage":"http://gitlab.internal:8929/g/p"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), push_body).await.unwrap();
    assert_eq!(resp["status"], "received");

    // Unknown event_name → ignored.
    let unknown = r#"{"event_name":"deployment","project":{"path_with_namespace":"g/p"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), unknown).await.unwrap();
    assert_eq!(resp["status"], "ignored");
}

/// A `X-Gitlab-Event: System Hook` payload carrying only `event_type`
/// (GitLab 19.2+ real captured shape, no `event_name`) still routes
/// `merge_request`/`note`/`push` to their branches; a payload with neither
/// event field is ignored.
#[tokio::test]
async fn system_hook_routes_by_event_type() {
    let handler = GitLabWebhookHandler::new(String::new(), None, MrDispatcher::new(), "test-token".to_string());

    // Real captured GitLab 19.2.4 shape: event_type, NO event_name.
    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_MR_HOOK_BODY_EVENT_TYPE)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["action"], "close");

    // note via event_type → Note branch.
    let note_body = r#"{"event_type":"note","project":{"path_with_namespace":"g/p"},"object_attributes":{"note":"just a comment"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), note_body).await.unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["note_preview"], "just a comment");

    // push via event_type → Push branch.
    let push_body = r#"{"event_type":"push","repository":{"homepage":"http://gitlab.internal:8929/g/p"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), push_body).await.unwrap();
    assert_eq!(resp["status"], "received");

    // Neither event_name nor event_type → ignored.
    let neither = r#"{"project":{"path_with_namespace":"g/p"},"object_attributes":{"action":"close"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), neither).await.unwrap();
    assert_eq!(resp["status"], "ignored");
}

/// A system-hook MR whose project is NOT in the matched platform's
/// `allowed_projects` is ignored before dispatch; an allowlisted project
/// proceeds to the MR branch.
#[tokio::test]
async fn system_hook_mr_respects_platform_allowlist() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let mut platform = platform_entry("testbed", "http://gitlab.internal:8929", "glpat-platform", "wh-secret");
    platform.allowed_projects = vec!["group/allowed".to_string()];
    let state = state_with_platforms(vec![platform]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let allowed_body = SYSTEM_MR_HOOK_BODY
        .replace("group/proj", "group/allowed")
        .replace("/42", "/1")
        .replace("\"iid\": 42", "\"iid\": 1");
    let resp = handler
        .handle_event(&system_hook_headers(), &allowed_body)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["action"], "close");

    let denied_body = SYSTEM_MR_HOOK_BODY
        .replace("group/proj", "group/other")
        .replace("/42", "/2")
        .replace("\"iid\": 42", "\"iid\": 2");
    let resp = handler
        .handle_event(&system_hook_headers(), &denied_body)
        .await
        .unwrap();
    assert_eq!(resp["status"], "ignored");
    assert_eq!(resp["reason"], "project not in allowlist");
}

/// Empty `allowed_projects` on a matched platform permits every project.
#[tokio::test]
async fn project_filter_empty_allowlist_allows_everything() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    // platform_entry defaults to an empty allowlist.
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "wh-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let body = SYSTEM_MR_HOOK_BODY
        .replace("group/proj", "any/thing")
        .replace("/42", "/9")
        .replace("\"iid\": 42", "\"iid\": 9");
    let resp = handler.handle_event(&system_hook_headers(), &body).await.unwrap();
    assert_eq!(resp["status"], "received");
}

/// The note hook applies the same allowlist gate before any dispatch, and
/// reaches the Note branch for an allowlisted project.
#[tokio::test]
async fn note_hook_respects_platform_allowlist() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let mut platform = platform_entry("testbed", "http://gitlab.internal:8929", "glpat-platform", "wh-secret");
    platform.allowed_projects = vec!["group/allowed".to_string()];
    let state = state_with_platforms(vec![platform]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        MrDispatcher::new(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    // /review on a non-allowlisted project → ignored.
    let denied = r#"{"event_name":"note","project":{"homepage":"http://gitlab.internal:8929/group/other","path_with_namespace":"group/other"},"object_attributes":{"note":"/review"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), denied).await.unwrap();
    assert_eq!(resp["status"], "ignored");
    assert_eq!(resp["reason"], "project not in allowlist");

    // /review on an allowlisted project reaches the Note branch (no token →
    // no dispatch, no network).
    let allowed = r#"{"event_name":"note","project":{"homepage":"http://gitlab.internal:8929/group/allowed","path_with_namespace":"group/allowed"},"object_attributes":{"note":"/review","url":"http://gitlab.internal:8929/group/allowed/-/merge_requests/5"}}"#;
    let resp = handler.handle_event(&system_hook_headers(), allowed).await.unwrap();
    assert_eq!(resp["status"], "received");
}

// ── MR URL rewrite onto the matched platform's base_url ─────────────

/// `rewrite_url_to_platform` pure semantics: the payload's `external_url`
/// path (query included) is re-hosted onto the platform's reachable
/// `base_url` — the exact local e2e case (localhost vs host.docker.internal)
/// and the NAS case (external :8443 vs container-internal 443).
#[test]
fn rewrite_url_to_platform_rehosts_payload_path_onto_base_url() {
    // Canonical local e2e: payload carries GitLab's external_url (localhost),
    // the platform's base_url is what the review container can actually reach.
    assert_eq!(
        rewrite_url_to_platform(
            "http://localhost:8929/review-lab/demo-app/-/merge_requests/2",
            "http://host.docker.internal:8929",
        ),
        "http://host.docker.internal:8929/review-lab/demo-app/-/merge_requests/2"
    );
    // NAS shape: external_url on the port-mapped :8443, base_url on the
    // container-internal 443 (port-less).
    assert_eq!(
        rewrite_url_to_platform(
            "https://gitlab.islet.space:8443/group/proj/-/merge_requests/2",
            "https://gitlab.islet.space",
        ),
        "https://gitlab.islet.space/group/proj/-/merge_requests/2"
    );
}

#[test]
fn rewrite_url_to_platform_preserves_query_string() {
    assert_eq!(
        rewrite_url_to_platform(
            "http://localhost:8929/group/proj/-/merge_requests/2?note_id=9&tab=discussion",
            "http://host.docker.internal:8929",
        ),
        "http://host.docker.internal:8929/group/proj/-/merge_requests/2?note_id=9&tab=discussion"
    );
}

#[test]
fn rewrite_url_to_platform_handles_base_url_trailing_slash() {
    let url = "http://localhost:8929/group/proj/-/merge_requests/2";
    let base = "http://host.docker.internal:8929";
    let expected = "http://host.docker.internal:8929/group/proj/-/merge_requests/2";
    // `reqwest::Url` normalizes a host-only URL to a trailing `/`; both the
    // with- and without-slash forms must produce the same result.
    assert_eq!(rewrite_url_to_platform(url, &format!("{base}/")), expected);
    assert_eq!(rewrite_url_to_platform(url, base), expected);
}

#[test]
fn rewrite_url_to_platform_unparseable_is_fail_safe() {
    let url = "http://localhost:8929/group/proj/-/merge_requests/2";
    // Unparseable / empty base URL → payload URL verbatim, no panic.
    assert_eq!(rewrite_url_to_platform(url, "not a url"), url);
    assert_eq!(rewrite_url_to_platform(url, ""), url);
    // Unparseable payload URL → returned verbatim too.
    assert_eq!(rewrite_url_to_platform("not a url", "http://host:8929"), "not a url");
}

/// System-hook MR payload in the `open` state with a commit SHA, carrying the
/// instance's `external_url` on the port-mapped :8443 — while the matched
/// platform's `base_url` is the container-reachable :8929 endpoint. The hook
/// must re-host the MR URL (and the dispatch key) onto :8929.
const SYSTEM_MR_OPEN_PORT_BODY: &str = r#"{
  "event_name": "merge_request",
  "project": {
    "homepage": "http://gitlab.internal:8443/review-lab/demo-app",
    "path_with_namespace": "review-lab/demo-app"
  },
  "object_attributes": {
    "action": "open",
    "iid": 2,
    "url": "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2",
    "last_commit": { "id": "abc123" }
  }
}"#;

/// Matched platform (external_url :8443 folds to the unique host-only match
/// on :8929): the MR hook rewrites the payload URL onto the platform's
/// reachable base_url BEFORE dispatch, so both the review provider and the
/// dispatch dedup key target the reachable endpoint.
///
/// The dispatcher's running marker is set synchronously inside `handle_event`
/// (`dispatch_mr_event` → `try_start`), and the spawned review task only runs
/// when this test future next yields — which never happens before the
/// assertions (the dispatcher mutex is uncontended, so `try_start` resolves
/// without suspending). Asserting `InProgress` on the rewritten URL and `Go`
/// on the payload URL therefore deterministically proves which key was used.
#[tokio::test]
async fn mr_hook_matched_platform_rewrites_dispatch_url_to_base_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "wh-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_MR_OPEN_PORT_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["action"], "open");

    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8929/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "dispatch must key on the rewritten base_url-hosted URL"
    );
    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::Go,
        "the payload's unreachable external_url must not be the dispatch key"
    );
}

/// No platform matched (handler without AppState) → empty allowlist (every
/// project allowed) and the payload's `external_url` is the dispatch key
/// verbatim — legacy behavior for setups without a platform entry.
#[tokio::test]
async fn mr_hook_without_matched_platform_keeps_payload_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    );

    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_MR_OPEN_PORT_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");

    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "without a matched platform the payload URL is the dispatch key"
    );
}

/// System-hook note carrying `/review` whose MR URL is built from
/// `project.homepage` (system hooks lack `web_url`) — same external_url :8443
/// host as `SYSTEM_MR_OPEN_PORT_BODY`, matched platform base_url :8929.
const SYSTEM_NOTE_REVIEW_PORT_BODY: &str = r#"{
  "event_name": "note",
  "project": {
    "homepage": "http://gitlab.internal:8443/review-lab/demo-app",
    "path_with_namespace": "review-lab/demo-app"
  },
  "object_attributes": {
    "note": "/review",
    "url": "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2"
  }
}"#;

/// The note hook applies the same rewrite: a matched platform re-hosts the
/// note's MR URL (and dispatch key) onto the reachable base_url.
#[tokio::test]
async fn note_hook_matched_platform_rewrites_dispatch_url_to_base_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "wh-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_NOTE_REVIEW_PORT_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");

    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8929/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "note dispatch must key on the rewritten base_url-hosted URL"
    );
    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::Go,
        "the payload's external_url must not be the note dispatch key"
    );
}

/// Project-level (non-system) MR webhook fixture: carries `project.web_url`
/// (GitLab's external_url) — the MR URL is constructed as
/// `web_url + /-/merge_requests/{iid}` and must be rewritten to the matched
/// platform's base_url exactly like a system hook's `object_attributes.url`.
#[tokio::test]
async fn project_level_mr_hook_rewrites_web_url_to_base_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    let state = state_with_platforms(vec![platform_entry(
        "testbed",
        "http://gitlab.internal:8929",
        "glpat-platform",
        "wh-secret",
    )]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let body = r#"{"project":{"web_url":"http://gitlab.internal:8443/review-lab/demo-app","path_with_namespace":"review-lab/demo-app"},"object_attributes":{"action":"open","iid":2,"last_commit":{"id":"abc123"}}}"#;
    let mut headers = HeaderMap::new();
    headers.insert("X-Gitlab-Event", "Merge Request Hook".parse().unwrap());
    let resp = handler.handle_event(&headers, body).await.unwrap();
    assert_eq!(resp["status"], "received");
    assert_eq!(resp["action"], "open");

    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8929/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "project-level webhook dispatch must rewrite web_url onto base_url"
    );
    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::Go,
        "the payload's web_url must not be the project-level dispatch key"
    );
}

// ── MR URL rewrite onto the matched platform's internal_base_url ─────

/// A matched platform with `internal_base_url` configured rewrites the MR URL
/// onto the INTERNAL endpoint (the container-reachable one), not `base_url`:
/// the NAS shape — payload carries the external :8443, `base_url` is the same
/// external address, `internal_base_url` is the container-internal 443.
#[tokio::test]
async fn mr_hook_internal_base_url_takes_priority_over_base_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    let mut platform = platform_entry("testbed", "http://gitlab.internal:8929", "glpat-platform", "wh-secret");
    platform.internal_base_url = "https://gitlab.islet.space".to_string();
    let state = state_with_platforms(vec![platform]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_MR_OPEN_PORT_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");

    assert_eq!(
        dispatcher
            .try_start(
                "https://gitlab.islet.space/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "dispatch must key on the internal_base_url-hosted URL when configured"
    );
    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8929/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::Go,
        "base_url must NOT be the dispatch key when internal_base_url is set"
    );
}

/// The note hook applies the same internal-first rewrite.
#[tokio::test]
async fn note_hook_internal_base_url_takes_priority_over_base_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    let mut platform = platform_entry("testbed", "http://gitlab.internal:8929", "glpat-platform", "wh-secret");
    platform.internal_base_url = "https://gitlab.islet.space".to_string();
    let state = state_with_platforms(vec![platform]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_NOTE_REVIEW_PORT_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");

    assert_eq!(
        dispatcher
            .try_start(
                "https://gitlab.islet.space/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "note dispatch must key on the internal_base_url-hosted URL when configured"
    );
    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8929/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::Go,
        "base_url must NOT be the note dispatch key when internal_base_url is set"
    );
}

/// Fail-safe through the real hook path: PUT validation forbids a non-empty
/// unparseable `internalBaseUrl`, but a platform constructed outside the PUT
/// pipeline (or a legacy TOML) can carry one — the rewrite then keeps the
/// payload URL verbatim (never panics). When BOTH `base_url` and
/// `internal_base_url` are unparseable the platform cannot even be matched
/// (`host_port` yields `None`), so the payload URL is the dispatch key via the
/// unmatched path — the same observable outcome (`mr_hook_without_matched_platform_keeps_payload_url`).
#[tokio::test]
async fn mr_hook_unparseable_rewrite_target_keeps_payload_url() {
    let _lock = super::RUNTIME_TEST_LOCK.lock().await;
    let _guard = EmptyRuntimeGuard::new();
    let dispatcher = MrDispatcher::new();
    // base_url stays parseable (and matching) so the platform IS matched;
    // internal_base_url is non-empty but unparseable → rewrite fail-safe.
    let mut platform = platform_entry("testbed", "http://gitlab.internal:8929", "glpat-platform", "wh-secret");
    platform.internal_base_url = "not a url".to_string();
    let state = state_with_platforms(vec![platform]);
    let handler = GitLabWebhookHandler::new(
        "default-secret".to_string(),
        None,
        dispatcher.clone(),
        "glpat-default".to_string(),
    )
    .with_app_state(&state);

    let resp = handler
        .handle_event(&system_hook_headers(), SYSTEM_MR_OPEN_PORT_BODY)
        .await
        .unwrap();
    assert_eq!(resp["status"], "received");

    assert_eq!(
        dispatcher
            .try_start(
                "http://gitlab.internal:8443/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::InProgress,
        "an unparseable rewrite target must keep the payload URL as the dispatch key"
    );
    assert_eq!(
        dispatcher
            .try_start(
                "https://gitlab.islet.space/review-lab/demo-app/-/merge_requests/2",
                "probe"
            )
            .await,
        ShouldStart::Go,
        "the unparseable internal target must not leak into the dispatch key"
    );
}
