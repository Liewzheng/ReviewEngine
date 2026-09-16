//! The recording half of RENG-57: the sink the review path attaches to its
//! [`LLMClient`](crate::llm::client::LLMClient), plus the retention rule that
//! keeps `llm_call_samples` bounded.
//!
//! Split from the trait (`crate::store::traits`) on purpose: the trait is the
//! storage contract, this module is the *policy* — how long samples live and
//! what happens when a write fails. The read/aggregate half lives in
//! `src/server/api/llm_latency.rs`.
//!
//! Invariants:
//!
//! - **A write failure is logged and dropped.** Sampling is observability, not
//!   part of a review's result: the sink returns `()` and swallows the error
//!   the same way `reviews.llm_summary`'s write-through does (§5 of
//!   design/persistence.md). The one visible cost is a review that is slower
//!   by one INSERT (sub-millisecond on SQLite, one round trip on PostgreSQL),
//!   awaited inline so a sample is never lost to a process exit.
//! - **A sink belongs to one review.** It carries the review id it was built
//!   for, so a sample can never be attributed to another review; the client
//!   knows nothing about reviews.
//! - **Samples are bounded by retention, not by luck.** Every sample table
//!   would otherwise grow with every call forever, and the read path scans the
//!   window: [`RETENTION_DAYS`] caps the table at ~30 days of calls, so the
//!   page's 7-day scan reads only what the window needs. The sweep runs once
//!   per sink (i.e. once per review, not once per call) and is a no-op delete
//!   — an indexed range lookup that finds nothing — when nothing is old.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use crate::llm::sampling::{LlmCallSample, LlmCallSink};
use crate::store::traits::ReviewStore;

/// How long a call sample is kept.
///
/// 30 days: wide enough to cover the page's rolling window (7 days) several
/// times over — so a week of downtime or a quiet fortnight still has history —
/// while bounding the table. At the observed cost of ~10 calls per review and
/// ~50 reviews a week that is ~2 000 rows; a busy instance making 10 000 calls
/// a week keeps ~43 000 rows, which the `(created_at, provider)` index scans in
/// a few milliseconds. The review rows themselves are not pruned here: this
/// bound is about the sample table, whose only reader is a time-window scan.
pub const RETENTION_DAYS: i64 = 30;

/// Oldest sample `now` lets survive: the retention cutoff for a sweep run at
/// `now` (inclusive boundary, like every other window in this codebase).
pub fn retention_cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::days(RETENTION_DAYS)
}

/// Writes a review's call samples to `llm_call_samples` (RENG-57).
///
/// Built by the review path from the store it already holds
/// ([`crate::server::task_queue::TaskStore::llm_sample_sink`]) and attached to
/// the client of every LLM call that review makes, so all of them —
/// successful, retried, failed, fallback-advanced — land in one table under
/// one review id.
pub struct StoreLlmCallSink {
    store: Arc<dyn ReviewStore>,
    /// Review the samples belong to; `None` for a caller with no review
    /// identity (the row's `review_id` is then NULL, which the read path never
    /// uses for aggregation).
    review_id: Option<String>,
    /// The retention sweep runs on the first record of this sink (once per
    /// review) instead of on every sample.
    pruned: AtomicBool,
}

impl StoreLlmCallSink {
    /// A sink writing to `store`, attributing every sample to `review_id`.
    pub fn new(store: Arc<dyn ReviewStore>, review_id: Option<String>) -> Self {
        Self {
            store,
            review_id,
            pruned: AtomicBool::new(false),
        }
    }

    /// The sink as the client's trait object.
    pub fn shared(store: Arc<dyn ReviewStore>, review_id: Option<String>) -> Arc<dyn LlmCallSink> {
        Arc::new(Self::new(store, review_id))
    }

    /// Run the retention sweep at most once per sink instance.
    async fn sweep_once(&self, now: DateTime<Utc>) {
        if self.pruned.swap(true, Ordering::Relaxed) {
            return;
        }
        match self.store.prune_llm_samples(retention_cutoff(now)).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(
                removed,
                retention_days = RETENTION_DAYS,
                "pruned LLM call samples past retention"
            ),
            Err(e) => tracing::warn!("failed to prune old LLM call samples: {e:#}"),
        }
    }
}

