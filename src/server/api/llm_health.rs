//! Cached per-provider LLM connectivity health (RENG-36).
//!
//! Everything that reports provider health reads from this ONE store:
//! `GET /api/v1/llm/providers` (the LLM Status page) and the dashboard's
//! `health.llmProviders` section. Before this module both derived
//! `healthy` from "the `api_key` field is non-empty" — no probe, so breaking a
//! key in the Web UI left the page reporting `healthy` while reviews failed
//! with 401 (the RENG-34 E2E finding this closes).
//!
//! Invariants:
//!
//! - **A report is always a real probe.** `healthy` is only ever produced by a
//!   successful `GET {api_base}/models`; a failed probe is `error` carrying the
//!   transport/HTTP failure; a provider with no `api_key` is `offline` and is
//!   never probed. There is no "assume healthy" fallback.
//! - **An entry belongs to the exact config it was probed with.** The cache key
//!   is a SHA-256 fingerprint over `(provider, model, api_base, api_key)`, so a
//!   credential or endpoint change cannot be answered by the previous config's
//!   result even if an invalidation call were missed.
//! - **Changed providers are dropped explicitly.** [`LlmHealthStore::retain_live`]
//!   drops every entry whose config is no longer in the effective provider set
//!   (edited credentials, removed provider, cleared key); the next read then
//!   re-probes it. Granularity is per provider: an edit to one provider never
//!   invalidates another's status, so untouched cards keep their badge and only
//!   the changed provider is probed again.
//! - **Probing is cheap.** Results are served for [`DEFAULT_TTL`]; a stale entry
//!   is refreshed in the background so the poll path never waits; a burst of
//!   concurrent readers collapses into one request per provider via the
//!   per-fingerprint flight gate.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::models::LLMConfig;

/// How long a probe result is served before the next reader refreshes it.
///
/// 60 s bounds provider traffic to one `GET /models` per provider per minute
/// however many pages/tabs poll (the LLM page polls every 30 s, the dashboard
/// on its own cadence) while keeping the status no more than a minute behind
/// reality.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

/// Health of one configured provider, as of its most recent probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHealth {
    pub status: ProviderStatus,
    /// Human-readable detail for the UI: `Configured`, `Missing API key`, or
    /// the probe's failure (`HTTP 401 Unauthorized`, `error sending request…`).
    pub message: String,
    /// Round-trip time of the probe itself (0 when no probe was made).
    pub latency_ms: u64,
    /// When the probe behind this report ran.
    pub checked_at: DateTime<Utc>,
}

/// The wire vocabulary of a health report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatus {
    /// The last probe reached the provider and it accepted the stored key.
    Healthy,
    /// The last probe failed (rejected credentials, transport error, …).
    Error,
    /// No `api_key` is stored, so no probe was made.
    Offline,
}

impl ProviderStatus {
    /// `GET /api/v1/llm/providers` vocabulary.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Error => "error",
            Self::Offline => "offline",
        }
    }

    /// Dashboard `health.llmProviders` vocabulary (RENG-32: `success` /
    /// `error` / `offline`).
    pub fn dashboard_str(self) -> &'static str {
        match self {
            Self::Healthy => "success",
            Self::Error => "error",
            Self::Offline => "offline",
        }
    }
}

impl ProviderHealth {
    /// A provider with no usable key: reported, never probed.
    pub fn offline() -> Self {
        Self {
            status: ProviderStatus::Offline,
            message: "Missing API key".to_string(),
            latency_ms: 0,
            checked_at: Utc::now(),
        }
    }

    /// A probe that reached the provider and was accepted.
    pub fn healthy(latency_ms: u64) -> Self {
        Self {
            status: ProviderStatus::Healthy,
            message: "Configured".to_string(),
            latency_ms,
            checked_at: Utc::now(),
        }
    }

    /// A probe that failed, carrying why.
    pub fn error(message: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            status: ProviderStatus::Error,
            message: message.into(),
            latency_ms,
            checked_at: Utc::now(),
        }
    }
}

