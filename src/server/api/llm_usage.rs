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
    by_provider: HashMap<String, ProviderUsageStats>,
}

impl UsageSnapshot {
    /// Fold the store's per-provider rows into a lookup with the window total
    /// resolved (the denominator of every share).
    pub fn new(since: DateTime<Utc>, stats: Vec<ProviderUsageStats>) -> Self {
        let total_usage = stats.iter().map(|s| s.usage_count).sum();
        let by_provider = stats.into_iter().map(|s| (s.provider.clone(), s)).collect();
        Self {
            since,
            total_usage,
            by_provider,
        }
    }

    /// The metrics of one configured provider. A provider the window never
    /// saw yields zeroed counts and `None` for everything derived from them.
    pub fn for_provider(&self, provider: &str) -> ProviderUsage {
        let stats = self.by_provider.get(provider);
        ProviderUsage {
            request_count: Some(stats.map_or(0, |s| s.usage_count)),
            usage_share: (self.total_usage > 0).then(|| {
                let count = stats.map_or(0, |s| s.usage_count);
                round4(count as f64 / self.total_usage as f64)
            }),
            success_rate: stats.and_then(|s| {
                let decided = s.completed_count + s.failed_count;
                (decided > 0).then(|| round4(s.completed_count as f64 / decided as f64))
            }),
            last_used_at: stats.and_then(|s| s.last_used_at),
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
        ProviderUsageStats {
            provider: provider.to_string(),
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

        let xiaomi = snapshot.for_provider("xiaomi-demo");
        assert_eq!(xiaomi.request_count, Some(30));
        assert_eq!(xiaomi.usage_share, Some(0.3));
        assert_eq!(xiaomi.success_rate, Some(1.0));
        assert_eq!(xiaomi.last_used_at.unwrap().to_rfc3339(), "2026-09-14T10:00:00+00:00");

        let deepseek = snapshot.for_provider("deepseek-demo");
        assert_eq!(deepseek.usage_share, Some(0.1));
        assert_eq!(deepseek.success_rate, Some(0.9));

        // A configured provider the window never saw: measured zero usage,
        // but nothing to derive a share/rate/last-use from.
        let untouched = snapshot.for_provider("brand-new");
        assert_eq!(untouched.request_count, Some(0));
        assert_eq!(untouched.usage_share, Some(0.0));
        assert_eq!(untouched.success_rate, None, "no terminal outcome to divide by");
        assert_eq!(untouched.last_used_at, None);
    }

    /// No usage at all in the window (empty DB): counts are a measured zero,
    /// everything derived is `None` — never a fabricated 0 %/100 %.
    #[test]
    fn empty_window_reports_zero_counts_and_no_derived_metrics() {
        let snapshot = UsageSnapshot::new(Utc::now(), Vec::new());
        assert_eq!(snapshot.total_usage, 0);
        let usage = snapshot.for_provider("xiaomi-demo");
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
        assert_eq!(snapshot.for_provider("xiaomi").success_rate, None);

        let snapshot = UsageSnapshot::new(Utc::now(), vec![stats("xiaomi", 4, 3, 1, Some("2026-09-14T10:00:00Z"))]);
        assert_eq!(snapshot.for_provider("xiaomi").success_rate, Some(0.75));
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
        let share = snapshot.for_provider("a").usage_share.unwrap();
        assert_eq!(share, 0.3333);
        assert_eq!(serde_json::to_string(&share).unwrap(), "0.3333");
    }
}
