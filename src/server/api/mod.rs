//! REST API route definitions for the review-engine server.
//!
//! Nests sub-routers for reviews (`/reviews`), repository health scans
//! (`/repo-scan`), system health (`/system`), configuration (`/config`),
//! finding feedback (`/feedback`), and server-sent events (`/events`).
//! Applies CORS middleware that allows
//! all origins and always mounts the authentication middleware (see
//! [`auth_middleware`](crate::server::auth::auth_middleware)): with a token it
//! enforces Bearer / X-API-Key, without one it gates the API behind first-run
//! bootstrap (`401 {"code":"auth_required"}`). The `routes` function assembles
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
pub mod upgrade;

pub fn routes(state: Arc<AppState>, auth: Arc<AuthConfig>) -> Router<Arc<AppState>> {
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let mut router = Router::new()
        .nest("/reviews", review::routes())
        .nest("/repo-scan", repo::routes())
        // /system hosts both the existing system endpoints and the
        // self-upgrade endpoints (disjoint paths, merged into one nest).
        .nest("/system", system::routes().merge(upgrade::routes()))
        .nest("/config", config::routes())
        .nest("/events", events::routes())
        .nest("/dashboard", dashboard::routes())
        .nest("/queue", queue::routes())
        .nest("/llm", llm::routes())
        .nest("/logs", logs::routes())
        .nest("/feedback", feedback::routes())
        .layer(cors);

    // Always mount the auth gate, not just when a token was configured at
    // startup: with no token the server runs in first-run bootstrap mode and
    // every endpoint returns `401 {"code":"auth_required"}` except the
    // bootstrap endpoints (PUT /system/token, GET /system/auth-status). With a
    // token it enforces Bearer / X-API-Key as before.
    router = router.layer(middleware::from_fn(crate::server::auth::auth_middleware));

    router.layer(axum::Extension(auth)).with_state(state)
}
