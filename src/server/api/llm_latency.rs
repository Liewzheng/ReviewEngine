//! Per-provider LLM call latency for the LLM Status page (RENG-57).
//!
//! RENG-36 gives the page an instantaneous value: the round-trip time of the
//! connectivity probe behind `status`. RENG-56 gives it review-level usage
//! counts. Neither is a latency *history*, so the card's "average latency" was
//! the probe's own number (confusing two different measurements — the RENG-53
//! finding) and the sparkline could only have been fabricated, which is why it
//! was removed rather than faked.
//!
//! This module turns the per-call samples recorded on the review path
//! (`llm_call_samples`, written by
//! [`crate::store::llm_samples::StoreLlmCallSink`]) into the two numbers that
//! were missing: an average over a reported window, and a real time series.
//!
//! Invariants:
//!
//! - **Only successful calls enter the average.** A failed attempt's duration
//!   measures the failure (a 401 answered in 5 ms, a timeout at 120 s), not how
//!   long the provider takes to answer; mixing them would make a provider look
//!   faster or slower because of its error mix. Failures are counted separately
//!   (`failureCount`) so the exclusion is visible instead of silent, and they
//!   remain in the table for anyone reading it directly.
//! - **Absent data is `null`, never 0.** No successful sample in the window →
//!   no average, on the wire and on the page (`—`). Only counts that were
//!   really read may be 0, and the whole aggregate is `null` when the store
//!   could not be read at all.
//! - **The window travels with the numbers.** `latencyWindowDays` /
//!   `latencySince` go out in the same payload, exactly as RENG-56 did for
//!   usage, so the client labels what it shows instead of assuming a window.
//! - **The probe stays distinguishable.** The instantaneous probe value is
//!   reported as `lastProbeLatencyMs`, never merged into the average.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;

use crate::server::AppState;
use crate::store::traits::{LlmCallSampleRow, ReviewStore};

/// Length of the latency window in days (rolling, same convention as usage).
///
/// One week: the card already labels its usage numbers with a 7-day window, and
/// a single window per card is easier to read (and to keep honest) than two
/// different ones. A week of samples also gives every provider that is used at
/// all enough calls for the average to mean something.
pub const LATENCY_WINDOW_DAYS: i64 = 7;

/// Points in the card's sparkline: 4 buckets per day, i.e. 6-hour buckets over
/// the 7-day window. Coarse enough that each bucket is a real average of the
/// calls in it rather than a single noisy call, fine enough to show a spike.
pub const SPARKLINE_BUCKETS: usize = 28;

/// Start of the latency window for a request observed at `now` (inclusive,
/// same boundary rule as `llm_usage::window_start`).
pub fn window_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::days(LATENCY_WINDOW_DAYS)
}

/// Width of one sparkline bucket in seconds (exact for the shipped constants:
/// 7 days / 28 buckets = 6 h).
pub fn bucket_seconds() -> i64 {
    (LATENCY_WINDOW_DAYS * 24 * 60 * 60) / SPARKLINE_BUCKETS as i64
}

/// Bucket index of a sample, clamped into `0..SPARKLINE_BUCKETS`.
///
/// Clamping is deliberate: a clock skewed slightly ahead of the server would
/// otherwise index past the end, and dropping such a sample would quietly lose
/// a real measurement.
fn bucket_of(sample_at: DateTime<Utc>, since: DateTime<Utc>) -> usize {
    let offset = sample_at.signed_duration_since(since).num_seconds();
    if offset <= 0 {
        return 0;
    }
    let index = (offset / bucket_seconds()) as usize;
    index.min(SPARKLINE_BUCKETS - 1)
}

/// The latency metrics of one provider, ready for serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLatency {
    /// Mean round-trip time of the SUCCESSFUL calls in the window, in whole
    /// milliseconds; `None` when the window holds no successful call (nothing
    /// to average — never `0`).
    pub avg_latency_ms: Option<u64>,
    /// Successful calls in the window — the average's denominator. This is a
    /// measured count, so `0` is a real value here.
    pub sample_count: u64,
    /// Failed calls in the window, excluded from the average. Counted
    /// separately so "no average" and "no failures" stay distinguishable.
    pub failure_count: u64,
    /// When this provider was last called (successful or not); `None` when the
    /// window holds no sample at all.
    pub last_sample_at: Option<DateTime<Utc>>,
    /// Per-bucket mean latency over the window, oldest bucket first. `None`
    /// when the provider has no sample in the window at all (no series to
    /// draw); individual empty buckets inside a series are `None` gaps.
    pub sparkline: Option<Vec<Option<u64>>>,
}