/// Resolves one provider config to a probe outcome (the error text on failure).
///
/// A seam rather than a direct call so tests can drive the store without
/// network access; production always uses [`real_probe`].
pub type ProbeFn = Arc<
    dyn Fn(LLMConfig) -> Pin<Box<dyn Future<Output = Result<crate::llm::probe::ProbeOutcome, String>> + Send>>
        + Send
        + Sync,
>;

/// The production probe: the same `GET {api_base}/models` the LLM page's Test
/// Connection button and the CLI's `reng config provider test` run.
pub fn real_probe() -> ProbeFn {
    Arc::new(|cfg| {
        Box::pin(async move {
            crate::llm::probe::probe_llm_connectivity(&cfg)
                .await
                .map_err(|e| e.to_string())
        })
    })
}

/// Per-provider health cache, shared through [`crate::server::AppState`].
pub struct LlmHealthStore {
    /// How long an entry is served before it is refreshed.
    ttl: chrono::Duration,
    probe: ProbeFn,
    /// Last probe result per config fingerprint; never held across an `await`.
    entries: RwLock<HashMap<String, ProviderHealth>>,
    /// Single-flight gates, one per fingerprint: readers that arrive while a
    /// probe is in flight wait for it instead of starting a second request
    /// (thundering-herd guard). `Weak` so finished flights do not accumulate.
    flights: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl LlmHealthStore {
    /// A store with the real probe and the default TTL.
    pub fn new() -> Self {
        Self::with_probe(real_probe(), DEFAULT_TTL)
    }

    /// Test/deployment seam: probe with `probe`, serve results for `ttl`.
    pub fn with_probe(probe: ProbeFn, ttl: Duration) -> Self {
        Self {
            ttl: chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(60)),
            probe,
            entries: RwLock::new(HashMap::new()),
            flights: Mutex::new(HashMap::new()),
        }
    }

    /// Report the health of every config in `cfgs`, in the same order.
    ///
    /// A config with no entry in the cache is probed before returning, so the
    /// caller never sees a status that predates the config it holds. A config
    /// whose entry is merely stale (same fingerprint, older than the TTL) is
    /// answered from the cache while a refresh runs in the background — the
    /// value is still a real probe of THIS config, just up to one TTL old.
    pub async fn report(self: &Arc<Self>, cfgs: &[LLMConfig]) -> Vec<ProviderHealth> {
        let mut out: Vec<Option<ProviderHealth>> = vec![None; cfgs.len()];
        let mut cold: Vec<(usize, String, LLMConfig)> = Vec::new();

        for (i, cfg) in cfgs.iter().enumerate() {
            if cfg.api_key.is_empty() {
                out[i] = Some(ProviderHealth::offline());
                continue;
            }
            let key = Self::fingerprint(cfg);
            match self.cached(&key) {
                Some(entry) => {
                    if !self.is_fresh(&entry) {
                        self.spawn_refresh(key, cfg.clone());
                    }
                    out[i] = Some(entry);
                }
                None => cold.push((i, key, cfg.clone())),
            }
        }

        if !cold.is_empty() {
            // Concurrently, one request per provider — but only for the
            // providers the cache could not answer.
            let probes = cold.iter().map(|(_, key, cfg)| self.probe(key, cfg));
            for ((i, _, _), health) in cold.iter().zip(futures::future::join_all(probes).await) {
                out[*i] = Some(health);
            }
        }

        // Every index is filled above (cached, offline, or — for the configs the
        // cache could not answer — by the probes just awaited), so nothing is
        // dropped here; the assertion is a guard against a future edit leaving
        // a hole.
        debug_assert!(out.iter().all(Option::is_some), "every provider resolves to a report");
        out.into_iter().flatten().collect()
    }

    /// Drop every cached entry whose provider config is not in `cfgs`.
    ///
    /// Called by the config-change paths (`PUT /api/v1/config` and the
    /// provider CRUD endpoints): the configs that survive keep their status,
    /// the edited/removed ones lose it and are re-probed on the next read.
    /// Returns how many entries were dropped (for logging/tests).
    pub fn retain_live(&self, cfgs: &[LLMConfig]) -> usize {
        let live: HashSet<String> = cfgs
            .iter()
            .filter(|c| !c.api_key.is_empty())
            .map(Self::fingerprint)
            .collect();
        let mut entries = self.entries.write().unwrap();
        let before = entries.len();
        entries.retain(|key, _| live.contains(key));
        before - entries.len()
    }

