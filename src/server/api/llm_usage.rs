//! Per-provider LLM usage for the LLM Status page (RENG-56).
//!
//! Before this module `GET /api/v1/llm/providers` filled `errorRate`,
//! `requestCount`, `usagePercent` and `sparkline` with hardcoded zeros:
//! no code path ever wrote them, so the page showed "0 requests / 0 %" for a
//! provider that had served hundreds of reviews. What actually exists is the
//! usage snapshot RENG-38 records on every review (`reviews.llm_summary`, the
//! deduplicated `{provider, model}` pairs that produced its reports) plus the
//! review's `state` and `created_at`. This module turns that into the page's
//! numbers.
//!
//! Invariants:
//!
//! - **Every number comes from recorded rows.** The window is the only input;
//!   there is no default, no estimate and no placeholder value.
//! - **Absent data is `null`, never 0.** A metric whose denominator does not
//!   exist (no usage in the window → no share; no terminal outcome → no
//!   success rate; no store attached → nothing known) is `null` on the wire
//!   and `—` in the UI. Only a count that was really measured can be 0.
//! - **The window is reported with the numbers.** `usageWindowDays` /
//!   `usageSince` travel in the same payload, so the UI labels what it shows
//!   instead of the client assuming a window.
//!
//! Latency is deliberately absent: it is the live probe's round-trip time
//! (RENG-36), not a recorded average — call-level history is RENG-57.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::Arc;

use crate::server::AppState;
use crate::store::traits::{ProviderUsageStats, ReviewStore};

/// Length of the usage window in days (rolling: `now - USAGE_WINDOW_DAYS`).
///
/// One week matches how the page is read — "what has this provider done for
/// me lately" — and keeps the aggregate cheap: it is served from the
/// `idx_reviews_created_at` index, so the scan is proportional to the reviews
/// of the last week, not the whole history.
pub const USAGE_WINDOW_DAYS: i64 = 7;

/// Start of the usage window for a request observed at `now` (inclusive:
/// `reviews.created_at >= window_start(now)`).
pub fn window_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::days(USAGE_WINDOW_DAYS)
}

/// The usage metrics of one provider, ready for serialization.
///
/// Every field is `Option`: `None` means "not known", and the JSON carries
/// `null` for it — the UI renders `—`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsage {
    /// Reviews that recorded this provider inside the window (0 when the
    /// window holds no usage of it — a measured zero, not a placeholder).
    pub request_count: Option<u64>,
    /// Share of ALL usages recorded in the window (all providers, including
    /// ones no longer configured), as a fraction 0..=1. `None` when the
    /// window holds no usage at all (0/0 has no meaning).
    pub usage_share: Option<f64>,
    /// `completed / (completed + failed)` over the reviews that used this
    /// provider. `None` when no such review reached a terminal state.
    ///
    /// NOTE the limit this metric inherits from the recording path:
    /// `llm_summary` is written when a review completes its write-through, so
    /// a review that failed before producing any report carries no snapshot
    /// and is invisible here. The rate therefore answers "of the reviews that
    /// used this provider and finished, how many ended completed" — it is an
    /// upper bound on the provider's own call success, not a call-level rate
    /// (that needs the per-call history of RENG-57).
    pub success_rate: Option<f64>,
    /// Newest recorded use of this provider; `None` when it was not used.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Usage aggregated once per `GET /llm/providers` request.
#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    /// Window start the aggregate was computed for.
    pub since: DateTime<Utc>,
    /// All usages recorded in the window, across every provider name.
    pub total_usage: u64,
    /// Buckets keyed by the `(provider, model, fp)` triple the reviews
    /// recorded (RENG-75); `fp: None` is the unmarked pre-fingerprint bucket.
    by_entry: HashMap<(String, String, Option<String>), ProviderUsageStats>,
}

