use once_cell::sync::Lazy;
use prometheus::{Counter, Histogram, HistogramOpts, Opts, Registry};

/// Global Prometheus registry.
pub static REGISTRY: Lazy<Registry> = Lazy::new(Registry::new);

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