    /// Record a probe that already ran elsewhere (the per-provider Test
    /// Connection endpoint), so the health read agrees with what the user just
    /// saw instead of re-probing.
    pub fn record(&self, cfg: &LLMConfig, health: ProviderHealth) {
        if cfg.api_key.is_empty() {
            return;
        }
        self.entries.write().unwrap().insert(Self::fingerprint(cfg), health);
    }

    /// Probe `cfg` unless a concurrent reader already resolved it while we
    /// waited on the flight gate.
    async fn probe(&self, key: &str, cfg: &LLMConfig) -> ProviderHealth {
        let flight = self.flight(key);
        let _guard = flight.lock().await;
        if let Some(entry) = self.cached(key) {
            if self.is_fresh(&entry) {
                return entry;
            }
        }
        let started = Instant::now();
        let health = match (self.probe)(cfg.clone()).await {
            Ok(_) => ProviderHealth::healthy(started.elapsed().as_millis() as u64),
            Err(e) => ProviderHealth::error(e, started.elapsed().as_millis() as u64),
        };
        self.entries.write().unwrap().insert(key.to_string(), health.clone());
        health
    }

    /// Refresh a stale entry off the request path.
    ///
    /// Skipped when a probe for the same config is already in flight: that one
    /// writes a fresh entry, and a second task would spend its time waiting on
    /// the same flight gate. The poll path therefore issues at most one request
    /// per provider per TTL, however often the pages tick.
    fn spawn_refresh(self: &Arc<Self>, key: String, cfg: LLMConfig) {
        if self.flight(&key).try_lock().is_err() {
            return;
        }
        let store = Arc::clone(self);
        tokio::spawn(async move {
            store.probe(&key, &cfg).await;
        });
    }

    fn cached(&self, key: &str) -> Option<ProviderHealth> {
        self.entries.read().unwrap().get(key).cloned()
    }

    fn is_fresh(&self, entry: &ProviderHealth) -> bool {
        Utc::now().signed_duration_since(entry.checked_at) < self.ttl
    }

    /// The flight gate for `key`, creating it when absent.
    fn flight(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut flights = self.flights.lock().unwrap();
        if let Some(existing) = flights.get(key).and_then(Weak::upgrade) {
            return existing;
        }
        flights.retain(|_, weak| weak.strong_count() > 0);
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        flights.insert(key.to_string(), Arc::downgrade(&lock));
        lock
    }

    /// SHA-256 over every field that can change what a probe does. The key is
    /// hashed rather than stored so the cache never holds a second copy of a
    /// secret.
    fn fingerprint(cfg: &LLMConfig) -> String {
        let mut hasher = Sha256::new();
        for field in [&cfg.provider, &cfg.model, &cfg.api_base, &cfg.api_key] {
            hasher.update(field.as_bytes());
            hasher.update([0x1f]);
        }
        hex::encode(hasher.finalize())
    }
}