impl UsageSnapshot {
    /// Fold the store's per-entry rows into a lookup with the window total
    /// resolved (the denominator of every share).
    pub fn new(since: DateTime<Utc>, stats: Vec<ProviderUsageStats>) -> Self {
        let total_usage = stats.iter().map(|s| s.usage_count).sum();
        let by_entry = stats
            .into_iter()
            .map(|s| ((s.provider.clone(), s.model.clone(), s.fp.clone()), s))
            .collect();
        Self {
            since,
            total_usage,
            by_entry,
        }
    }

    /// The metrics of one configured CARD (RENG-75): its exact fingerprint
    /// bucket, plus — when `merge_unmarked` — the unmarked (pre-fingerprint)
    /// bucket of the same `(provider, model)`. The API layer passes
    /// `merge_unmarked` only for the unique ENABLED card of that pair, so old
    /// data lands exactly where it can be attributed and nowhere else.
    ///
    /// A card the window never saw yields zeroed counts and `None` for
    /// everything derived from them.
    pub fn for_card(&self, provider: &str, model: &str, fp: &str, merge_unmarked: bool) -> ProviderUsage {
        let key = |fp: Option<String>| (provider.to_string(), model.to_string(), fp);
        let exact = self.by_entry.get(&key(Some(fp.to_string())));
        let unmarked = merge_unmarked.then(|| self.by_entry.get(&key(None))).flatten();

        let usage_count = exact.map_or(0, |s| s.usage_count) + unmarked.map_or(0, |s| s.usage_count);
        let completed = exact.map_or(0, |s| s.completed_count) + unmarked.map_or(0, |s| s.completed_count);
        let failed = exact.map_or(0, |s| s.failed_count) + unmarked.map_or(0, |s| s.failed_count);
        let last_used_at = [exact, unmarked]
            .into_iter()
            .flatten()
            .filter_map(|s| s.last_used_at)
            .max();

        ProviderUsage {
            request_count: Some(usage_count),
            usage_share: (self.total_usage > 0).then(|| round4(usage_count as f64 / self.total_usage as f64)),
            success_rate: {
                let decided = completed + failed;
                (decided > 0).then(|| round4(completed as f64 / decided as f64))
            },
            last_used_at,
        }
    }
}

/// Read the usage of the window ending at `now`, or `None` when no usage can
/// be known.
///
/// `None` means "unknown", and the payload then carries `null` for the usage
/// metrics — never a zero. Two cases produce it: no persistent store is
/// attached (`REVIEW_DISABLE_DB=1`, embedded use) and the aggregate query
/// failed. Both are logged/observable rather than dressed up as "no usage":
/// reporting 0 there would claim a measurement that never happened.
pub async fn snapshot(state: &Arc<AppState>, now: DateTime<Utc>) -> Option<UsageSnapshot> {
    let db = state.db.as_ref()?;
    let since = window_start(now);
    match db.llm_usage_since(since).await {
        Ok(stats) => Some(UsageSnapshot::new(since, stats)),
        Err(e) => {
            tracing::warn!("llm usage aggregate unavailable for {since}: {e:#}; reporting null usage");
            None
        }
    }
}

