//! REST API endpoints for creating, listing, and deleting review tasks.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team

mod handlers;
mod resolve;
mod task;
#[cfg(test)]
mod tests;

use axum::{
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;

use crate::server::AppState;

pub(crate) use self::task::{task_status_str, task_to_status};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", post(handlers::submit_review))
        .route("/", get(handlers::list_reviews))
        .route("/{task_id}", get(handlers::get_review))
        .route("/{task_id}", delete(handlers::delete_review))
        .route("/{task_id}/rerun", post(handlers::rerun_review))
}