impl ProviderLatency {
    /// A provider the window never saw: measured zero counts, nothing derived.
    fn untouched() -> Self {
        Self {
            avg_latency_ms: None,
            sample_count: 0,
            failure_count: 0,
            last_sample_at: None,
            sparkline: None,
        }
    }
}

/// Accumulator for one entry while folding the window's rows.
#[derive(Debug, Default, Clone)]
struct Fold {
    success_sum_ms: u64,
    success_count: u64,
    failure_count: u64,
    last_sample_at: Option<DateTime<Utc>>,
    /// Per-bucket (sum, count) of successful calls.
    buckets: Vec<(u64, u64)>,
}

impl Fold {
    /// Add another fold's measurements (the unmarked-bucket merge, RENG-75):
    /// sums and counts, never the rounded averages — the average of two
    /// averages is not the average.
    fn merge_from(&mut self, other: &Fold) {
        self.success_sum_ms += other.success_sum_ms;
        self.success_count += other.success_count;
        self.failure_count += other.failure_count;
        if self
            .last_sample_at
            .is_none_or(|prev| other.last_sample_at.is_some_and(|t| t > prev))
        {
            self.last_sample_at = other.last_sample_at;
        }
        for (dst, src) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            dst.0 += src.0;
            dst.1 += src.1;
        }
    }

    /// Round the fold into the wire shape.
    fn finish(&self) -> ProviderLatency {
        let avg_latency_ms = (self.success_count > 0).then(|| {
            // Whole milliseconds: the page shows "234 ms", and the wire stays
            // clean (`234`, not `234.0`).
            (self.success_sum_ms as f64 / self.success_count as f64).round() as u64
        });
        let has_any = self.success_count + self.failure_count > 0;
        let sparkline = has_any.then(|| {
            self.buckets
                .iter()
                .map(|&(sum, count)| (count > 0).then(|| (sum as f64 / count as f64).round() as u64))
                .collect()
        });
        ProviderLatency {
            avg_latency_ms,
            sample_count: self.success_count,
            failure_count: self.failure_count,
            last_sample_at: self.last_sample_at,
            sparkline,
        }
    }
}

/// Latency aggregated once per `GET /llm/providers` request.
#[derive(Debug, Clone)]
pub struct LatencySnapshot {
    /// Window start the aggregate was computed for.
    pub since: DateTime<Utc>,
    /// Folds keyed by the `(provider, model, entry_fp)` triple the samples
    /// recorded (RENG-75); `entry_fp: None` is the unmarked pre-0005 bucket.
    by_entry: HashMap<(String, String, Option<String>), Fold>,
}

impl LatencySnapshot {
    /// Fold the window's rows into per-entry latency.
    ///
    /// Rows of providers that are no longer configured are folded too (the
    /// samples are facts); the API layer only asks for the providers it
    /// displays.
    pub fn new(since: DateTime<Utc>, rows: impl IntoIterator<Item = LlmCallSampleRow>) -> Self {
        let mut folds: HashMap<(String, String, Option<String>), Fold> = HashMap::new();
        for row in rows {
            let fold = folds
                .entry((row.provider, row.model, row.entry_fp))
                .or_insert_with(|| Fold {
                    buckets: vec![(0, 0); SPARKLINE_BUCKETS],
                    ..Fold::default()
                });
            if fold.last_sample_at.is_none_or(|prev| row.created_at > prev) {
                fold.last_sample_at = Some(row.created_at);
            }
            if !row.success {
                fold.failure_count += 1;
                continue;
            }
            // A negative latency cannot be produced by the recorder (it is an
            // elapsed duration), but the column is read as signed: treat a
            // nonsensical value as 0 rather than wrapping into a huge u64.
            let latency = row.latency_ms.max(0) as u64;
            fold.success_sum_ms += latency;
            fold.success_count += 1;
            let bucket = &mut fold.buckets[bucket_of(row.created_at, since)];
            bucket.0 += latency;
            bucket.1 += 1;
        }

        Self { since, by_entry: folds }
    }