/// Round a 0..=1 fraction to 4 decimals so the JSON carries `0.2516` instead
/// of binary-float noise (`0.25160000000000004`).
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::traits::ProviderUsageStats;

    fn stats(provider: &str, usage: u64, completed: u64, failed: u64, last: Option<&str>) -> ProviderUsageStats {
        entry_stats(
            provider,
            &format!("{provider}-model"),
            Some(&format!("fp-{provider}")),
            usage,
            completed,
            failed,
            last,
        )
    }

    fn entry_stats(
        provider: &str,
        model: &str,
        fp: Option<&str>,
        usage: u64,
        completed: u64,
        failed: u64,
        last: Option<&str>,
    ) -> ProviderUsageStats {
        ProviderUsageStats {
            provider: provider.to_string(),
            model: model.to_string(),
            fp: fp.map(str::to_string),
            usage_count: usage,
            completed_count: completed,
            failed_count: failed,
            last_used_at: last.map(|s| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)),
        }
    }

    #[test]
    fn window_start_is_seven_rolling_days() {
        let now = DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            window_start(now).to_rfc3339(),
            "2026-09-08T12:00:00+00:00",
            "a rolling window, not a calendar-day boundary"
        );
        assert_eq!(USAGE_WINDOW_DAYS, 7);
    }

    /// Shares are computed against EVERY usage of the window, so they add up
    /// to 100 % even when part of the window came from providers that are no
    /// longer configured.
    #[test]
    fn usage_share_is_over_the_whole_window() {
        let snapshot = UsageSnapshot::new(
            Utc::now(),
            vec![
                stats("xiaomi-demo", 30, 30, 0, Some("2026-09-14T10:00:00Z")),
                stats("deepseek-demo", 10, 9, 1, Some("2026-09-13T10:00:00Z")),
                // A provider that is NOT in the config any more still counts.
                stats("retired", 60, 60, 0, Some("2026-09-12T10:00:00Z")),
            ],
        );
        assert_eq!(snapshot.total_usage, 100);

        let xiaomi = snapshot.for_card("xiaomi-demo", "xiaomi-demo-model", "fp-xiaomi-demo", false);
        assert_eq!(xiaomi.request_count, Some(30));
        assert_eq!(xiaomi.usage_share, Some(0.3));
        assert_eq!(xiaomi.success_rate, Some(1.0));
        assert_eq!(xiaomi.last_used_at.unwrap().to_rfc3339(), "2026-09-14T10:00:00+00:00");

        let deepseek = snapshot.for_card("deepseek-demo", "deepseek-demo-model", "fp-deepseek-demo", false);
        assert_eq!(deepseek.usage_share, Some(0.1));
        assert_eq!(deepseek.success_rate, Some(0.9));

        // A configured provider the window never saw: measured zero usage,
        // but nothing to derive a share/rate/last-use from.
        let untouched = snapshot.for_card("brand-new", "m", "fp-n", false);
        assert_eq!(untouched.request_count, Some(0));
        assert_eq!(untouched.usage_share, Some(0.0));
        assert_eq!(untouched.success_rate, None, "no terminal outcome to divide by");
        assert_eq!(untouched.last_used_at, None);
    }

    /// RENG-75: two cards sharing `(provider, model)` but fingerprinted
    /// differently are two buckets — each card reports only its own numbers.
    #[test]
    fn same_name_cards_fold_per_fingerprint() {
        let snapshot = UsageSnapshot::new(
            Utc::now(),
            vec![
                entry_stats("acme", "m1", Some("fp-a"), 3, 3, 0, Some("2026-09-14T10:00:00Z")),
                entry_stats("acme", "m1", Some("fp-b"), 1, 0, 1, Some("2026-09-13T10:00:00Z")),
            ],
        );
        assert_eq!(snapshot.total_usage, 4);

        let a = snapshot.for_card("acme", "m1", "fp-a", false);
        assert_eq!(a.request_count, Some(3));
        assert_eq!(a.success_rate, Some(1.0));
        assert_eq!(a.usage_share, Some(0.75));
        let b = snapshot.for_card("acme", "m1", "fp-b", false);
        assert_eq!(b.request_count, Some(1));
        assert_eq!(b.success_rate, Some(0.0), "its own failure, not shared with A");
        assert_eq!(b.usage_share, Some(0.25));
    }

    /// RENG-75 upgrade rule at the aggregate level: the unmarked bucket folds
    /// into a card only when the API layer flags the merge; otherwise the
    /// card reports just its own fingerprinted usage and the unmarked rows
    /// stay invisible (but counted in `total_usage`).
    #[test]
    fn unmarked_bucket_merges_only_when_flagged() {
        let snapshot = UsageSnapshot::new(
            Utc::now(),
            vec![
                entry_stats("acme", "m1", Some("fp-a"), 2, 2, 0, Some("2026-09-14T10:00:00Z")),
                entry_stats("acme", "m1", None, 5, 4, 1, Some("2026-09-13T10:00:00Z")),
            ],
        );
        assert_eq!(
            snapshot.total_usage, 7,
            "the unmarked rows still count in the window total"
        );

        let merged = snapshot.for_card("acme", "m1", "fp-a", true);
        assert_eq!(merged.request_count, Some(7), "2 own + 5 unmarked");
        assert_eq!(merged.success_rate, Some(0.8571), "6 completed of 7 decided");
        assert_eq!(merged.usage_share, Some(1.0));
        assert_eq!(
            merged.last_used_at.unwrap().to_rfc3339(),
            "2026-09-14T10:00:00+00:00",
            "newest across both buckets"
        );

        let own_only = snapshot.for_card("acme", "m1", "fp-a", false);
        assert_eq!(own_only.request_count, Some(2), "unflagged: no merge");
        assert_eq!(own_only.usage_share, Some(0.2857));

        // A card of a DIFFERENT (provider, model) never sees this pair's
        // unmarked bucket, even flagged; and an unflagged same-pair card gets
        // only its own fingerprint bucket. The unique-enabled-card decision
        // itself lives in the API layer (`may_merge_unmarked`).
        let other_pair = snapshot.for_card("acme", "m2", "fp-b", true);
        assert_eq!(other_pair.request_count, Some(0));
        let same_pair_unflagged = snapshot.for_card("acme", "m1", "fp-b", false);
        assert_eq!(same_pair_unflagged.request_count, Some(0));
    }

    /// No usage at all in the window (empty DB): counts are a measured zero,
    /// everything derived is `None` — never a fabricated 0 %/100 %.
    #[test]
    fn empty_window_reports_zero_counts_and_no_derived_metrics() {
        let snapshot = UsageSnapshot::new(Utc::now(), Vec::new());
        assert_eq!(snapshot.total_usage, 0);
        let usage = snapshot.for_card("xiaomi-demo", "m", "fp", false);
        assert_eq!(usage.request_count, Some(0));
        assert_eq!(usage.usage_share, None, "0/0 has no share");
        assert_eq!(usage.success_rate, None);
        assert_eq!(usage.last_used_at, None);
    }

    /// A provider used only by reviews that have not finished has no success
    /// rate yet — pending/running/cancelled are not outcomes.
    #[test]
    fn success_rate_needs_a_terminal_outcome() {
        let snapshot = UsageSnapshot::new(Utc::now(), vec![stats("xiaomi", 3, 0, 0, Some("2026-09-14T10:00:00Z"))]);
        assert_eq!(
            snapshot
                .for_card("xiaomi", "xiaomi-model", "fp-xiaomi", false)
                .success_rate,
            None
        );

        let snapshot = UsageSnapshot::new(Utc::now(), vec![stats("xiaomi", 4, 3, 1, Some("2026-09-14T10:00:00Z"))]);
        assert_eq!(
            snapshot
                .for_card("xiaomi", "xiaomi-model", "fp-xiaomi", false)
                .success_rate,
            Some(0.75)
        );
    }

    /// Fractions are rounded for the wire (no `0.30000000000000004`).
    #[test]
    fn shares_are_rounded_to_four_decimals() {
        let snapshot = UsageSnapshot::new(
            Utc::now(),
            vec![
                stats("a", 1, 1, 0, None),
                stats("b", 1, 1, 0, None),
                stats("c", 1, 1, 0, None),
            ],
        );
        let share = snapshot.for_card("a", "a-model", "fp-a", false).usage_share.unwrap();
        assert_eq!(share, 0.3333);
        assert_eq!(serde_json::to_string(&share).unwrap(), "0.3333");
    }
}
