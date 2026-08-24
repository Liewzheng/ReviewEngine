use std::time::{SystemTime, UNIX_EPOCH};

use axum::{http::StatusCode, Json};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use super::super::dispatcher::MrDispatcher;
use super::super::webhook::WebhookHandler;
use super::{gitlab_runtime, GitLabRuntimeConfig};

use async_trait::async_trait;
use axum::http::HeaderMap;

pub(crate) type HmacSha256 = Hmac<Sha256>;

/// GitLab webhook handler.
///
/// Supports two verification methods (can be configured independently or together):
/// 1. **Secret token** (`X-Gitlab-Token` header) — legacy, configured via `webhook_secret`.
/// 2. **Signing token** (`webhook-signature` header, Standard Webhooks HMAC-SHA256 of
///    `{message_id}.{timestamp}.{body}`) — GitLab 19.0+, configured via `signing_secret`.
///
/// See: <https://docs.gitlab.com/19.0/user/project/integrations/webhooks/#signing-tokens>
#[derive(Clone)]
pub struct GitLabWebhookHandler {
    pub webhook_secret: String,
    pub signing_secret: Option<String>,
    pub(crate) signing_key: Option<Vec<u8>>,
    pub dispatcher: MrDispatcher,
    pub token: String,
    /// Shared multi-platform configs (weak handle to the server's `AppState`
    /// — the handler is mounted next to it, never owning it). `None` in
    /// tests and legacy startup paths: only the runtime default applies.
    app_state: Option<std::sync::Weak<crate::server::AppState>>,
}

/// Extract the instance URL identifying which git platform a webhook payload
/// belongs to: GitLab MR/Note hooks carry `project.web_url`, Push hooks
/// carry `repository.homepage`. `None` when the body carries neither (or is
/// not JSON) — the caller then uses the runtime default.
fn payload_instance_url(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    parsed["project"]["web_url"]
        .as_str()
        .or_else(|| parsed["repository"]["homepage"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl GitLabWebhookHandler {
    /// Return the effective runtime config: if the global was initialised
    /// (e.g. from the UI) use that, otherwise fall back to `self.*` so
    /// tests and legacy startup paths continue to work.
    pub(crate) fn effective_config(&self) -> GitLabRuntimeConfig {
        gitlab_runtime()
            .try_read()
            .ok()
            .filter(|rt| {
                // Only use runtime if it was explicitly initialised
                !rt.webhook_secret.is_empty() || rt.signing_secret.is_some() || !rt.token.is_empty()
            })
            .map(|rt| rt.clone())
            .unwrap_or_else(|| GitLabRuntimeConfig::from_handler(self))
    }

    /// Return the effective config for a specific webhook body: when the
    /// payload's project/repository URL host matches a configured git
    /// platform, THAT platform's webhook_secret / signing_secret / token
    /// take over both verification and review dispatch; otherwise the
    /// runtime default applies.
    ///
    /// A matched platform WITHOUT any webhook verification credential
    /// (token-only entry, configured for REST review routing) deliberately
    /// does not take over: webhooks for its host keep the runtime default,
    /// so adding an instance for routing never breaks an existing webhook
    /// setup for the same host.
    pub(crate) fn effective_config_for_body(&self, body: &str) -> GitLabRuntimeConfig {
        let fallback = self.effective_config();
        let Some(state) = self.app_state.as_ref().and_then(|w| w.upgrade()) else {
            return fallback;
        };
        let Some(url) = payload_instance_url(body) else {
            return fallback;
        };
        let platforms = state.git_platforms.read().unwrap().clone();
        match crate::models::find_git_platform_for_url(&platforms, &url) {
            Some(platform) if platform.has_webhook_verification() => {
                tracing::debug!(platform = %platform.name, "gitlab webhook matched configured git platform");
                GitLabRuntimeConfig::from_platform(platform)
            }
            _ => fallback,
        }
    }

    /// Create a new GitLab webhook handler.
    ///
    /// `webhook_secret` — legacy `X-Gitlab-Token` verification (empty string disables).
    /// `signing_secret` — Standard Webhooks HMAC-SHA256 signature verification (`None` disables).
    /// Expected format: `whsec_<base64-encoded-key>`.
    pub fn new(
        webhook_secret: String,
        signing_secret: Option<String>,
        dispatcher: MrDispatcher,
        token: String,
    ) -> Self {
        let signing_key = signing_secret.as_ref().filter(|s| !s.is_empty()).and_then(|s| {
            s.strip_prefix("whsec_")
                .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        });
        Self {
            webhook_secret,
            signing_secret,
            signing_key,
            dispatcher,
            token,
            app_state: None,
        }
    }

    /// Attach the shared `AppState` so webhook verification/dispatch can
    /// match the payload's instance URL against the configured git
    /// platforms. Stored as a weak handle: the router owns the state, and
    /// tests construct handlers without one.
    pub fn with_app_state(mut self, state: &std::sync::Arc<crate::server::AppState>) -> Self {
        self.app_state = Some(std::sync::Arc::downgrade(state));
        self
    }

    /// Verify `webhook-signature` header (GitLab 19.0+ signing tokens, Standard Webhooks).
    fn verify_signing(
        &self,
        cfg: &GitLabRuntimeConfig,
        headers: &HeaderMap,
        body: &str,
    ) -> Result<(), (StatusCode, Json<Value>)> {
        let key = match &cfg.signing_key {
            Some(k) => k,
            None => {
                tracing::warn!("GitLab webhook signing secret is invalid");
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": "invalid signing secret"})),
                ));
            }
        };

        let signature_header = headers
            .get("webhook-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if signature_header.is_empty() {
            tracing::warn!("GitLab webhook signing secret configured but webhook-signature header missing");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "missing webhook-signature header"})),
            ));
        }

        let message_id = headers.get("webhook-id").and_then(|v| v.to_str().ok()).unwrap_or("");
        let timestamp = headers
            .get("webhook-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if message_id.is_empty() || timestamp.is_empty() {
            tracing::warn!("GitLab webhook signing missing webhook-id or webhook-timestamp header");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "missing webhook-id or webhook-timestamp header"})),
            ));
        }

        // Replay protection: timestamp must be within 5 minutes.
        let timestamp_seconds = timestamp.parse::<i64>().map_err(|_| {
            tracing::warn!("GitLab webhook signing invalid timestamp: {}", timestamp);
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid webhook timestamp"})),
            )
        })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| {
                (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({"error": "invalid server time"})),
                )
            })?
            .as_secs() as i64;
        if timestamp_seconds.abs_diff(now) > 300 {
            tracing::warn!("GitLab webhook signing timestamp out of tolerance");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "webhook timestamp out of tolerance"})),
            ));
        }

        // Build message: "{message_id}.{timestamp}.{body}"
        let message = format!("{}.{}.{}", message_id, timestamp, body);

        // Compute HMAC-SHA256.
        let mut mac = HmacSha256::new_from_slice(key).map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid signing key"})),
            )
        })?;
        mac.update(message.as_bytes());
        let digest = mac.finalize().into_bytes();
        let computed_sig = format!("v1,{}", base64::engine::general_purpose::STANDARD.encode(&digest));

        // Compare against each signature in the header (constant-time).
        let valid = signature_header
            .split_whitespace()
            .any(|sig| bool::from(subtle::ConstantTimeEq::ct_eq(computed_sig.as_bytes(), sig.as_bytes())));

        if valid {
            Ok(())
        } else {
            tracing::warn!("GitLab webhook signing signature mismatch — check GITLAB_WEBHOOK_SIGNING_SECRET");
            Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid signing signature"})),
            ))
        }
    }

    /// Verify legacy `X-Gitlab-Token` header.
    fn verify_secret_token(
        &self,
        cfg: &GitLabRuntimeConfig,
        headers: &HeaderMap,
    ) -> Result<(), (StatusCode, Json<Value>)> {
        if cfg.webhook_secret.is_empty() {
            return Ok(()); // legacy secret not configured — skip this check
        }

        let token = headers
            .get("X-Gitlab-Token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let token_bytes = token.as_bytes();
        let secret_bytes = cfg.webhook_secret.as_bytes();
        if token_bytes.len() != secret_bytes.len() {
            tracing::warn!("GitLab webhook received with invalid token");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid token"})),
            ));
        }
        if !bool::from(subtle::ConstantTimeEq::ct_eq(token_bytes, secret_bytes)) {
            tracing::warn!("GitLab webhook received with invalid token");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "invalid token"})),
            ));
        }

        Ok(())
    }
}

