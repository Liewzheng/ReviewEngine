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
