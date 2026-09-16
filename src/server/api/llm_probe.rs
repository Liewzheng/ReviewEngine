//! The communication latency of a provider, measured by the connectivity probe
//! (RENG-78).
//!
//! RENG-57/RENG-77 put a "latency" on the provider cards, but neither number
//! was the one the user asked for. `avgLatencyMs` is the mean of the LLM calls
//! a review made — it contains model generation (17 s-scale, measured).
//! `avgTtfbMs` was meant to isolate communication, and does for a provider that
//! flushes response headers early, but the shipped request shape is
//! non-streaming: the server generates the whole body before the response
//! leaves, so `ttfb ≈ latency` and neither is a network metric. What the user
//! means by "communication latency" is the round trip of the lightweight probe
//! itself — one `GET {api_base}/models`, DNS + TCP + TLS + HTTP, no model
//! involved anywhere.
//!
//! Before this module, a probe left exactly one trace: the LATEST value, in the
//! health cache (`lastProbeLatencyMs`, RENG-36). No history, no average, and
//! nothing at all unless somebody opened a page (probing is lazy, 60 s TTL).
//! This module is the other half: every probe the health store runs writes one
//! sample to `llm_probe_samples`, and the card's communication latency is the
//! mean of the successful ones over the window.
//!
//! Invariants:
//!
//! - **Only successful probes enter the average.** A failed probe's duration
//!   describes the failure (a 401 answered in 5 ms, a timeout at 120 s), not
//!   how fast the endpoint answers; the sample's `latency_ms` is therefore NULL
//!   on failure (0007) and the fold skips those rows. Same rule as
//!   [`crate::server::api::llm_latency`].
//! - **Sampling never changes a probe's result.** The write is best-effort: it
//!   is awaited (so a sample is not lost to a process exit) but a failure is
//!   logged at WARN and dropped ([`StoreProbeSampleSink`]). A broken statistics
//!   table must not turn a provider that just answered into one reported as
//!   broken.
//! - **The sample belongs to the CARD, not to the name.** Rows carry the
//!   four-tuple fingerprint (RENG-75) and the aggregate matches on it, so two
//!   same-named cards never share a latency — their `api_base`s are different
//!   network paths.
//! - **Samples are bounded by retention.** The proactive loop writes one sample
//!   per provider per round and a page view adds one per TTL, so the table
//!   would grow without bound; [`retention_cutoff`] caps it at
//!   [`PROBE_RETENTION_DAYS`], with the same once-per-process sweep the call
//!   samples use.
//! - **Probing is server-mode work.** [`start_proactive_probing`] refuses to run
//!   in [`ProcessMode::Cli`]: a one-shot command (`reng review`, `reng config
//!   …`) exits when its work is done, so a 30-minute loop would never fire
//!   once and would only keep an idle runtime alive.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use super::llm_health::{LlmHealthStore, ProviderStatus};
use super::llm_latency::LATENCY_WINDOW_DAYS;
use crate::models::LLMConfig;
use crate::server::AppState;
use crate::store::traits::{ProbeSample, ReviewStore};

/// Length of the probe-latency window in days.
///
/// Deliberately the SAME rolling week the call-latency and usage aggregates
/// report ([`LATENCY_WINDOW_DAYS`]): the card shows three numbers side by side,
/// and three different windows would be a trap for whoever reads them. The
/// window travels to the client as `latencyWindowDays` — this average shares it
/// with the call-latency fields instead of inventing a second one.
pub const PROBE_WINDOW_DAYS: i64 = LATENCY_WINDOW_DAYS;

/// How long a probe sample is kept.
///
/// 30 days, the same as the call samples
/// ([`crate::store::llm_samples::RETENTION_DAYS`]): wide enough to cover the
/// page's 7-day window several times over, narrow enough to bound a table whose
/// rows are produced by a 30-minute loop plus one per page-view probe. Two
/// providers probed every 30 minutes make ~2 900 rows a month; the `(created_at,
/// provider)` index scans that in milliseconds.
pub const PROBE_RETENTION_DAYS: i64 = 30;

/// Start of the probe window for a request observed at `now` (inclusive, the
/// same boundary rule as every other window in this codebase).
pub fn window_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now - ChronoDuration::days(PROBE_WINDOW_DAYS)
}

