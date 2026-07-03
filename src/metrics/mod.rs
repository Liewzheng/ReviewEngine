//! Prometheus metrics for the review engine.
//!
//! This module exposes a global [`Registry`] and a set of lazily initialized
//! counters, gauges, and histograms used by the server, CLI, and LLM client.
//!
//! # Exported metrics
//!
//! - `REGISTRY`: Global Prometheus registry.
//! - `REVIEW_REQUESTS`: Total number of review requests.
//! - `REVIEW_DURATION`: Duration of review requests in seconds.
//! - `LLM_REQUESTS`: LLM API requests by provider, model, and status.
//!
//! # Usage
//!
//! ```rust
//! use review_engine::metrics::LLM_REQUESTS;
//! LLM_REQUESTS.with_label_values(&["openai", "gpt-4", "ok"]).inc();
//! ```

use once_cell::sync::Lazy;
use prometheus::{Counter, Gauge, Histogram, HistogramOpts, Opts, Registry};

/// Global Prometheus registry.
pub static REGISTRY: Lazy<Registry> = Lazy::new(|| {
    let registry = Registry::new();
    // Register a build-info gauge so `/metrics` always exposes at least one
    // `review_engine_*` series, even before any review traffic is handled.
    if let Ok(build_info) = Gauge::new("review_engine_build_info", "Review Engine build information") {
        build_info.set(1.0);
        registry.register(Box::new(build_info)).ok();
    }
    registry
});

/// Total number of review requests.
#[allow(clippy::expect_used)]
pub static REVIEW_REQUESTS: Lazy<Counter> = Lazy::new(|| {
    let counter = Counter::new("review_requests_total", "Total number of review requests")
        .expect("failed to create review_requests_total");
    REGISTRY.register(Box::new(counter.clone())).ok();
    counter
});

/// Duration of review requests in seconds.
#[allow(clippy::expect_used)]
pub static REVIEW_DURATION: Lazy<Histogram> = Lazy::new(|| {
    let histogram = Histogram::with_opts(HistogramOpts::new(
        "review_duration_seconds",
        "Duration of review requests in seconds",
    ))
    .expect("failed to create review_duration_seconds");
    REGISTRY.register(Box::new(histogram.clone())).ok();
    histogram
});

/// LLM API requests by provider, model, and status.
#[allow(clippy::expect_used)]
pub static LLM_REQUESTS: Lazy<prometheus::CounterVec> = Lazy::new(|| {
    let counter = prometheus::CounterVec::new(
        Opts::new("llm_requests_total", "Total number of LLM API requests"),
        &["provider", "model", "status"],
    )
    .expect("failed to create llm_requests_total");
    REGISTRY.register(Box::new(counter.clone())).ok();
    counter
});
