//! Axum Router construction for the review-engine HTTP server.
//!
//! Assembles the top-level router from its sub-components: health
//! probes, metrics, progress tracking, REST API routes, and optional
//! webhook handlers.

use axum::{
    extract::Request,
    http::{header::CACHE_CONTROL, HeaderMap, HeaderValue},
    middleware::{self, Next},
    response::{Html, Response},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::services::ServeDir;

use super::{api, auth::AuthConfig, routes, webhook, AppState};
use webhook::WebhookHandler;

async fn serve_frontend() -> Html<&'static str> {
    Html("<h1>Review Engine</h1><p>Dashboard coming soon. Run <code>npm run build</code> in frontend/ to build the Vue app.</p>")
}

/// Cache-Control policy for the built SPA (content-hashed assets).
///
/// - `/` and `/index.html` — `no-cache, must-revalidate`. The entry HTML pins
///   hashed asset filenames; a stale copy references chunks that vanish after
///   an upgrade (blank page). `no-cache` still allows storing but forces a
///   conditional request on every load (ServeDir answers 304 when unchanged);
///   `must-revalidate` forbids serving the stored copy when revalidation is
///   impossible. Chosen over `no-store` because revalidation gives the same
///   freshness guarantee while keeping 304 cheap — index.html is the one file
///   whose freshness gates the whole app.
/// - `/assets/**` — the bundler emits content-hashed filenames, so a given URL
///   never changes: safe to cache for a year, `immutable` skips revalidation.
/// - Anything else (favicon, icons, …) — no explicit policy, browser defaults.
fn cache_control_for_path(path: &str) -> Option<HeaderValue> {
    if path == "/" || path == "/index.html" {
        Some(HeaderValue::from_static("no-cache, must-revalidate"))
    } else if path.starts_with("/assets/") {
        Some(HeaderValue::from_static("public, max-age=31536000, immutable"))
    } else {
        None
    }
}

/// Apply [`cache_control_for_path`] to static-file responses. Scoped to the
/// static fallback only (see `build`), so API/health routes keep their
/// existing no-Cache-Control behavior. The header is set only on successful
/// responses: an immutably cached 404 for a missing asset would otherwise keep
/// the app broken even after the file is deployed.
async fn static_cache_control(request: Request, next: Next) -> Response {
    let cache_control = cache_control_for_path(request.uri().path());
    let mut response = next.run(request).await;
    if let Some(value) = cache_control {
        if response.status().is_success() {
            response.headers_mut().insert(CACHE_CONTROL, value);
        }
    }
    response
}

/// Detect the frontend static assets directory (Docker or local dev).
fn static_dir() -> Option<String> {
    for path in ["/app/frontend/dist", "./frontend/dist"] {
        if std::path::Path::new(path).is_dir() {
            return Some(path.to_string());
        }
    }
    None
}

/// Build the complete Axum application router.
///
/// Always mounts health, metrics, progress, and `/api/v1` routes.
/// Webhook handlers are mounted for each handler provided in the vector.
pub fn build(state: Arc<AppState>, auth: Arc<AuthConfig>, webhook_handlers: Vec<Arc<dyn WebhookHandler>>) -> Router {
    let api_routes = api::routes(state.clone(), auth);

    let mut app = Router::new();

    // Serve built frontend static files if available, otherwise placeholder.
    // The cache-control middleware is scoped to the static fallback: the layer
    // call happens before any route is added, and `Router::layer` only covers
    // what is registered at that point (fallback included), so API/health and
    // webhook routes below are unaffected.
    if let Some(dir) = static_dir() {
        app = app
            .fallback_service(ServeDir::new(dir))
            .layer(middleware::from_fn(static_cache_control));
    } else {
        app = app.route("/", get(serve_frontend));
    }

    app = app
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
                Some("test-signing".to_string()),
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
                    Some("test-signing".to_string()),
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
                disable_thinking: None,
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

    /// Path → Cache-Control decision table behind `static_cache_control`.
    /// The middleware itself is thin (decide, run, stamp on 2xx), so the
    /// decision function carries the contract and is exercised end-to-end by
    /// the integration test in tests/server.rs.
    mod cache_control {
        use super::*;

        #[test]
        fn index_paths_revalidate_every_load() {
            for path in ["/", "/index.html"] {
                let value = cache_control_for_path(path)
                    .unwrap_or_else(|| panic!("{path} must carry an explicit cache policy"));
                assert_eq!(value, "no-cache, must-revalidate", "unexpected policy for {path}");
            }
        }

        #[test]
        fn hashed_assets_are_immutable() {
            for path in ["/assets/app-CqstUsos.js", "/assets/Dashboard-wlAd9MnP.css"] {
                let value = cache_control_for_path(path)
                    .unwrap_or_else(|| panic!("{path} must carry an explicit cache policy"));
                assert_eq!(
                    value, "public, max-age=31536000, immutable",
                    "unexpected policy for {path}"
                );
            }
        }

        #[test]
        fn other_paths_keep_default_policy() {
            // Note "/assets" (no trailing slash) has no policy: ServeDir only
            // redirects it to "/assets/", and a bare-404 redirect must not be
            // cached for a year.
            for path in [
                "/favicon.svg",
                "/icons.svg",
                "/assets",
                "/index.html.bak",
                "/health",
                "/api/v1/config",
            ] {
                assert!(
                    cache_control_for_path(path).is_none(),
                    "{path} must keep the default (absent) cache policy"
                );
            }
        }
    }
}
