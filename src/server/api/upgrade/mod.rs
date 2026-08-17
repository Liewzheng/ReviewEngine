//! Self-upgrade REST endpoints under `/api/v1/system/upgrade`.
//!
//! - `GET  /api/v1/system/upgrade/check`  — latest version + install hints (1h cache)
//! - `POST /api/v1/system/upgrade`        — start a binary upgrade (single-flight)
//! - `GET  /api/v1/system/upgrade/status` — job state machine

mod check;
mod start;
mod task;
#[cfg(test)]
mod tests;

use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;

use crate::server::AppState;

pub(crate) use self::start::start_upgrade_inner;
pub(crate) use self::task::{run_upgrade_task, resolve_install_dir, UpgradeMode};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/upgrade/check", get(check::check_upgrade))
        .route("/upgrade", post(start::start_upgrade))
        .route("/upgrade/status", get(start::upgrade_status))
}