#[async_trait]
impl WebhookHandler for GitLabWebhookHandler {
    fn path(&self) -> &'static str {
        "/webhook/gitlab"
    }

    fn name(&self) -> &'static str {
        "gitlab"
    }

    async fn verify(&self, headers: &HeaderMap, body: &str) -> Result<(), (StatusCode, Json<Value>)> {
        let cfg = self.effective_config_for_body(body);
        let signing_configured = cfg.signing_secret.as_ref().map_or(false, |s| !s.is_empty());
        let legacy_configured = !cfg.webhook_secret.is_empty();
        let signature_header_present = headers.get("webhook-signature").is_some();

        // Security policy: when the `webhook-signature` header is present, the
        // signature MUST be verified. If it is invalid, the request is rejected.
        // We do NOT fall back to the legacy `X-Gitlab-Token` check on signature
        // failure because that would allow a downgrade attack. We only fall back
        // to the legacy token when the signature header is absent.
        if signing_configured && signature_header_present {
            return self.verify_signing(&cfg, headers, body);
        }

        // Otherwise fall back to the legacy token if it is configured.
        if legacy_configured {
            return self.verify_secret_token(&cfg, headers);
        }

        // Signing is configured but the signature header is missing, and no
        // legacy fallback is available.
        if signing_configured {
            tracing::warn!("GitLab webhook signing secret configured but webhook-signature header missing");
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "missing webhook-signature header"})),
            ));
        }

        tracing::warn!("GitLab webhook rejected: no verification configured");
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "no verification configured"})),
        ))
    }

    async fn handle_event(&self, headers: &HeaderMap, body: &str) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        let event = headers
            .get("X-Gitlab-Event")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Clone the token before the match so the RwLock guard is dropped
        // before any .await call (RwLockReadGuard is not Send). The token is
        // resolved per payload: a body whose instance URL matches a
        // configured git platform dispatches with that platform's token.
        let token = self.effective_config_for_body(body).token.clone();

        match event {
            "Merge Request Hook" => super::handle_mr_hook(body, &self.dispatcher, &token)
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            "Note Hook" => super::handle_note_hook(body, &self.dispatcher, &token)
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            "Push Hook" => super::handle_push_hook(body)
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            _ => {
                tracing::debug!("Ignoring unsupported GitLab event: {}", event);
                Ok(Json(serde_json::json!({ "status": "ignored" })))
            }
        }
    }
}
