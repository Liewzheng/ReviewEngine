//! Cached per-instance git platform connectivity health (RENG-97).
//!
//! Everything that reports the GitLab/GitHub integration state reads from this
//! ONE store. The Configuration page's probe (`POST /config/git-platforms/test`
//! — the only real authenticated check) writes here; the dashboard health
//! section and `GET /system/health` both read here, so the same question has
//! the same answer on every surface. Before this module the dashboard called
//! "Configured" on mere presence (a `git_platforms` row, or the startup
//! env/CLI token flags) and `/system/health` guessed from the LLM provider
//! list, so a deployment whose credentials were broken reported healthy on
//! both pages while the Configuration page showed the real 401.
//!
//! Invariants:
//!
//! - **A verdict is always a real probe.** `Healthy` only ever follows a
//!   successful `GET {baseUrl}/api/v4/version`; a failed probe is `Error`
//!   carrying the transport/HTTP failure. There is no "assume healthy" from
//!   presence — an entry (or env/CLI token) that was never probed reads
//!   `unknown`.
//! - **An entry is never answered by another config's probe.** The cache is
//!   keyed by the entry's normalized `base_url`, and each record carries a
//!   fingerprint of the `(base_url, token)` it was probed with, so a changed
//!   credential — or a candidate token tested in the add dialog and never
//!   saved — cannot be reported as the stored entry's health: the lookup
//!   treats a fingerprint mismatch as "not probed".
//! - **The per-type row is a conservative aggregation.** Several entries may
//!   share a platform type (`gitlab`); any failure governs the type's row,
//!   otherwise the most recent successful probe's latency/timestamp is
//!   reported, and with no outcome for any entry the row is `unknown`, never
//!   `success`. (With a mix of probed-success and unprobed entries of one
//!   type, the successful probe is reported — the unprobed sibling is not
//!   probed again by the dashboard; probing is the Configuration page's job.)

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use chrono::{DateTime, Utc};

use crate::models::GitPlatformConfig;
use crate::server::AppState;

/// Verdict of one platform probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPlatformStatus {
    /// The probe reached the instance and authenticated.
    Healthy,
    /// The probe failed (rejected credentials, transport error, …).
    Error,
}

/// Last probe outcome for one platform entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPlatformHealth {
    pub status: GitPlatformStatus,
    /// Human detail: `Configured`, or the probe's failure
    /// (`HTTP 401 Unauthorized`, `error sending request…`).
    pub message: String,
    /// Instance version reported by a successful probe (GitLab
    /// `/api/v4/version`); `None` for a failure.
    pub version: Option<String>,
    /// Round-trip time of the probe itself; 0 when the probe failed (a
    /// failure's duration is not a link measurement).
    pub latency_ms: u64,
    /// When the probe ran — served to the UI so a stale result is visible.
    pub checked_at: DateTime<Utc>,
    /// Hash of the `(base_url, token)` this outcome was probed with, so a
    /// credential change can never be answered by the previous probe.
    fingerprint: String,
}

impl GitPlatformHealth {
    /// A probe that reached the instance and was accepted.
    pub fn healthy(version: Option<String>, latency_ms: u64) -> Self {
        Self {
            status: GitPlatformStatus::Healthy,
            message: "Configured".to_string(),
            version,
            latency_ms,
            checked_at: Utc::now(),
            fingerprint: String::new(),
        }
    }

    /// A probe that failed, carrying why.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: GitPlatformStatus::Error,
            message: message.into(),
            version: None,
            latency_ms: 0,
            checked_at: Utc::now(),
            fingerprint: String::new(),
        }
    }

    /// Attach the `(base_url, token)` fingerprint a probe ran with. Split from
    /// the constructors so a test can build a record for any `checked_at`.
    fn probed_with(mut self, base_url: &str, token: &str) -> Self {
        self.fingerprint = Self::fingerprint(base_url, token);
        self
    }

    /// SHA-256 over `(base_url, token)`, truncated to 12 hex chars. Hashed
    /// rather than stored so the cache never holds a second copy of a token.
    fn fingerprint(base_url: &str, token: &str) -> String {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        for field in [base_url, token] {
            hasher.update(field.as_bytes());
            hasher.update(b"\n");
        }
        let digest = hex::encode(hasher.finalize());
        digest[..12].to_string()
    }
}

