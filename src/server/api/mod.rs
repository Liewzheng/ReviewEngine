//! REST API route definitions for the review-engine server.
//!
//! Nests sub-routers for reviews (`/reviews`), repository health scans
//! (`/repo-scan`), system health (`/system`), configuration (`/config`),
//! finding feedback (`/feedback`), and server-sent events (`/events`).
//! Applies CORS middleware that allows
//! all origins and optionally adds authentication middleware when
//! [`AuthConfig`] indicates auth is enabled. The `routes` function assembles
//! the full [`Router`] with shared [`AppState`] and returns it to the caller.

use axum::{middleware, Router};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

use super::AppState;
use crate::server::auth::AuthConfig;

pub mod callback;
pub mod config;
pub mod dashboard;
pub mod events;
pub mod feedback;
pub mod llm;
pub mod logs;
pub mod queue;
pub mod repo;
pub mod review;
pub mod system;
pub mod types;

pub fn routes(state: Arc<AppState>, auth: Arc<AuthConfig>) -> Router<Arc<AppState>> {
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let mut router = Router::new()
        .nest("/reviews", review::routes())
        .nest("/repo-scan", repo::routes())
        .nest("/system", system::routes())
        .nest("/config", config::routes())
        .nest("/events", events::routes())
        .nest("/dashboard", dashboard::routes())
        .nest("/queue", queue::routes())
        .nest("/llm", llm::routes())
        .nest("/logs", logs::routes())
        .nest("/feedback", feedback::routes())
        .layer(cors);

    if auth.is_enabled() {
        router = router.layer(middleware::from_fn(crate::server::auth::auth_middleware));
    }

    router.layer(axum::Extension(auth)).with_state(state)
}
