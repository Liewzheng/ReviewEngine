//! Route handlers for the health-check and webhook server.
//!
//! Exposes Axum handler functions for health probes (`health`,
//! `health_ready`), Prometheus metrics scraping (`metrics`), and
//! review progress tracking (`list_progress`, `get_progress`).

pub mod health;
pub mod metrics;
pub mod progress;