/// Oldest probe sample `now` lets survive: the retention cutoff for a sweep run
/// at `now` (inclusive, like every other window here).
pub fn retention_cutoff(now: DateTime<Utc>) -> DateTime<Utc> {
    now - ChronoDuration::days(PROBE_RETENTION_DAYS)
}

/// How a probe result reaches `llm_probe_samples`.
///
/// A function rather than a `ReviewStore` handle, so the health store knows
/// nothing about the storage layer and its unit tests can count samples without
/// a database — the same seam shape as [`super::llm_health::ProbeFn`].
/// Production wires [`store_sink`]; a test wires a collector.
pub type ProbeSampleSink = Arc<dyn Fn(ProbeSample) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// The production sink: every sample appended to `llm_probe_samples`, with the
/// retention sweep run once per process.
pub struct StoreProbeSampleSink {
    store: Arc<dyn ReviewStore>,
    /// The sweep runs on the first sample this process records rather than on
    /// every one: a DELETE per probe would put maintenance on the poll path,
    /// while the retention bound only needs to hold eventually.
    swept: AtomicBool,
}

impl StoreProbeSampleSink {
    /// A sink writing to `store`.
    pub fn new(store: Arc<dyn ReviewStore>) -> Self {
        Self {
            store,
            swept: AtomicBool::new(false),
        }
    }

    /// Run the retention sweep at most once per instance.
    async fn sweep_once(&self, now: DateTime<Utc>) {
        if self.swept.swap(true, Ordering::Relaxed) {
            return;
        }
        match self.store.prune_probe_samples(retention_cutoff(now)).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(
                removed,
                retention_days = PROBE_RETENTION_DAYS,
                "pruned LLM probe samples past retention"
            ),
            Err(e) => tracing::warn!("failed to prune old LLM probe samples: {e:#}"),
        }
    }

    /// Record one sample. Best-effort by contract: the probe's result is
    /// already computed and is returned to the caller whatever happens here.
    async fn record(&self, sample: &ProbeSample) {
        self.sweep_once(sample.at).await;
        if let Err(e) = self.store.insert_probe_sample(sample).await {
            tracing::warn!(
                provider = %sample.provider,
                "failed to record LLM probe sample (the probe result stands): {e:#}"
            );
        }
    }
}

/// The production sample sink over `store`.
pub fn store_sink(store: Arc<dyn ReviewStore>) -> ProbeSampleSink {
    let sink = Arc::new(StoreProbeSampleSink::new(store));
    Arc::new(move |sample| {
        let sink = sink.clone();
        Box::pin(async move { sink.record(&sample).await })
    })
}

/// The communication latency of one card, ready for serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderProbe {
    /// Mean round-trip time of the SUCCESSFUL probes in the window, in whole
    /// milliseconds; `None` when the window holds no successful probe — `null`
    /// on the wire, never a fabricated `0`.
    pub avg_latency_ms: Option<u64>,
    /// Successful probes in the window — the average's denominator. A measured
    /// count, so `0` is a real value here.
    pub sample_count: u64,
}

impl ProviderProbe {
    /// A card the window holds no successful probe for.
    fn untouched() -> Self {
        Self {
            avg_latency_ms: None,
            sample_count: 0,
        }
    }
}

/// Probe latency aggregated once per `GET /llm/providers` request.
#[derive(Debug, Clone, Default)]
pub struct ProbeSnapshot {
    /// Folds keyed by the `(provider, entry_fp)` pair the samples recorded.
    by_entry: HashMap<(String, String), (u64, u64)>,
}

impl ProbeSnapshot {
    /// Fold the window's rows into per-card probe latency.
    ///
    /// Rows of providers that are no longer configured are folded too (the
    /// samples are facts); the API layer only asks for the cards it displays.
    pub fn new(rows: impl IntoIterator<Item = ProbeSample>) -> Self {
        let mut by_entry: HashMap<(String, String), (u64, u64)> = HashMap::new();
        for row in rows {
            if !row.success {
                continue;
            }
            // A successful probe with no duration cannot happen (the recorder
            // measures it), but the column is nullable: treat "no measurement"
            // as "not in the average" rather than as a zero-second round trip.
            let Some(latency) = row.latency_ms.filter(|v| *v >= 0) else {
                continue;
            };
            let fold = by_entry.entry((row.provider, row.entry_fp)).or_insert((0, 0));
            fold.0 += latency as u64;
            fold.1 += 1;
        }
        Self { by_entry }
    }

