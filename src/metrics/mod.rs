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

#[cfg(test)]
mod tests {
    use super::*;

    /// Find a gathered metric family by name. These are the exported
    /// `prometheus::proto` types returned by `Registry::gather`.
    fn family(name: &str) -> Option<prometheus::proto::MetricFamily> {
        REGISTRY.gather().into_iter().find(|f| f.name() == name)
    }

    /// Total counter value of the first sample of `name` (0 when missing).
    fn counter_total(name: &str) -> f64 {
        let fam = family(name).expect("metric family should exist");
        fam.get_metric().iter().map(|m| m.get_counter().get_value()).sum()
    }

    #[test]
    fn registry_exposes_build_info_gauge() {
        let build_info = family("review_engine_build_info").expect("build info gauge missing");
        assert_eq!(build_info.get_metric()[0].get_gauge().get_value(), 1.0);
    }

    #[test]
    fn review_request_and_duration_counters_are_registered() {
        // Force the lazy statics to initialize and self-register.
        let _ = &*REVIEW_REQUESTS;
        let _ = &*REVIEW_DURATION;
        assert!(family("review_requests_total").is_some());
        assert!(family("review_duration_seconds").is_some());
    }

    #[test]
    fn llm_requests_counter_tracks_provider_model_and_status_labels() {
        // Use a label set unique to this test so the assertion is monotonic
        // even if other tests bump the same global CounterVec concurrently.
        let labels = ["unit-test-provider", "unit-test-model", "ok"];

        let before = LLM_REQUESTS.with_label_values(&labels).get();
        LLM_REQUESTS.with_label_values(&labels).inc();

        let after = LLM_REQUESTS.with_label_values(&labels).get();
        assert!(
            after >= before + 1.0,
            "counter must increase by at least 1: {before} -> {after}"
        );

        // The series must also be visible through the global registry.
        let fam = family("llm_requests_total").expect("llm_requests_total missing");
        let series = fam
            .get_metric()
            .iter()
            .find(|m| {
                m.get_label()
                    .iter()
                    .any(|l| l.name() == "provider" && l.value() == labels[0])
            })
            .expect("series with the test label set must be exported");
        assert!(series.get_counter().get_value() >= 1.0);
    }

    #[test]
    fn request_counter_increments_through_the_global_registry() {
        // `REVIEW_REQUESTS` is a plain global counter; force the lazy static
        // to self-register first, then assert only that it grows monotonically
        // across gather snapshots — never an exact total (the counter is
        // shared by all tests in the binary).
        let _ = &*REVIEW_REQUESTS;
        let before = counter_total("review_requests_total");
        REVIEW_REQUESTS.inc();
        let after = counter_total("review_requests_total");
        assert!(after >= before + 1.0, "{before} -> {after}");
    }
}