impl Default for LlmHealthStore {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cfg(provider: &str, key: &str, base: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            api_key: key.to_string(),
            api_base: base.to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }
    }

    /// A probe that answers `ok` unless the key is `bad-*`, counting calls so
    /// tests can assert how often the network would be touched.
    fn counting_probe(calls: Arc<AtomicUsize>, ok: bool) -> ProbeFn {
        Arc::new(move |_cfg: LLMConfig| {
            calls.fetch_add(1, Ordering::SeqCst);
            let ok = ok;
            Box::pin(async move {
                if ok {
                    Ok(crate::llm::probe::ProbeOutcome {
                        resolved_base: "stub".to_string(),
                    })
                } else {
                    Err("HTTP 401 Unauthorized".to_string())
                }
            })
        })
    }

    /// The status vocabulary each endpoint consumes.
    #[test]
    fn status_vocabularies_are_stable() {
        assert_eq!(ProviderStatus::Healthy.as_str(), "healthy");
        assert_eq!(ProviderStatus::Error.as_str(), "error");
        assert_eq!(ProviderStatus::Offline.as_str(), "offline");
        // Dashboard rows (RENG-32) keep their own `success` spelling.
        assert_eq!(ProviderStatus::Healthy.dashboard_str(), "success");
        assert_eq!(ProviderStatus::Error.dashboard_str(), "error");
        assert_eq!(ProviderStatus::Offline.dashboard_str(), "offline");
    }

    /// The fingerprint is stable for identical configs and changes with every
    /// field that can change what a probe does — including the key, which is
    /// why the cache can never answer a changed credential with a leftover.
    #[test]
    fn fingerprint_tracks_every_probe_relevant_field() {
        let base = cfg("openai", "sk-a", "https://api.openai.com/v1");
        assert_eq!(
            LlmHealthStore::fingerprint(&base),
            LlmHealthStore::fingerprint(&base.clone()),
            "same config → same key"
        );
        assert_eq!(LlmHealthStore::fingerprint(&base).len(), 64, "hex SHA-256");
        for changed in [
            cfg("anthropic", "sk-a", "https://api.openai.com/v1"),
            cfg("openai", "sk-b", "https://api.openai.com/v1"),
            cfg("openai", "sk-a", "https://proxy.example/v1"),
            LLMConfig {
                model: "gpt-4o-mini".to_string(),
                ..base.clone()
            },
        ] {
            assert_ne!(
                LlmHealthStore::fingerprint(&base),
                LlmHealthStore::fingerprint(&changed),
                "a changed config must not reuse the previous entry: {changed:?}"
            );
        }
        // The key itself never appears in the cache key.
        assert!(!LlmHealthStore::fingerprint(&base).contains("sk-a"));
    }

    /// A keyless provider is reported `offline` and never probed.
    #[tokio::test]
    async fn keyless_provider_is_offline_and_not_probed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            counting_probe(calls.clone(), true),
            DEFAULT_TTL,
        ));
        let reports = store.report(&[cfg("ollama", "", "http://localhost:11434")]).await;
        assert_eq!(reports[0].status, ProviderStatus::Offline);
        assert_eq!(reports[0].message, "Missing API key");
        assert_eq!(reports[0].latency_ms, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no probe for an unconfigured provider");
    }

    /// A fresh entry is served from the cache — the poll path does not re-probe
    /// on every request; an entry past its TTL is refreshed in the background,
    /// so the reader still gets an immediate answer.
    #[tokio::test]
    async fn fresh_entries_are_served_and_stale_ones_refresh_in_background() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            counting_probe(calls.clone(), true),
            Duration::from_millis(40),
        ));
        let configs = vec![cfg("openai", "sk-a", "https://api.openai.com/v1")];

        assert_eq!(store.report(&configs).await[0].status, ProviderStatus::Healthy);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Still fresh → cached, no second probe.
        assert_eq!(store.report(&configs).await[0].status, ProviderStatus::Healthy);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a fresh entry must not re-probe");

        // Past the TTL the read is answered at once from the (still probed)
        // entry while a refresh runs off the request path.
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(store.report(&configs).await[0].status, ProviderStatus::Healthy);
        for _ in 0..50 {
            if calls.load(Ordering::SeqCst) > 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a stale entry is refreshed once");
    }

    /// Concurrent readers collapse into one probe per provider: the flight gate
    /// is the thundering-herd guard on a page polled by several tabs.
    #[tokio::test]
    async fn concurrent_readers_share_one_probe_per_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            counting_probe(calls.clone(), true),
            DEFAULT_TTL,
        ));
        let configs = vec![
            cfg("openai", "sk-a", "https://api.openai.com/v1"),
            cfg("deepseek", "sk-b", "https://api.deepseek.com/v1"),
        ];

        let (a, b) = tokio::join!(store.report(&configs), store.report(&configs));
        assert!(a.iter().all(|h| h.status == ProviderStatus::Healthy));
        assert!(b.iter().all(|h| h.status == ProviderStatus::Healthy));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one probe per provider, however many readers"
        );
    }

    /// The reported defect at the store level: a probe is healthy, the key
    /// changes, and the next report reflects the NEW key — a failed probe here,
    /// never the previous `healthy`.
    #[tokio::test]
    async fn a_changed_key_is_reported_from_its_own_probe() {
        // The probe answers by key: `sk-good` succeeds, anything else 401s.
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let store = Arc::new(LlmHealthStore::with_probe(
            Arc::new(move |cfg: LLMConfig| {
                counted.fetch_add(1, Ordering::SeqCst);
                let ok = cfg.api_key == "sk-good";
                Box::pin(async move {
                    if ok {
                        Ok(crate::llm::probe::ProbeOutcome {
                            resolved_base: "stub".to_string(),
                        })
                    } else {
                        Err("HTTP 401 Unauthorized".to_string())
                    }
                })
            }),
            DEFAULT_TTL,
        ));

        let good = vec![cfg("openai", "sk-good", "https://api.openai.com/v1")];
        assert_eq!(store.report(&good).await[0].status, ProviderStatus::Healthy);

        // Credential change (no explicit invalidation call: the fingerprint
        // alone already makes the old entry unreachable).
        let broken = vec![cfg("openai", "sk-broken", "https://api.openai.com/v1")];
        let report = &store.report(&broken).await[0];
        assert_eq!(report.status, ProviderStatus::Error);
        assert_eq!(report.message, "HTTP 401 Unauthorized");

        // Fixing the key reports healthy again. The earlier `sk-good` entry is
        // still inside the TTL, so it is served as-is (a real probe of exactly
        // this config, less than one TTL old) — no third request.
        assert_eq!(store.report(&good).await[0].status, ProviderStatus::Healthy);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// `retain_live` is the explicit change-path invalidation, and its
    /// granularity is per provider: dropping one provider's entry leaves the
    /// others' statuses (and badges) untouched.
    #[tokio::test]
    async fn retain_live_drops_only_the_changed_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            counting_probe(calls.clone(), true),
            DEFAULT_TTL,
        ));
        let openai = cfg("openai", "sk-a", "https://api.openai.com/v1");
        let deepseek = cfg("deepseek", "sk-b", "https://api.deepseek.com/v1");
        store.report(&[openai.clone(), deepseek.clone()]).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // An edit to openai only: its entry goes, deepseek's stays cached.
        let edited = cfg("openai", "sk-edited", "https://api.openai.com/v1");
        assert_eq!(store.retain_live(&[edited.clone(), deepseek.clone()]), 1);
        let reports = store.report(&[edited, deepseek]).await;
        assert!(reports.iter().all(|h| h.status == ProviderStatus::Healthy));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "only the changed provider is re-probed"
        );

        // Removing a provider drops its entry; clearing its key does too.
        assert_eq!(store.retain_live(&[]), 2);
    }

    /// A recorded manual probe is served to the next read instead of re-probing.
    #[tokio::test]
    async fn recorded_probe_is_served_to_the_next_read() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(LlmHealthStore::with_probe(
            counting_probe(calls.clone(), true),
            DEFAULT_TTL,
        ));
        let provider = cfg("openai", "sk-a", "https://api.openai.com/v1");

        store.record(&provider, ProviderHealth::error("HTTP 401 Unauthorized", 9));
        let report = &store.report(std::slice::from_ref(&provider)).await[0];
        assert_eq!(report.status, ProviderStatus::Error);
        assert_eq!(report.message, "HTTP 401 Unauthorized");
        assert_eq!(report.latency_ms, 9);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the recorded probe stands in for a read probe"
        );

        // A record for an unconfigured provider is a no-op (there is nothing to
        // report beyond `offline`).
        let mut keyless = provider;
        keyless.api_key = String::new();
        store.record(&keyless, ProviderHealth::healthy(1));
        assert_eq!(store.report(&[keyless]).await[0].status, ProviderStatus::Offline);
    }
}