    /// The latency metrics of one configured CARD (RENG-75): its exact
    /// fingerprint fold, plus — when `merge_unmarked` — the unmarked
    /// (pre-fingerprint) fold of the same `(provider, model)`, merged as raw
    /// sums (the API layer passes `merge_unmarked` only for the unique
    /// ENABLED card of that pair).
    ///
    /// A card the window never called yields zero counts and `None` for
    /// everything derived.
    pub fn for_card(&self, provider: &str, model: &str, entry_fp: &str, merge_unmarked: bool) -> ProviderLatency {
        let key = |fp: Option<String>| (provider.to_string(), model.to_string(), fp);
        let mut fold = self
            .by_entry
            .get(&key(Some(entry_fp.to_string())))
            .cloned()
            .unwrap_or_default();
        if merge_unmarked {
            if let Some(unmarked) = self.by_entry.get(&key(None)) {
                fold.merge_from(unmarked);
            }
        }
        if fold.buckets.is_empty() {
            // No exact fold existed and nothing merged in: report the
            // untouched shape (no series), not 28 zero buckets.
            return ProviderLatency::untouched();
        }
        fold.finish()
    }
}

/// Read the call samples of the window ending at `now`, or `None` when nothing
/// can be known.
///
/// `None` means "unknown" and the payload then carries `null` for every
/// latency metric — never a zero. Two cases produce it, the same two as the
/// usage aggregate: no persistent store attached (`REVIEW_DISABLE_DB=1`,
/// embedded use) and a failed query. Both are logged rather than dressed up as
/// "no calls".
pub async fn snapshot(state: &Arc<AppState>, now: DateTime<Utc>) -> Option<LatencySnapshot> {
    let db = state.db.as_ref()?;
    let since = window_start(now);
    match db.llm_samples_since(since).await {
        Ok(rows) => Some(LatencySnapshot::new(since, rows)),
        Err(e) => {
            tracing::warn!("llm latency aggregate unavailable for {since}: {e:#}; reporting null latency");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().with_timezone(&Utc)
    }

    fn row(provider: &str, at: DateTime<Utc>, latency_ms: i64, success: bool) -> LlmCallSampleRow {
        entry_row(
            provider,
            &format!("{provider}-model"),
            Some(&format!("fp-{provider}")),
            at,
            latency_ms,
            success,
        )
    }

    fn entry_row(
        provider: &str,
        model: &str,
        entry_fp: Option<&str>,
        at: DateTime<Utc>,
        latency_ms: i64,
        success: bool,
    ) -> LlmCallSampleRow {
        LlmCallSampleRow {
            provider: provider.to_string(),
            model: model.to_string(),
            entry_fp: entry_fp.map(str::to_string),
            created_at: at,
            latency_ms,
            success,
        }
    }

    #[test]
    fn window_is_seven_rolling_days_in_six_hour_buckets() {
        let now = at("2026-09-15T12:00:00Z");
        assert_eq!(window_start(now).to_rfc3339(), "2026-09-08T12:00:00+00:00");
        assert_eq!(LATENCY_WINDOW_DAYS, 7);
        assert_eq!(bucket_seconds(), 6 * 60 * 60);
        assert_eq!(SPARKLINE_BUCKETS, 28);
    }

    /// The recorded samples aggregate into the expected average, per provider,
    /// with failures counted but excluded.
    #[test]
    fn samples_average_per_provider_and_exclude_failures() {
        let since = at("2026-09-08T12:00:00Z");
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                row("xiaomi", at("2026-09-14T10:00:00Z"), 100, true),
                row("xiaomi", at("2026-09-14T10:00:30Z"), 200, true),
                row("xiaomi", at("2026-09-14T11:00:00Z"), 300, true),
                // A failure at a wild latency must not move the average.
                row("xiaomi", at("2026-09-14T11:05:00Z"), 120_000, false),
                row("deepseek", at("2026-09-14T11:00:00Z"), 50, true),
                // A provider that is no longer configured still aggregates.
                row("retired", at("2026-09-10T11:00:00Z"), 10, true),
            ],
        );

        let xiaomi = snapshot.for_card("xiaomi", "xiaomi-model", "fp-xiaomi", false);
        assert_eq!(xiaomi.avg_latency_ms, Some(200), "mean of 100/200/300");
        assert_eq!(xiaomi.sample_count, 3);
        assert_eq!(xiaomi.failure_count, 1, "the failed call is counted, not averaged");
        assert_eq!(
            xiaomi.last_sample_at.unwrap().to_rfc3339(),
            "2026-09-14T11:05:00+00:00",
            "last sample includes failures"
        );

        let deepseek = snapshot.for_card("deepseek", "deepseek-model", "fp-deepseek", false);
        assert_eq!(deepseek.avg_latency_ms, Some(50));
        assert_eq!(deepseek.sample_count, 1);
        assert_eq!(deepseek.failure_count, 0);
        assert_eq!(
            snapshot
                .for_card("retired", "retired-model", "fp-retired", false)
                .avg_latency_ms,
            Some(10)
        );

        // A configured provider the window never called: measured zeros, no
        // derived number and no series.
        let untouched = snapshot.for_card("brand-new", "m", "fp-new", false);
        assert_eq!(untouched.avg_latency_ms, None, "0/0 is not an average");
        assert_eq!(untouched.sample_count, 0);
        assert_eq!(untouched.failure_count, 0);
        assert_eq!(untouched.last_sample_at, None);
        assert_eq!(untouched.sparkline, None);
    }

    /// An empty window reports zero counts and no derived metric — never a
    /// fabricated `0 ms`.
    #[test]
    fn empty_window_reports_no_average() {
        let snapshot = LatencySnapshot::new(at("2026-09-08T12:00:00Z"), Vec::new());
        let latency = snapshot.for_card("xiaomi", "xiaomi-model", "fp-xiaomi", false);
        assert_eq!(latency.avg_latency_ms, None);
        assert_eq!(latency.sample_count, 0);
        assert_eq!(latency.failure_count, 0);
        assert_eq!(latency.sparkline, None);
    }

    /// A provider with only FAILURES has a failure count and no average: the
    /// two are independent facts.
    #[test]
    fn failures_alone_produce_no_average() {
        let since = at("2026-09-08T12:00:00Z");
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                row("broken", at("2026-09-14T10:00:00Z"), 5, false),
                row("broken", at("2026-09-14T10:01:00Z"), 9, false),
            ],
        );
        let latency = snapshot.for_card("broken", "broken-model", "fp-broken", false);
        assert_eq!(latency.avg_latency_ms, None);
        assert_eq!(latency.sample_count, 0);
        assert_eq!(latency.failure_count, 2);
        assert_eq!(
            latency.last_sample_at.unwrap().to_rfc3339(),
            "2026-09-14T10:01:00+00:00"
        );
        // Failures alone still do not draw a series: there is no latency to
        // plot, and a flat zero line would be a fabrication.
        assert_eq!(latency.sparkline.unwrap().iter().filter(|b| b.is_some()).count(), 0);
    }

    /// The sparkline buckets the window oldest-first, averages each bucket,
    /// leaves empty buckets as gaps and drops failures.
    #[test]
    fn sparkline_buckets_the_window_with_gaps() {
        let since = at("2026-09-08T12:00:00Z");
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                // Bucket 0: [09-08 12:00, 09-08 18:00).
                row("xiaomi", at("2026-09-08T12:00:00Z"), 100, true),
                row("xiaomi", at("2026-09-08T17:59:59Z"), 300, true),
                // Bucket 1 is empty (gap).
                // Bucket 2: [09-09 00:00, 09-09 06:00).
                row("xiaomi", at("2026-09-09T00:00:00Z"), 400, true),
                // The last bucket is the newest 6 h of the window.
                row("xiaomi", at("2026-09-15T11:00:00Z"), 700, true),
                // A failure in bucket 2 must not shift its average.
                row("xiaomi", at("2026-09-09T01:00:00Z"), 60_000, false),
            ],
        );
        let series = snapshot
            .for_card("xiaomi", "xiaomi-model", "fp-xiaomi", false)
            .sparkline
            .unwrap();
        assert_eq!(series.len(), SPARKLINE_BUCKETS, "one point per bucket");
        assert_eq!(series[0], Some(200), "mean of 100 and 300 in bucket 0");
        assert_eq!(series[1], None, "a bucket with no call is a gap");
        assert_eq!(series[2], Some(400), "the failed call stays out of the average");
        assert_eq!(series[SPARKLINE_BUCKETS - 1], Some(700));
        assert_eq!(series.iter().filter(|b| b.is_some()).count(), 3);
    }

    /// Bucket boundaries are inclusive at the start and clamped at both ends,
    /// so a clock ahead of the server cannot index out of range.
    #[test]
    fn bucket_index_is_clamped_to_the_window() {
        let since = at("2026-09-08T12:00:00Z");
        assert_eq!(bucket_of(since, since), 0);
        assert_eq!(bucket_of(since + Duration::seconds(21_599), since), 0);
        assert_eq!(bucket_of(since + Duration::seconds(21_600), since), 1);
        // A sample ahead of "now" (clock skew) lands in the last bucket.
        assert_eq!(
            bucket_of(since + Duration::days(9), since),
            SPARKLINE_BUCKETS - 1,
            "clamped, not dropped"
        );
        assert_eq!(bucket_of(since - Duration::hours(1), since), 0);
    }

    /// Averages are rounded to whole milliseconds on the wire (`234`, not a
    /// long float).
    #[test]
    fn averages_are_whole_milliseconds() {
        let since = at("2026-09-08T12:00:00Z");
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                row("xiaomi", at("2026-09-14T10:00:00Z"), 100, true),
                row("xiaomi", at("2026-09-14T10:01:00Z"), 101, true),
            ],
        );
        let avg = snapshot
            .for_card("xiaomi", "xiaomi-model", "fp-xiaomi", false)
            .avg_latency_ms
            .unwrap();
        assert_eq!(avg, 101, "100.5 rounds to 101");
        let json = serde_json::to_string(&serde_json::json!({ "avgLatencyMs": avg })).unwrap();
        assert_eq!(json, r#"{"avgLatencyMs":101}"#);
    }

    /// RENG-75: two same-named cards (same provider AND model, different
    /// keys → different fingerprints) fold separately — each card's average,
    /// counts and series are its own.
    #[test]
    fn same_name_cards_fold_per_fingerprint() {
        let since = at("2026-09-08T12:00:00Z");
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                entry_row("acme", "m1", Some("fp-a"), at("2026-09-14T10:00:00Z"), 100, true),
                entry_row("acme", "m1", Some("fp-a"), at("2026-09-14T10:05:00Z"), 300, true),
                entry_row("acme", "m1", Some("fp-b"), at("2026-09-14T10:00:00Z"), 900, true),
                entry_row("acme", "m1", Some("fp-b"), at("2026-09-14T10:01:00Z"), 5, false),
            ],
        );

        let a = snapshot.for_card("acme", "m1", "fp-a", false);
        assert_eq!(a.avg_latency_ms, Some(200), "mean of its own 100/300");
        assert_eq!(a.sample_count, 2);
        assert_eq!(a.failure_count, 0, "B's failure is not A's");

        let b = snapshot.for_card("acme", "m1", "fp-b", false);
        assert_eq!(b.avg_latency_ms, Some(900));
        assert_eq!(b.sample_count, 1);
        assert_eq!(b.failure_count, 1);

        // A fingerprint the window never saw: untouched.
        let other = snapshot.for_card("acme", "m1", "fp-c", false);
        assert_eq!(other.avg_latency_ms, None);
        assert_eq!(other.sample_count, 0);
    }

    /// RENG-75 upgrade rule: the unmarked (pre-0005, `entry_fp` NULL) fold
    /// merges as RAW SUMS when the API flags it — the merged average is the
    /// true mean of both folds, not the average of two averages.
    #[test]
    fn unmarked_fold_merges_as_raw_sums_only_when_flagged() {
        let since = at("2026-09-08T12:00:00Z");
        let snapshot = LatencySnapshot::new(
            since,
            vec![
                entry_row("acme", "m1", Some("fp-a"), at("2026-09-14T10:00:00Z"), 100, true),
                entry_row("acme", "m1", None, at("2026-09-13T10:00:00Z"), 300, true),
                entry_row("acme", "m1", None, at("2026-09-13T11:00:00Z"), 500, true),
                entry_row("acme", "m1", None, at("2026-09-13T11:05:00Z"), 7, false),
            ],
        );

        let merged = snapshot.for_card("acme", "m1", "fp-a", true);
        assert_eq!(merged.avg_latency_ms, Some(300), "(100+300+500)/3, not (100+400)/2");
        assert_eq!(merged.sample_count, 3);
        assert_eq!(merged.failure_count, 1, "the unmarked failure comes along");
        assert_eq!(
            merged.last_sample_at.unwrap().to_rfc3339(),
            "2026-09-14T10:00:00+00:00",
            "newest across both folds"
        );
        // The sparkline draws both folds' buckets (the two unmarked successes
        // share one 6-hour bucket).
        let series = merged.sparkline.unwrap();
        assert_eq!(series.iter().filter(|b| b.is_some()).count(), 2);

        let own_only = snapshot.for_card("acme", "m1", "fp-a", false);
        assert_eq!(own_only.avg_latency_ms, Some(100), "unflagged: no merge");
        assert_eq!(own_only.failure_count, 0);

        // A DIFFERENT (provider, model) pair never sees this unmarked fold,
        // even flagged; the unique-enabled-card decision itself lives in the
        // API layer (`may_merge_unmarked`).
        let other = snapshot.for_card("acme", "m2", "fp-b", true);
        assert_eq!(other.avg_latency_ms, None);
        assert_eq!(other.sample_count, 0);
    }
}