    /// The probe latency of one configured CARD (RENG-75 identity): its exact
    /// fingerprint fold. A card the window never probed successfully yields an
    /// untouched reading (`None`, count 0).
    pub fn for_card(&self, provider: &str, entry_fp: &str) -> ProviderProbe {
        match self.by_entry.get(&(provider.to_string(), entry_fp.to_string())) {
            Some(&(sum, count)) if count > 0 => ProviderProbe {
                // Whole milliseconds: the page shows "234 ms", and the wire
                // stays clean (`234`, not `234.0`).
                avg_latency_ms: Some((sum as f64 / count as f64).round() as u64),
                sample_count: count,
            },
            _ => ProviderProbe::untouched(),
        }
    }
}

/// Read the probe samples of the window ending at `now`, or `None` when nothing
/// can be known.
///
/// `None` means "unknown" and the payload then carries `null` for the probe
/// fields — never a zero. The two cases are the same two as the call-latency
/// aggregate: no persistent store attached (`REVIEW_DISABLE_DB=1`, embedded
/// use) and a failed query. Both are logged rather than dressed up as "this
/// provider was never probed".
pub async fn snapshot(state: &Arc<AppState>, now: DateTime<Utc>) -> Option<ProbeSnapshot> {
    let db = state.db.as_ref()?;
    let since = window_start(now);
    match db.probe_samples_since(since).await {
        Ok(rows) => Some(ProbeSnapshot::new(rows)),
        Err(e) => {
            tracing::warn!("llm probe aggregate unavailable for {since}: {e:#}; reporting null probe latency");
            None
        }
    }
}

// ─── Proactive probing (server mode only) ─────────────────────────────

/// How often the server probes every enabled provider on its own initiative.
///
/// 30 minutes, as specified: often enough that the card's average keeps moving
/// without a page ever being opened (48 samples per provider per day), and
/// nowhere near often enough to matter to a provider's rate limit — the probe
/// is one `GET /models`.
pub const PROACTIVE_PROBE_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// Which kind of process is running, as far as proactive probing is concerned.
///
/// The distinction is not cosmetic: `serve` holds an HTTP listener for as long
/// as the operator keeps it up, while every other command is one-shot. Making
/// it an explicit parameter (rather than assuming "called from serve" means
/// "server") is also what lets a test assert the CLI case without binding a
/// port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMode {
    /// A long-lived server (`reng serve`, the Docker image).
    Server,
    /// A one-shot command that exits when its work is done.
    Cli,
}

/// What the loop probes each round: the configs in effect right now.
///
/// A function, not a snapshot: cards are added, edited and removed while the
/// server runs, and the next round must use the current list (that is also why
/// the loop reads the state instead of holding a config vector).
pub type ProbeTargets = Arc<dyn Fn() -> Vec<LLMConfig> + Send + Sync>;

