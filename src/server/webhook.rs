//! Provider-agnostic webhook handling trait.
//!
//! Defines the [`WebhookHandler`] trait and a shared [`handle_webhook`]
//! helper that every provider-specific webhook route uses for verification
//! and response mapping.

use async_trait::async_trait;
use axum::{http::HeaderMap, http::StatusCode, response::IntoResponse, Json};
use serde_json::Value;
use std::sync::Arc;

/// Provider-agnostic webhook handler.
///
/// Implementations supply their own route path, verification logic, and
/// event dispatch.
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    /// Route path for this webhook handler (e.g. `/webhook/github`).
    fn path(&self) -> &'static str;

    /// Short provider name used for logging.
    fn name(&self) -> &'static str;

    /// Verify the incoming webhook request.
    ///
    /// Returns `Ok(())` when the request is authentic, otherwise a status code
    /// and JSON error body.
    async fn verify(&self, headers: &HeaderMap, body: &str) -> Result<(), (StatusCode, Json<Value>)>;

    /// Handle the webhook event.
    ///
    /// Returns the JSON response on success, or a status code and JSON error
    /// body on failure.
    async fn handle_event(&self, headers: &HeaderMap, body: &str) -> Result<Json<Value>, (StatusCode, Json<Value>)>;
}

/// Shared entry point for all webhook routes.
///
/// Verifies the request, dispatches the event, and maps the result to an
/// Axum response. Rejects bodies larger than 1 MiB to prevent memory DoS.
const MAX_WEBHOOK_BODY_SIZE: usize = 1024 * 1024;

pub async fn handle_webhook(handler: Arc<dyn WebhookHandler>, headers: HeaderMap, body: String) -> impl IntoResponse {
    if body.len() > MAX_WEBHOOK_BODY_SIZE {
        tracing::warn!(
            "{} webhook body exceeds {} bytes, rejecting",
            handler.name(),
            MAX_WEBHOOK_BODY_SIZE
        );
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "payload too large"})),
        )
            .into_response();
    }

    if let Err((status, json)) = handler.verify(&headers, &body).await {
        tracing::warn!("{} webhook verification failed", handler.name());
        return (status, json).into_response();
    }

    match handler.handle_event(&headers, &body).await {
        Ok(json) => (StatusCode::OK, json).into_response(),
        Err((status, json)) => (status, json).into_response(),
    }
}