#[async_trait]
impl LlmCallSink for StoreLlmCallSink {
    async fn record(&self, sample: &LlmCallSample) {
        self.sweep_once(sample.at).await;
        // Best-effort: the review path never sees this failure. Logged at WARN
        // because a persistently failing sink silently turns the page's
        // average into `—`, which is exactly the kind of invisible degradation
        // the surrounding code tries to avoid.
        if let Err(e) = self.store.insert_llm_sample(self.review_id.as_deref(), sample).await {
            tracing::warn!(
                provider = %sample.provider,
                model = %sample.model,
                "failed to record LLM call sample (the review continues): {e:#}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqlxStore;

    fn sample(provider: &str, at: DateTime<Utc>, success: bool) -> LlmCallSample {
        LlmCallSample {
            at,
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            entry_fp: format!("fp-{provider}"),
            latency_ms: 250,
            ttfb_ms: None,
            success,
            error: (!success).then(|| "HTTP 500 Internal Server Error".to_string()),
            chain_position: 1,
            attempt: 1,
        }
    }

    async fn store() -> SqlxStore {
        let store = SqlxStore::new_in_memory().await.unwrap();
        store.migrate().await.unwrap();
        store
    }

    /// The sink writes the row it was given, attributed to its review — both
    /// outcomes, so the failures RENG-56 could not see are recorded.
    #[tokio::test]
    async fn sink_records_every_attempt_under_its_review() {
        let store = Arc::new(store().await);
        let sink = StoreLlmCallSink::shared(store.clone(), Some("review-1".to_string()));
        let now = Utc::now();
        sink.record(&sample("xiaomi", now, true)).await;
        sink.record(&sample("deepseek", now, false)).await;

        let sql = "SELECT review_id, provider, success FROM llm_call_samples ORDER BY provider";
        let rows: Vec<(Option<String>, String, i64)> = ::sqlx::query_as(sql).fetch_all(store.pool()).await.unwrap();
        assert_eq!(
            rows,
            vec![
                (Some("review-1".to_string()), "deepseek".to_string(), 0),
                (Some("review-1".to_string()), "xiaomi".to_string(), 1),
            ]
        );
    }

    /// Retention is enforced by the write path: the first sample of a review
    /// sweeps everything older than the cutoff, and the sweep happens once per
    /// sink rather than once per call.
    #[tokio::test]
    async fn sink_prunes_past_retention_once() {
        let store = Arc::new(store().await);
        let now = Utc::now();
        store
            .insert_llm_sample(None, &sample("old", retention_cutoff(now) - Duration::days(1), true))
            .await
            .unwrap();
        store
            .insert_llm_sample(None, &sample("fresh", now, true))
            .await
            .unwrap();

        let sink = StoreLlmCallSink::shared(store.clone(), None);
        sink.record(&sample("xiaomi", now, true)).await;
        let survivors: Vec<String> = ::sqlx::query_as::<_, (String,)>("SELECT provider FROM llm_call_samples")
            .fetch_all(store.pool())
            .await
            .unwrap()
            .into_iter()
            .map(|(p,)| p)
            .collect();
        assert!(!survivors.contains(&"old".to_string()), "got {survivors:?}");
        assert!(survivors.contains(&"fresh".to_string()));

        // A second record on the same sink does not re-sweep (no observable
        // difference in rows — the point is that it stays a no-op delete).
        sink.record(&sample("deepseek", now, true)).await;
        let count: (i64,) = ::sqlx::query_as("SELECT COUNT(*) FROM llm_call_samples")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 3);
    }

    /// A failing store must not surface to the caller: the sink's contract is
    /// that a review continues (the trait returns `()` and the failure is
    /// logged). `SqlxStore` against a closed pool gives a real write error.
    #[tokio::test]
    async fn a_write_failure_is_swallowed() {
        let store = Arc::new(store().await);
        store.pool().close().await;
        let sink = StoreLlmCallSink::shared(store, Some("review-1".to_string()));
        sink.record(&sample("xiaomi", Utc::now(), true)).await;
    }
}