/// Per-entry git platform health cache, shared through [`crate::server::AppState`].
///
/// Deliberately separate from [`super::llm_health::LlmHealthStore`]: platform
/// probes are user-initiated (the Configuration page's Test button) rather
/// than poll-driven, so there is no TTL, no flight gate and no background
/// refresh — the store only ever holds what a probe actually ran.
pub struct GitHealthStore {
    /// Last probe outcome per normalized `base_url`; never held across an
    /// `.await`.
    entries: RwLock<HashMap<String, GitPlatformHealth>>,
}

impl GitHealthStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Record a probe that just ran against `base_url` with `token`.
    pub fn record(&self, base_url: &str, token: &str, health: GitPlatformHealth) {
        self.entries
            .write()
            .unwrap()
            .insert(base_url.to_string(), health.probed_with(base_url, token));
    }

    /// The cached outcome for `platform` — only when it was probed with this
    /// entry's CURRENT token. A changed credential makes the old outcome
    /// unreachable (reported as never-probed until the new config is tested).
    pub fn lookup(&self, platform: &GitPlatformConfig) -> Option<GitPlatformHealth> {
        let entries = self.entries.read().unwrap();
        let entry = entries.get(&platform.base_url)?;
        (entry.fingerprint == GitPlatformHealth::fingerprint(&platform.base_url, &platform.token))
            .then(|| entry.clone())
    }

    /// Drop every cached outcome whose base URL is no longer configured
    /// (memory hygiene on `PUT /config`). Returns how many were dropped.
    pub fn retain_live(&self, platforms: &[GitPlatformConfig]) -> usize {
        let live: HashSet<&str> = platforms.iter().map(|p| p.base_url.as_str()).collect();
        let mut entries = self.entries.write().unwrap();
        let before = entries.len();
        entries.retain(|url, _| live.contains(url.as_str()));
        before - entries.len()
    }
}

