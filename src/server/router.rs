//! Axum Router construction for the review-engine HTTP server.
//!
//! Assembles the top-level router from its sub-components: health
//! probes, metrics, progress tracking, REST API routes, and optional
//! GitLab/GitHub webhook handlers.

use axum::{routing::get, Router};
use std::sync::Arc;

use super::{api, auth::AuthConfig, github, gitlab, routes, AppState};

/// Build the complete Axum application router.
///
/// Always mounts health, metrics, progress, and `/api/v1` routes.
/// Webhook handlers are only mounted when their respective state
/// is provided.
pub fn build(
    state: Arc<AppState>,
    auth: Arc<AuthConfig>,
    webhook_state: Option<gitlab::GitLabWebhookState>,
    github_webhook_state: Option<github::GitHubWebhookState>,
) -> Router {
    let api_routes = api::routes(state.clone(), auth);

    let mut app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/health/ready", get(routes::health::health_ready))
        .route("/metrics", get(routes::metrics::metrics))
        .route("/progress", get(routes::progress::list_progress))
        .route("/progress/{review_id}", get(routes::progress::get_progress))
        .nest("/api/v1", api_routes)
        .with_state(state);

    if let Some(ws) = webhook_state {
        app = app.merge(gitlab::routes().with_state(ws));
    }

    if let Some(gh_ws) = github_webhook_state {
        app = app.merge(github::routes().with_state(gh_ws));
    }

    app
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the router builds successfully under all
    /// webhook-state combinations.  These tests will panic on
    /// route conflicts, missing states, or invalid path syntax.
    mod builds {
        use super::*;
        use crate::server::dispatcher::MrDispatcher;

        #[tokio::test]
        async fn minimal() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let _app = build(state, auth, None, None);
        }

        #[tokio::test]
        async fn gitlab() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let ws = gitlab::GitLabWebhookState {
                webhook_secret: "test-secret".to_string(),
                dispatcher: MrDispatcher::new(),
            };
            let _app = build(state, auth, Some(ws), None);
        }

        #[tokio::test]
        async fn github() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let gh_ws = github::GitHubWebhookState {
                webhook_secret: "test-secret".to_string(),
                dispatcher: MrDispatcher::new(),
                token: "test-token".to_string(),
            };
            let _app = build(state, auth, None, Some(gh_ws));
        }

        #[tokio::test]
        async fn both() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let ws = gitlab::GitLabWebhookState {
                webhook_secret: "test-secret".to_string(),
                dispatcher: MrDispatcher::new(),
            };
            let gh_ws = github::GitHubWebhookState {
                webhook_secret: "test-secret".to_string(),
                dispatcher: MrDispatcher::new(),
                token: "test-token".to_string(),
            };
            let _app = build(state, auth, Some(ws), Some(gh_ws));
        }

        #[tokio::test]
        async fn with_llm_configs() {
            let configs = vec![crate::models::LLMConfig {
                provider: "openai".to_string(),
                model: "gpt-4".to_string(),
                api_key: "sk-test".to_string(),
                api_base: String::new(),
                max_tokens: 4096,
                temperature: 0.7,
            }];
            let state = Arc::new(AppState::new(configs));
            let auth = Arc::new(AuthConfig::default());
            let _app = build(state, auth, None, None);
        }

        #[tokio::test]
        async fn minimal_does_not_panic() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let _app = build(state, auth, None, None);
            // Router builds without panicking
        }
    }
}