/// The running proactive-probe loop.
///
/// Dropping it signals the loop to stop; [`ProbeTask::shutdown`] additionally
/// waits for it, so a caller that returns from `serve` knows no probe is still
/// in flight.
pub struct ProbeTask {
    shutdown: tokio::sync::watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl ProbeTask {
    /// Signal the loop and wait for its last round to finish.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for ProbeTask {
    fn drop(&mut self) {
        // Any early return out of the server (a failed TLS bind, say) drops the
        // task; without this the loop would keep probing for a server that is
        // no longer there.
        let _ = self.shutdown.send(true);
    }
}

/// Start the proactive probe loop, or `None` when this process must not run one.
///
/// Called by [`crate::server::serve`] with [`ProcessMode::Server`] — the only
/// caller, and the only server-mode entry point in the binary. In
/// [`ProcessMode::Cli`] nothing is spawned and nothing is scheduled: the
/// command owns the process until it exits.
pub fn start_proactive_probing(state: &Arc<AppState>, mode: ProcessMode, interval: Duration) -> Option<ProbeTask> {
    if mode != ProcessMode::Server {
        return None;
    }
    let store = Arc::clone(&state.llm_health);
    let targets: ProbeTargets = {
        let state = Arc::clone(state);
        // Disabled cards are already absent from this list (RENG-75), and a
        // keyless one is skipped by the store; between them those are the
        // providers a probe must not touch.
        Arc::new(move || state.ordered_llm_configs())
    };
    Some(spawn_loop(store, targets, interval))
}

/// Spawn the loop: probe every target once per `interval` until the returned
/// task is shut down (or dropped).
///
/// The first round runs immediately — a restart should not leave the card's
/// average stale for half an hour — and a round that overruns its interval does
/// not queue up the missed ticks (`MissedTickBehavior::Delay`).
pub fn spawn_loop(store: Arc<LlmHealthStore>, targets: ProbeTargets, interval: Duration) -> ProbeTask {
    let (shutdown, mut stop) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut failures = FailureLog::default();
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let configs = targets();
                    if configs.is_empty() {
                        continue;
                    }
                    let reports = store.probe_all(&configs).await;
                    log_round(&mut failures, &configs, &reports);
                }
                _ = stop.changed() => break,
            }
        }
    });
    ProbeTask {
        shutdown,
        handle: Some(handle),
    }
}

/// Report a round's outcomes, one line per provider — and only on a TRANSITION.
///
/// A provider whose endpoint is down fails every round; one WARN per provider
/// per round, forever, is how a log file becomes unreadable. The samples are
/// still written every round ([`StoreProbeSampleSink`], driven by the probe
/// itself) — it is the LOG that stays quiet, not the history.
fn log_round(failures: &mut FailureLog, configs: &[LLMConfig], reports: &[super::llm_health::ProviderHealth]) {
    for (cfg, health) in configs.iter().zip(reports) {
        let event = match health.status {
            ProviderStatus::Healthy => failures.observe(&cfg.entry_fp(), true),
            ProviderStatus::Error => failures.observe(&cfg.entry_fp(), false),
            // No probe happened (no key stored, or the card is disabled):
            // there is no round outcome to report, and recording it as a
            // failure would warn about a provider that was never contacted.
            ProviderStatus::Offline | ProviderStatus::Disabled => continue,
        };
        match event {
            RoundEvent::FirstFailure => tracing::warn!(
                provider = %cfg.provider,
                error = %health.message,
                "LLM connectivity probe is failing (the samples keep recording; \
                 further rounds stay quiet until it recovers)"
            ),
            RoundEvent::Recovered => tracing::info!(
                provider = %cfg.provider,
                latency_ms = health.latency_ms,
                "LLM connectivity probe recovered"
            ),
            RoundEvent::StillFailing => tracing::debug!(
                provider = %cfg.provider,
                "LLM connectivity probe still failing"
            ),
            RoundEvent::Healthy => {}
        }
    }
}

/// What one round's outcome means for the log, given the rounds before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundEvent {
    /// The probe succeeded and has been succeeding: nothing to say.
    Healthy,
    /// The probe failed after succeeding (or for the first time): the one line
    /// a persistent outage produces.
    FirstFailure,
    /// The probe failed again, as it did last round: sampled, not logged.
    StillFailing,
    /// The probe succeeded after failing: worth a line, it closes the outage.
    Recovered,
}

/// Which cards are currently failing, so a persistent failure is reported once
/// per outage instead of once per round.
///
/// Keyed by the card fingerprint (RENG-75): two cards sharing a provider name
/// have separate endpoints and therefore separate outages.
#[derive(Debug, Default)]
pub struct FailureLog {
    failing: HashSet<String>,
}

impl FailureLog {
    /// Fold one round's outcome for one card in, returning what to log.
    pub fn observe(&mut self, entry_fp: &str, success: bool) -> RoundEvent {
        match (self.failing.contains(entry_fp), success) {
            (false, true) => RoundEvent::Healthy,
            (false, false) => {
                self.failing.insert(entry_fp.to_string());
                RoundEvent::FirstFailure
            }
            (true, true) => {
                self.failing.remove(entry_fp);
                RoundEvent::Recovered
            }
            (true, false) => RoundEvent::StillFailing,
        }
    }

