//! GitLab webhook handler and runtime configuration.
//!
//! Supports two verification methods (can be configured independently or together):
//! 1. **Secret token** (`X-Gitlab-Token` header) — legacy, configured via `webhook_secret`.
//! 2. **Signing token** (`webhook-signature` header, Standard Webhooks HMAC-SHA256 of
//!    `{message_id}.{timestamp}.{body}`) — GitLab 19.0+, configured via `signing_secret`.
//!
//! See: <https://docs.gitlab.com/19.0/user/project/integrations/webhooks/#signing-tokens>

mod handler;
mod hooks;

#[cfg(test)]
mod tests;

pub use super::webhook::WebhookHandler;
pub use handler::GitLabWebhookHandler;
pub use hooks::{
    dispatch_mr_event, handle_mr_hook, handle_mr_in_progress, handle_note_hook, handle_push_hook,
    note_starts_with_command, parse_mr_hook_payload, spawn_mr_review_task, MrHookPayload,
};

use std::sync::{OnceLock, RwLock};

/// Runtime-mutable GitLab configuration shared between the webhook handler
/// and the UI config API. Updated by `PUT /api/v1/config` without restart.
#[derive(Clone)]
pub struct GitLabRuntimeConfig {
    pub webhook_secret: String,
    pub signing_secret: Option<String>,
    pub signing_key: Option<Vec<u8>>,
    pub token: String,
}

impl GitLabRuntimeConfig {
    pub fn from_handler(handler: &GitLabWebhookHandler) -> Self {
        Self {
            webhook_secret: handler.webhook_secret.clone(),
            signing_secret: handler.signing_secret.clone(),
            signing_key: handler.signing_key.clone(),
            token: handler.token.clone(),
        }
    }
}

/// Accessor for the global GitLab runtime config.
pub(crate) fn gitlab_runtime() -> &'static RwLock<GitLabRuntimeConfig> {
    static INSTANCE: OnceLock<RwLock<GitLabRuntimeConfig>> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        RwLock::new(GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: String::new(),
        })
    })
}

/// Initialise the global GitLab runtime config from a handler (called at startup).
pub fn init_gitlab_runtime(handler: &GitLabWebhookHandler) {
    let mut rt = gitlab_runtime().write().unwrap();
    *rt = GitLabRuntimeConfig::from_handler(handler);
}