impl Default for GitHealthStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The dashboard / `/system/health` view of one platform type, aggregated
/// across its configured entries. The single honest answer both surfaces
/// serve: `offline` when nothing is configured, `unknown` when configured but
/// never probed (env/CLI tokens included), `error` with the failure when a
/// probe failed, `success` with the real latency/timestamp when one succeeded.
///
/// `latencyMs` appears only on `success` (a real round-trip measurement) and
/// `checkedAt` only when a probe ran — a missing timestamp IS the "never
/// probed" signal.
pub fn integration_row(
    state: &AppState,
    service: &str,
    platform_type: &str,
    env_configured: bool,
) -> serde_json::Value {
    let entries: Vec<GitPlatformConfig> = state
        .git_platforms
        .read()
        .unwrap()
        .iter()
        .filter(|p| p.platform_type.eq_ignore_ascii_case(platform_type))
        .cloned()
        .collect();

    if !env_configured && entries.is_empty() {
        return serde_json::json!({
            "service": service,
            "type": "integration",
            "status": "offline",
            "message": "Not configured",
        });
    }

    let outcomes: Vec<GitPlatformHealth> = entries
        .iter()
        .filter_map(|platform| state.git_health.lookup(platform))
        .collect();
    if outcomes.is_empty() {
        return serde_json::json!({
            "service": service,
            "type": "integration",
            "status": "unknown",
            "message": "Not probed yet",
        });
    }

    // Failure wins: one broken entry must never be masked by a healthy
    // sibling. Report the most recent failure's message so the row says what
    // is actually wrong.
    if let Some(failure) = outcomes
        .iter()
        .filter(|h| h.status == GitPlatformStatus::Error)
        .max_by_key(|h| h.checked_at)
    {
        return serde_json::json!({
            "service": service,
            "type": "integration",
            "status": "error",
            "message": failure.message,
            "checkedAt": failure.checked_at.to_rfc3339(),
        });
    }

    // Otherwise the most recent successful probe stands for the type (reached
    // only with a non-empty outcome set — the empty and failure cases returned
    // above).
    let success = outcomes.iter().max_by_key(|h| h.checked_at).unwrap_or(&outcomes[0]);
    serde_json::json!({
        "service": service,
        "type": "integration",
        "status": "success",
        "message": "Configured",
        "latencyMs": success.latency_ms,
        "checkedAt": success.checked_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn platform(name: &str, platform_type: &str, base_url: &str, token: &str) -> GitPlatformConfig {
        GitPlatformConfig {
            name: name.to_string(),
            platform_type: platform_type.to_string(),
            base_url: base_url.to_string(),
            token: token.to_string(),
            ..Default::default()
        }
    }

    fn state_with(platforms: Vec<GitPlatformConfig>, env_gitlab: bool) -> Arc<AppState> {
        let mut state = AppState::new(vec![]);
        *state.git_platforms.write().unwrap() = platforms;
        state.env_gitlab_configured = env_gitlab;
        Arc::new(state)
    }

    fn row(state: &AppState, platform_type: &str, env: bool) -> serde_json::Value {
        integration_row(state, "GitLab API", platform_type, env)
    }

    /// A record whose `checked_at` is fixed, so aggregation tests can pin
    /// which probe "most recent" picks.
    fn record_at(
        store: &GitHealthStore,
        platform: &GitPlatformConfig,
        at: chrono::NaiveDateTime,
        status: GitPlatformStatus,
        message: &str,
    ) {
        let mut health = match status {
            GitPlatformStatus::Healthy => GitPlatformHealth::healthy(Some("16.9.0".to_string()), 42),
            GitPlatformStatus::Error => GitPlatformHealth::error(message),
        };
        health.checked_at = at.and_utc();
        store.record(&platform.base_url, &platform.token, health);
    }

    #[test]
    fn fingerprint_tracks_base_url_and_token() {
        let a = GitPlatformHealth::fingerprint("https://gitlab.example", "tok-a");
        assert_eq!(a, GitPlatformHealth::fingerprint("https://gitlab.example", "tok-a"));
        assert_eq!(a.len(), 12);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, GitPlatformHealth::fingerprint("https://gitlab.example", "tok-b"));
        assert_ne!(a, GitPlatformHealth::fingerprint("https://other.example", "tok-a"));
        // The token never appears in the key.
        assert!(!a.contains("tok-a"));
    }

    #[test]
    fn lookup_ignores_a_probe_run_with_a_different_token() {
        let store = GitHealthStore::new();
        let stored = platform("gitlab", "gitlab", "https://gitlab.example", "stored-tok");
        store.record(
            "https://gitlab.example",
            "candidate-tok",
            GitPlatformHealth::healthy(None, 5),
        );
        assert_eq!(
            store.lookup(&stored),
            None,
            "a candidate token tested but never saved must not report the stored entry's health"
        );

        store.record(
            "https://gitlab.example",
            "stored-tok",
            GitPlatformHealth::healthy(None, 5),
        );
        assert!(store.lookup(&stored).is_some(), "the stored token's probe is served");

        // A credential change invalidates the old outcome until re-probed.
        let rotated = platform("gitlab", "gitlab", "https://gitlab.example", "new-tok");
        assert_eq!(
            store.lookup(&rotated),
            None,
            "a rotated token must not be answered by the old probe"
        );
    }

    #[test]
    fn retain_live_drops_only_removed_base_urls() {
        let store = GitHealthStore::new();
        let a = platform("a", "gitlab", "https://a.example", "t");
        let b = platform("b", "gitlab", "https://b.example", "t");
        store.record(&a.base_url, &a.token, GitPlatformHealth::healthy(None, 1));
        store.record(&b.base_url, &b.token, GitPlatformHealth::healthy(None, 1));
        assert_eq!(store.retain_live(std::slice::from_ref(&a)), 1);
        assert!(store.lookup(&a).is_some());
        assert_eq!(store.lookup(&b), None);
    }

    #[tokio::test]
    async fn no_entry_is_offline() {
        let state = state_with(vec![], false);
        let json = row(&state, "gitlab", false);
        assert_eq!(json["status"], "offline");
        assert_eq!(json["message"], "Not configured");
        assert!(json.get("checkedAt").is_none());
    }

    #[tokio::test]
    async fn configured_but_never_probed_is_unknown() {
        let state = state_with(vec![platform("gitlab", "gitlab", "https://gitlab.example", "t")], false);
        let json = row(&state, "gitlab", false);
        assert_eq!(json["status"], "unknown");
        assert_eq!(json["message"], "Not probed yet");
        assert!(json.get("checkedAt").is_none(), "no probe, no timestamp: {json}");
        assert!(json.get("latencyMs").is_none());
    }

    #[tokio::test]
    async fn env_flag_alone_is_unknown_not_success() {
        // `GITLAB_TOKEN` at startup wires the review client without a
        // `git_platforms` entry, so there is nothing the Configuration probe
        // can test — presence must not read as health.
        let state = state_with(vec![], true);
        let json = row(&state, "gitlab", true);
        assert_eq!(json["status"], "unknown");
        assert_eq!(json["message"], "Not probed yet");
    }

    #[tokio::test]
    async fn failed_probe_is_error_with_message_and_timestamp() {
        let gitlab = platform("gitlab", "gitlab", "https://gitlab.example", "t");
        let state = state_with(vec![gitlab.clone()], false);
        state.git_health.record(
            &gitlab.base_url,
            &gitlab.token,
            GitPlatformHealth::error("HTTP 401 Unauthorized"),
        );
        let json = row(&state, "gitlab", false);
        assert_eq!(json["status"], "error");
        assert_eq!(json["message"], "HTTP 401 Unauthorized");
        assert!(
            json["checkedAt"].as_str().is_some(),
            "a failure carries its probe time: {json}"
        );
        assert!(
            json.get("latencyMs").is_none(),
            "a failed probe records no latency: {json}"
        );
    }

    #[tokio::test]
    async fn successful_probe_is_success_with_latency_and_timestamp() {
        let gitlab = platform("gitlab", "gitlab", "https://gitlab.example", "t");
        let state = state_with(vec![gitlab.clone()], false);
        state.git_health.record(
            &gitlab.base_url,
            &gitlab.token,
            GitPlatformHealth::healthy(Some("16.9.0".to_string()), 47),
        );
        let json = row(&state, "gitlab", false);
        assert_eq!(json["status"], "success");
        assert_eq!(json["message"], "Configured");
        assert_eq!(json["latencyMs"], 47);
        assert!(json["checkedAt"].as_str().is_some());
    }

    #[tokio::test]
    async fn any_failure_governs_across_entries_of_one_type() {
        let gitlab = platform("gitlab", "gitlab", "https://gitlab.example", "t");
        let other = platform("other", "gitlab", "https://other.example", "t");
        let state = state_with(vec![gitlab.clone(), other.clone()], false);
        state
            .git_health
            .record(&gitlab.base_url, &gitlab.token, GitPlatformHealth::healthy(None, 5));
        state.git_health.record(
            &other.base_url,
            &other.token,
            GitPlatformHealth::error("HTTP 403 Forbidden"),
        );
        let json = row(&state, "gitlab", false);
        assert_eq!(json["status"], "error", "a broken sibling must not be masked: {json}");
        assert_eq!(json["message"], "HTTP 403 Forbidden");
    }

    #[tokio::test]
    async fn the_most_recent_success_stands_when_all_succeed() {
        let gitlab = platform("gitlab", "gitlab", "https://gitlab.example", "t");
        let other = platform("other", "gitlab", "https://other.example", "t");
        let state = state_with(vec![gitlab.clone(), other.clone()], false);
        record_at(
            &state.git_health,
            &gitlab,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
            GitPlatformStatus::Healthy,
            "Configured",
        );
        record_at(
            &state.git_health,
            &other,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 2)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
            GitPlatformStatus::Healthy,
            "Configured",
        );
        let json = row(&state, "gitlab", false);
        assert_eq!(json["status"], "success");
        assert_eq!(json["latencyMs"], 42, "the newest successful probe's latency: {json}");
    }
}