    /// How many cards are in a failing state right now (for tests/logging).
    pub fn failing_count(&self) -> usize {
        self.failing.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::probe::ProbeOutcome;
    use crate::server::api::llm_health::{LlmHealthStore, ProbeFn};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn cfg(provider: &str, key: &str, base: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            api_key: key.to_string(),
            api_base: base.to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }
    }

    /// A probe answering `ok` (with the given key) and counting calls.
    fn probe_fn(calls: Arc<AtomicUsize>, ok: bool) -> ProbeFn {
        Arc::new(move |_cfg: LLMConfig| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if ok {
                    Ok(ProbeOutcome {
                        resolved_base: "stub".to_string(),
                    })
                } else {
                    Err("HTTP 401 Unauthorized".to_string())
                }
            })
        })
    }

    /// A store whose every probe records through `sink`.
    fn sampling_store(probe: ProbeFn, sink: ProbeSampleSink) -> Arc<LlmHealthStore> {
        let store = LlmHealthStore::with_probe(probe, Duration::from_secs(60));
        store.attach_sample_sink(Some(sink));
        Arc::new(store)
    }

    /// A health store whose probes write to a real database.
    fn storage_of(store: LlmHealthStore, db: Arc<crate::store::SqlxStore>) -> Arc<LlmHealthStore> {
        store.attach_sample_sink(Some(store_sink(db as Arc<dyn ReviewStore>)));
        Arc::new(store)
    }

    /// A sink collecting the samples a probe wrote, in order.
    fn collecting_sink() -> (Arc<Mutex<Vec<ProbeSample>>>, ProbeSampleSink) {
        let seen: Arc<Mutex<Vec<ProbeSample>>> = Arc::new(Mutex::new(Vec::new()));
        let sink: ProbeSampleSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |sample: ProbeSample| {
                let seen = Arc::clone(&seen);
                Box::pin(async move {
                    seen.lock().unwrap().push(sample);
                })
            })
        };
        (seen, sink)
    }

    async fn migrated_store() -> Arc<crate::store::SqlxStore> {
        let store = Arc::new(crate::store::SqlxStore::new_in_memory().await.unwrap());
        store.migrate().await.unwrap();
        store
    }

    fn sample(provider: &str, fp: &str, latency_ms: Option<i64>, success: bool) -> ProbeSample {
        ProbeSample {
            provider: provider.to_string(),
            entry_fp: fp.to_string(),
            at: Utc::now(),
            latency_ms,
            success,
            error: (!success).then(|| "HTTP 401 Unauthorized".to_string()),
        }
    }

    /// The window is the latency window: one number on the card cannot be a
    /// week of calls and another a different week of probes.
    #[test]
    fn the_probe_window_is_the_latency_window() {
        let now = DateTime::parse_from_rfc3339("2026-09-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(PROBE_WINDOW_DAYS, LATENCY_WINDOW_DAYS);
        assert_eq!(window_start(now), super::super::llm_latency::window_start(now));
        assert_eq!(window_start(now).to_rfc3339(), "2026-09-08T12:00:00+00:00");
        assert_eq!(PROBE_RETENTION_DAYS, 30);
        assert_eq!(retention_cutoff(now).to_rfc3339(), "2026-08-16T12:00:00+00:00");
    }

    /// The average is over the SUCCESSFUL samples only, rounded to whole
    /// milliseconds — the failures are recorded, and stay out of the number.
    #[test]
    fn the_average_uses_successful_samples_only() {
        let snapshot = ProbeSnapshot::new(vec![
            sample("xiaomi", "fp-a", Some(100), true),
            sample("xiaomi", "fp-a", Some(201), true),
            // A failure with (and without) a duration: neither enters.
            sample("xiaomi", "fp-a", None, false),
            sample("xiaomi", "fp-a", Some(90_000), false),
        ]);
        let probe = snapshot.for_card("xiaomi", "fp-a");
        assert_eq!(probe.avg_latency_ms, Some(151), "(100 + 201) / 2, failures excluded");
        assert_eq!(probe.sample_count, 2, "the average's denominator counts successes only");
    }

    /// Two same-named cards keep their own average: the fold is the card
    /// fingerprint, not the provider name (RENG-75).
    #[test]
    fn same_named_cards_fold_per_fingerprint() {
        let snapshot = ProbeSnapshot::new(vec![
            sample("acme", "fp-a", Some(20), true),
            sample("acme", "fp-b", Some(900), true),
            sample("acme", "fp-b", Some(1000), true),
        ]);
        assert_eq!(snapshot.for_card("acme", "fp-a").avg_latency_ms, Some(20));
        assert_eq!(snapshot.for_card("acme", "fp-b").avg_latency_ms, Some(950));
        assert_eq!(snapshot.for_card("acme", "fp-b").sample_count, 2);
        // A card the window never probed successfully: nothing measured.
        assert_eq!(snapshot.for_card("acme", "fp-c"), ProviderProbe::untouched());
        assert_eq!(snapshot.for_card("acme", "fp-c").avg_latency_ms, None);
        assert_eq!(snapshot.for_card("acme", "fp-c").sample_count, 0);
    }

    /// Nothing but failures (or nothing at all) is `null`, not `0 ms`: a
    /// provider that never answered must not read as an instant one.
    #[test]
    fn no_successful_sample_is_null_and_not_zero() {
        let only_failures = ProbeSnapshot::new(vec![
            sample("broken", "fp-x", None, false),
            sample("broken", "fp-x", None, false),
        ]);
        assert_eq!(only_failures.for_card("broken", "fp-x").avg_latency_ms, None);
        assert_eq!(only_failures.for_card("broken", "fp-x").sample_count, 0);

        let empty = ProbeSnapshot::new(Vec::new());
        assert_eq!(empty.for_card("xiaomi", "fp-a"), ProviderProbe::untouched());
    }

    /// The window boundary is enforced by the query the aggregate reads
    /// through: a probe one second before the window start is not in it.
    #[tokio::test]
    async fn the_aggregate_respects_the_window_boundary() {
        let store = migrated_store().await;
        let now = Utc::now();
        let since = window_start(now);
        for (at, latency) in [
            (since - ChronoDuration::seconds(1), 10),
            (since + ChronoDuration::seconds(1), 200),
            (since + ChronoDuration::hours(1), 300),
        ] {
            store
                .insert_probe_sample(&ProbeSample {
                    at,
                    latency_ms: Some(latency),
                    success: true,
                    ..sample("xiaomi", "fp-a", None, true)
                })
                .await
                .unwrap();
        }

        let rows = store.probe_samples_since(since).await.unwrap();
        assert_eq!(rows.len(), 2, "the sample before the window is not read");
        let snapshot = ProbeSnapshot::new(rows);
        assert_eq!(
            snapshot.for_card("xiaomi", "fp-a").avg_latency_ms,
            Some(250),
            "mean of the two in-window probes only"
        );
        assert_eq!(snapshot.for_card("xiaomi", "fp-a").sample_count, 2);
    }

    /// A probe writes a sample — success and failure both — through the sink
    /// the health store holds, and the write is what reaches the table.
    #[tokio::test]
    async fn every_probe_writes_one_sample_to_the_store() {
        let store = migrated_store().await;
        let calls = Arc::new(AtomicUsize::new(0));
        // The probe answers by key: `sk-good` succeeds, anything else 401s.
        let counted = Arc::clone(&calls);
        let probe: ProbeFn = Arc::new(move |cfg: LLMConfig| {
            counted.fetch_add(1, Ordering::SeqCst);
            let ok = cfg.api_key == "sk-good";
            Box::pin(async move {
                if ok {
                    Ok(ProbeOutcome {
                        resolved_base: "stub".to_string(),
                    })
                } else {
                    Err("HTTP 401 Unauthorized".to_string())
                }
            })
        });
        let draft = LlmHealthStore::with_probe(probe, Duration::from_secs(60));
        let health = storage_of(draft, Arc::clone(&store));

        let good = cfg("openai", "sk-good", "https://api.openai.com/v1");
        let bad = cfg("openai", "sk-bad", "https://api.openai.com/v1");
        health.probe_all(std::slice::from_ref(&good)).await;
        health.probe_all(std::slice::from_ref(&bad)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one probe per card");

        let rows = store.probe_samples_since(window_start(Utc::now())).await.unwrap();
        assert_eq!(rows.len(), 2, "every probe leaves one sample");
        let success = rows
            .iter()
            .find(|r| r.success)
            .expect("the successful probe is recorded");
        let failure = rows
            .iter()
            .find(|r| !r.success)
            .expect("the failed probe is recorded too");
        assert_eq!(success.provider, "openai");
        assert_eq!(
            success.entry_fp,
            good.entry_fp(),
            "attributed to the card that was probed"
        );
        assert!(
            success.latency_ms.is_some(),
            "a successful probe measures the round trip"
        );
        assert_eq!(success.error, None);
        assert_eq!(failure.entry_fp, bad.entry_fp());
        assert_eq!(
            failure.latency_ms, None,
            "a failed probe records no duration (0007): the failure is not a link measurement"
        );
        assert_eq!(failure.error.as_deref(), Some("HTTP 401 Unauthorized"));

        // A card that was never probed (no key) records nothing at all.
        let keyless = cfg("ollama", "", "http://localhost:11434");
        health.probe_all(std::slice::from_ref(&keyless)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "no probe, no sample");
        assert_eq!(
            store.probe_samples_since(window_start(Utc::now())).await.unwrap().len(),
            2
        );
    }

    /// A disabled card is not probed and therefore records nothing — its
    /// history is untouched while it is switched off (RENG-75).
    #[tokio::test]
    async fn a_disabled_card_is_not_probed_and_records_no_sample() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (seen, sink) = collecting_sink();
        let store = sampling_store(probe_fn(Arc::clone(&calls), true), sink);

        let mut off = cfg("deepseek", "sk-b", "https://api.deepseek.com/v1");
        off.disabled = true;
        let reports = store.probe_all(&[off]).await;
        assert_eq!(reports[0].status, ProviderStatus::Disabled);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "a disabled card is never probed");
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The health store records one sample per probe, and does NOT record the
    /// report it serves from the cache — a page poll must not manufacture
    /// samples out of a cached value.
    #[tokio::test]
    async fn a_cached_report_writes_no_sample() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (seen, sink) = collecting_sink();
        let store = sampling_store(probe_fn(Arc::clone(&calls), true), sink);
        let configs = vec![cfg("xiaomi", "sk-a", "https://api.xiaomi.example/v1")];

        store.report(&configs).await;
        assert_eq!(seen.lock().unwrap().len(), 1, "the probe that ran wrote one sample");
        store.report(&configs).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "still one probe");
        assert_eq!(seen.lock().unwrap().len(), 1, "a cached report writes nothing");
    }

    /// The sink's contract: a failing store is logged and swallowed — the probe
    /// result the caller receives is unaffected.
    #[tokio::test]
    async fn a_sample_write_failure_does_not_change_the_probe_result() {
        let store = migrated_store().await;
        store.pool().close().await;
        let draft = LlmHealthStore::with_probe(probe_fn(Arc::new(AtomicUsize::new(0)), true), Duration::from_secs(60));
        let health = storage_of(draft, Arc::clone(&store));

        let reports = health
            .probe_all(&[cfg("xiaomi", "sk-a", "https://api.xiaomi.example/v1")])
            .await;
        assert_eq!(
            reports[0].status,
            ProviderStatus::Healthy,
            "a statistics failure must not mark a provider that answered as broken"
        );
        assert_eq!(reports[0].message, "Configured");
    }

    /// A persistent failure is reported once per outage, not once per round;
    /// a recovery is reported once.
    #[test]
    fn consecutive_failures_are_logged_once_per_outage() {
        let mut log = FailureLog::default();
        assert_eq!(
            log.observe("fp-a", true),
            RoundEvent::Healthy,
            "healthy rounds are silent"
        );
        assert_eq!(log.observe("fp-a", false), RoundEvent::FirstFailure);
        for _ in 0..5 {
            assert_eq!(
                log.observe("fp-a", false),
                RoundEvent::StillFailing,
                "a failing provider must not warn every round"
            );
        }
        assert_eq!(log.observe("fp-a", true), RoundEvent::Recovered);
        assert_eq!(log.observe("fp-a", true), RoundEvent::Healthy, "and goes quiet again");

        // The memory is per card: one card's outage never silences another's.
        assert_eq!(log.observe("fp-b", false), RoundEvent::FirstFailure);
        assert_eq!(log.failing_count(), 1);
        assert_eq!(
            log.observe("fp-a", false),
            RoundEvent::FirstFailure,
            "a new outage is reported"
        );
    }

    /// Only the transitions are log-worthy: the loop maps Healthy and
    /// StillFailing to no WARN at all, which is what bounds a persistent
    /// outage to one line per provider.
    #[test]
    fn only_transitions_are_log_worthy() {
        let mut log = FailureLog::default();
        let rounds: Vec<RoundEvent> = (0..10).map(|_| log.observe("fp-a", false)).collect();
        assert_eq!(rounds.iter().filter(|e| **e == RoundEvent::FirstFailure).count(), 1);
        assert_eq!(rounds[0], RoundEvent::FirstFailure);
        assert!(rounds[1..].iter().all(|e| *e == RoundEvent::StillFailing));
    }

    /// The proactive loop probes every target every interval, and stops when
    /// the server's shutdown signal arrives.
    #[tokio::test]
    async fn the_loop_probes_on_its_interval_and_stops_on_shutdown() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            probe_fn(Arc::clone(&calls), true),
            Duration::from_secs(60),
        ));
        let targets: ProbeTargets = Arc::new(|| vec![cfg("xiaomi", "sk-a", "https://api.xiaomi.example/v1")]);

        let task = spawn_loop(store, targets, Duration::from_millis(20));
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            calls.load(Ordering::SeqCst) >= 3,
            "the loop probes repeatedly: {} probes",
            calls.load(Ordering::SeqCst)
        );

        task.shutdown().await;
        let after_shutdown = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_shutdown,
            "no probe outlives the shutdown signal"
        );
    }

    /// Dropping the task stops the loop too: an early return out of `serve`
    /// (a failed TLS bind, say) must not leave a probe loop behind.
    #[tokio::test]
    async fn dropping_the_task_stops_the_loop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            probe_fn(Arc::clone(&calls), true),
            Duration::from_secs(60),
        ));
        let targets: ProbeTargets = Arc::new(|| vec![cfg("xiaomi", "sk-a", "https://api.xiaomi.example/v1")]);

        let task = spawn_loop(store, targets, Duration::from_millis(20));
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(task);
        let after_drop = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(calls.load(Ordering::SeqCst), after_drop);
    }

    /// Server mode starts the loop; CLI mode starts nothing. Asserted without a
    /// live server: the loop's observable effect is the probes it makes.
    #[tokio::test]
    async fn server_mode_starts_the_loop_and_cli_mode_does_not() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut state = crate::server::AppState::new(vec![cfg("xiaomi", "sk-a", "https://api.xiaomi.example/v1")]);
        state.llm_health = Arc::new(LlmHealthStore::with_probe(
            probe_fn(Arc::clone(&calls), true),
            Duration::from_secs(60),
        ));
        let state = Arc::new(state);

        assert!(
            start_proactive_probing(&state, ProcessMode::Cli, Duration::from_millis(10)).is_none(),
            "a one-shot CLI command must not start a background probe loop"
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0, "CLI mode probes nothing at all");

        let task = start_proactive_probing(&state, ProcessMode::Server, Duration::from_millis(10))
            .expect("server mode starts the loop");
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "server mode probes on its interval: {} probes",
            calls.load(Ordering::SeqCst)
        );
        task.shutdown().await;
    }

    /// The loop reads the CURRENT config set every round: a card removed
    /// between rounds is not probed again.
    #[tokio::test]
    async fn the_loop_re_reads_its_targets_every_round() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            probe_fn(Arc::clone(&calls), true),
            Duration::from_secs(60),
        ));
        let live = Arc::new(Mutex::new(vec![cfg("xiaomi", "sk-a", "https://api.xiaomi.example/v1")]));
        let targets: ProbeTargets = {
            let live = Arc::clone(&live);
            Arc::new(move || live.lock().unwrap().clone())
        };

        let task = spawn_loop(store, targets, Duration::from_millis(20));
        // A round probes the provider once; the count's absolute value is
        // whatever the timing produced, so the assertion is on the flat line
        // after the provider is gone.
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(calls.load(Ordering::SeqCst) >= 1, "the one provider is probed");

        live.lock().unwrap().clear();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let after_removal = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after_removal,
            "a removed provider is not probed by the next round"
        );
        task.shutdown().await;
    }
}
