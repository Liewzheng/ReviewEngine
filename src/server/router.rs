//! Axum Router construction for the review-engine HTTP server.
//!
//! Assembles the top-level router from its sub-components: health
//! probes, metrics, progress tracking, REST API routes, and optional
//! webhook handlers.

use axum::{
    http::HeaderMap,
    routing::{get, post},
    Router,
};
use std::sync::Arc;

use super::{api, auth::AuthConfig, routes, webhook, AppState};
use webhook::WebhookHandler;

/// Build the complete Axum application router.
///
/// Always mounts health, metrics, progress, and `/api/v1` routes.
/// Webhook handlers are mounted for each handler provided in the vector.
pub fn build(state: Arc<AppState>, auth: Arc<AuthConfig>, webhook_handlers: Vec<Arc<dyn WebhookHandler>>) -> Router {
    let api_routes = api::routes(state.clone(), auth);

    let mut app = Router::new()
        .route("/health", get(routes::health::health))
        .route("/health/ready", get(routes::health::health_ready))
        .route("/metrics", get(routes::metrics::metrics))
        .route("/progress", get(routes::progress::list_progress))
        .route("/progress/{review_id}", get(routes::progress::get_progress))
        .nest("/api/v1", api_routes);

    for handler in webhook_handlers {
        let h = handler.clone();
        app = app.route(
            handler.path(),
            post(move |headers: HeaderMap, body: String| async move {
                webhook::handle_webhook(h.clone(), headers, body).await
            }),
        );
    }

    app.with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the router builds successfully under all
    /// webhook handler combinations.  These tests will panic on
    /// route conflicts, missing states, or invalid path syntax.
    mod builds {
        use super::*;
        use crate::server::dispatcher::MrDispatcher;
        use crate::server::github::GitHubWebhookHandler;
        use crate::server::gitlab::GitLabWebhookHandler;

        #[tokio::test]
        async fn minimal() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let handlers: Vec<Arc<dyn WebhookHandler>> = vec![];
            let _app = build(state, auth, handlers);
        }

        #[tokio::test]
        async fn gitlab() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let handlers: Vec<Arc<dyn WebhookHandler>> = vec![Arc::new(GitLabWebhookHandler::new(
                "test-secret".to_string(),
                MrDispatcher::new(),
                "test-token".to_string(),
            ))];
            let _app = build(state, auth, handlers);
        }

        #[tokio::test]
        async fn github() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let handlers: Vec<Arc<dyn WebhookHandler>> = vec![Arc::new(GitHubWebhookHandler::new(
                "test-secret".to_string(),
                MrDispatcher::new(),
                "test-token".to_string(),
            ))];
            let _app = build(state, auth, handlers);
        }

        #[tokio::test]
        async fn both() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let handlers: Vec<Arc<dyn WebhookHandler>> = vec![
                Arc::new(GitLabWebhookHandler::new(
                    "test-secret".to_string(),
                    MrDispatcher::new(),
                    "test-token".to_string(),
                )),
                Arc::new(GitHubWebhookHandler::new(
                    "test-secret".to_string(),
                    MrDispatcher::new(),
                    "test-token".to_string(),
                )),
            ];
            let _app = build(state, auth, handlers);
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
            let handlers: Vec<Arc<dyn WebhookHandler>> = vec![];
            let _app = build(state, auth, handlers);
        }

        #[tokio::test]
        async fn minimal_does_not_panic() {
            let state = Arc::new(AppState::new(vec![]));
            let auth = Arc::new(AuthConfig::default());
            let handlers: Vec<Arc<dyn WebhookHandler>> = vec![];
            let _app = build(state, auth, handlers);
            // Router builds without panicking
        }
    }
}
