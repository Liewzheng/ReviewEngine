use std::time::{SystemTime, UNIX_EPOCH};

use axum::{http::StatusCode, Json};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::sync::Arc;

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
/// belongs to. GitLab project-level MR/Note hooks carry `project.web_url`;
/// admin-level System Hooks carry no `project.web_url`, so the chain falls
/// back through `project.homepage` → `repository.homepage` → (for MR/Note
/// events) the full URL in `object_attributes.url`. `None` when the body
/// carries none of these (or is not JSON) — the caller then uses the runtime
/// default.
pub(crate) fn payload_instance_url(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    parsed["project"]["web_url"]
        .as_str()
        .or_else(|| parsed["project"]["homepage"].as_str())
        .or_else(|| parsed["repository"]["homepage"].as_str())
        .or_else(|| parsed["object_attributes"]["url"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Extract the event name from a System Hook payload. GitLab 19.2+ system
/// hooks send **`event_type`** (`merge_request`/`note`/`push`) instead of
/// `event_name`; older versions and the docs send `event_name`. Prefer
/// `event_name`, fall back to `event_type`. Empty string when neither is
/// present (or the body is not JSON / the value is not a string) — the caller
/// treats that as an unknown event and ignores it.
pub(crate) fn system_hook_event_name(body: &str) -> String {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    parsed["event_name"]
        .as_str()
        .or_else(|| parsed["event_type"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The "unique verification platform" fallback: when EXACTLY ONE configured
/// git platform can verify webhooks ([`crate::models::GitPlatformConfig::has_webhook_verification`]),
/// return it; `None` when zero or multiple entries are verification-capable
/// (or the list is empty). Used when a payload carries no instance URL —
/// GitLab System Hooks' **Test** button sends a sample payload with no
/// `project`/URL at all — or its URL matched no platform: a single
/// verification-capable entry is unambiguous, so it safely takes over; zero
/// or multiple entries keep the runtime default — never guess.
pub(crate) fn unique_verification_platform(
    platforms: &[crate::models::GitPlatformConfig],
) -> Option<crate::models::GitPlatformConfig> {
    let mut verification = platforms.iter().filter(|p| p.has_webhook_verification());
    let hit = verification.next()?.clone();
    if verification.next().is_some() {
        return None;
    }
    Some(hit)
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

    /// Return the effective config for a specific webhook body.
    ///
    /// Resolution order:
    /// 1. **URL match** — when the payload carries an instance URL
    ///    (`project.web_url` / `project.homepage` / `repository.homepage` /
    ///    `object_attributes.url`) and it matches a configured git platform
    ///    (strict host[:port], with `find_git_platform_for_url`'s unique-host
    ///    port fold), THAT platform's webhook_secret / signing_secret / token
    ///    take over both verification and review dispatch. Multi-platform
    ///    semantics are unchanged. A URL-matched entry WITHOUT webhook
    ///    verification (token-only, configured for REST review routing)
    ///    deliberately does not take over — webhooks for its host keep the
    ///    runtime default, so adding an instance for routing never breaks an
    ///    existing webhook setup for the same host.
    /// 2. **Unique verification platform fallback** — when the payload has no
    ///    instance URL, or its URL matched no platform (host mismatch, e.g.
    ///    `base_url` = `host.docker.internal` while the payload carries
    ///    `localhost`), exactly ONE verification-capable platform
    ///    (`unique_verification_platform`) unambiguously takes over. This is
    ///    what makes GitLab System Hooks' **Test** button work: it sends a
    ///    sample payload with no URL, which used to resolve to no platform →
    ///    403 "no verification configured". Zero or multiple
    ///    verification-capable entries keep the runtime default — never guess.
    pub(crate) fn effective_config_for_body(&self, body: &str) -> GitLabRuntimeConfig {
        let fallback = self.effective_config();
        let Some(state) = self.app_state.as_ref().and_then(|w| w.upgrade()) else {
            return fallback;
        };
        let platforms = state.git_platforms.read().unwrap().clone();
        // A payload carrying an instance URL resolves strictly by URL: the
        // URL-matched verification-capable platform wins regardless of how
        // many entries exist; a token-only URL match keeps the runtime default
        // (the host has no verification setup on purpose — never the fallback).
        if let Some(url) = payload_instance_url(body) {
            if let Some(platform) = crate::models::find_git_platform_for_url(&platforms, &url) {
                if platform.has_webhook_verification() {
                    tracing::debug!(platform = %platform.name, "gitlab webhook matched configured git platform");
                    return GitLabRuntimeConfig::from_platform(platform);
                }
                return fallback;
            }
        }
        // No URL, or the URL matched no platform: when exactly ONE platform can
        // verify webhooks it unambiguously takes over; zero or multiple keep
        // the runtime default — never guess.
        if let Some(platform) = unique_verification_platform(&platforms) {
            tracing::debug!(platform = %platform.name, "gitlab webhook fell back to unique verification platform");
            return GitLabRuntimeConfig::from_platform(&platform);
        }
        fallback
    }

    /// Return the configured git platform for `body`'s webhook, or `None`.
    ///
    /// Mirrors `effective_config_for_body`'s resolution order so the platform
    /// driving the `allowed_projects` allowlist AND its `base_url` (rewriting
    /// payload MR URLs onto the reachable endpoint before review dispatch) is
    /// the same one whose credentials verified the body:
    /// 1. URL-matched verification-capable platform (`None` when the match is
    ///    token-only — it must not take over);
    /// 2. else the unique verification platform fallback
    ///    (`unique_verification_platform`), so a no-URL payload (System Hooks
    ///    **Test** button) or a host-mismatch payload (localhost vs
    ///    `host.docker.internal`) still resolves to the single
    ///    verification-capable entry and its allowlist + reachable URL.
    /// `None` when there is no AppState, no platform matched and no unique
    /// verification platform exists → empty allowlist (every project allowed)
    /// and the payload URL kept verbatim (legacy).
    pub(crate) fn matched_platform(&self, body: &str) -> Option<crate::models::GitPlatformConfig> {
        let state = self.app_state.as_ref()?.upgrade()?;
        let platforms = state.git_platforms.read().unwrap().clone();
        if let Some(url) = payload_instance_url(body) {
            if let Some(platform) = crate::models::find_git_platform_for_url(&platforms, &url) {
                if platform.has_webhook_verification() {
                    return Some(platform.clone());
                }
                return None;
            }
        }
        unique_verification_platform(&platforms)
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

    /// Route a GitLab System Hook (admin-level) payload by its event name
    /// (`system_hook_event_name`: `event_name`, falling back to `event_type`
    /// for GitLab 19.2+): `merge_request`/`note`/`push` map to the
    /// corresponding project hook handlers (they share the same payload
    /// shape); any other event name is ignored with a debug log.
    async fn handle_system_hook(
        &self,
        body: &str,
        token: &str,
        platform: Option<crate::models::GitPlatformConfig>,
        task_store: Option<Arc<crate::server::task_queue::TaskStore>>,
        db: Option<Arc<crate::store::SqlxStore>>,
    ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
        let event_name = system_hook_event_name(body);
        match event_name.as_str() {
            "merge_request" => {
                super::handle_mr_hook(body, &self.dispatcher, token, platform, task_store.clone(), db.clone())
                    .await
                    .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"}))))
            }
            "note" => super::handle_note_hook(body, &self.dispatcher, token, platform, task_store.clone(), db)
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            "push" => super::handle_push_hook(body)
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            other => {
                tracing::debug!("Ignoring unknown GitLab System Hook event_name: {}", other);
                Ok(Json(serde_json::json!({ "status": "ignored" })))
            }
        }
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
        // The matched git platform (if any) drives BOTH the project
        // `allowed_projects` allowlist and the review URL rewrite (re-hosting
        // the payload's `external_url` onto the platform's reachable
        // `base_url` before dispatch). Resolved ONCE here — a single payload
        // parse — and handed to the hook, which uses it for both. `None` when
        // no platform matched (or none can verify) → empty allowlist (every
        // project allowed) and the payload URL kept verbatim (legacy).
        let platform = self.matched_platform(body);
        // The shared task store (weak-handled via AppState) so webhook-dispatched
        // reviews record a task entry: create → running → completed/failed. `None`
        // in tests and legacy paths — the review still runs, just without a record.
        // The DB handle (0.10.0) feeds Note-hook ingestion; `None` = 0.9 behaviour.
        let app_state = self.app_state.as_ref().and_then(|w| w.upgrade());
        let task_store = app_state.as_ref().and_then(|s| s.task_store.clone());
        let db = app_state.and_then(|s| s.db.clone());

        match event {
            "Merge Request Hook" => {
                super::handle_mr_hook(body, &self.dispatcher, &token, platform, task_store.clone(), db.clone())
                    .await
                    .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"}))))
            }
            "Note Hook" => {
                super::handle_note_hook(body, &self.dispatcher, &token, platform, task_store.clone(), db.clone())
                    .await
                    .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"}))))
            }
            "Push Hook" => super::handle_push_hook(body)
                .await
                .map_err(|status| (status, Json(serde_json::json!({"error": "request failed"})))),
            "System Hook" => self.handle_system_hook(body, &token, platform, task_store, db).await,
            _ => {
                tracing::debug!("Ignoring unsupported GitLab event: {}", event);
                Ok(Json(serde_json::json!({ "status": "ignored" })))
            }
        }
    }
}
