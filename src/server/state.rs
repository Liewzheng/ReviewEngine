//! Application state shared across all HTTP route handlers.
//!
//! [`AppState`] is injected into every Axum route via
//! `axum::extract::State`. It holds LLM configurations, the Prometheus
//! metrics registry, review progress tracking, the background task
//! store, and the resolved application configuration.

use prometheus::Registry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, Utc};

use crate::feedback::FeedbackStore;
use crate::models::{ExpertTomlDef, LLMConfig};
use crate::server::api::config::UiConfig;
use crate::server::log_collector::LogCollector;
use crate::server::task_queue::TaskStore;
use crate::upgrade::InstallMethod;

/// Lifecycle of the self-upgrade job surfaced by `/api/v1/system/upgrade/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpgradeJobState {
    Idle,
    Checking,
    Downloading,
    Verifying,
    Installing,
    Done,
    Failed,
    #[serde(rename = "notSupported")]
    NotSupported,
}

impl UpgradeJobState {
    /// States that mean an upgrade is in flight (the single-flight gate).
    /// States that mean an upgrade is in flight (the single-flight gate).
    ///
    /// Only one upgrade job may run at a time; this check prevents
    /// concurrent download/verify/install cycles.
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading | Self::Verifying | Self::Installing
        )
    }
}

/// Real-time download progress for the upgrade's download phase.
///
/// Serialized into the `download` field of the upgrade-status payload with
/// camelCase keys so the frontend can render a progress bar, transfer speed,
/// and ETA while the job is in the `downloading` state.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    /// Cumulative bytes written across all downloads of this job so far.
    pub downloaded_bytes: u64,
    /// Total bytes expected for all downloads of this job (from GitHub API metadata).
    pub total_bytes: u64,
    /// When the download phase started (RFC3339/ISO8601 UTC once serialized).
    pub started_at: DateTime<Utc>,
}

/// Snapshot of the current upgrade job for the status endpoint.
/// Snapshot of the current self-upgrade job for the status endpoint.
///
/// Polling `GET /api/v1/system/upgrade/status` returns this struct so
/// the frontend can display a progress bar and status message.
#[derive(Debug, Clone)]
pub struct UpgradeJob {
    /// Current lifecycle state of the upgrade.
    pub state: UpgradeJobState,
    /// Human-readable status message (e.g. "Downloading v0.9.17…").
    pub message: String,
    /// Version currently installed.
    pub current_version: String,
    /// Target version being upgraded to (if an upgrade is in flight).
    pub target_version: Option<String>,
    /// When the upgrade job started (for elapsed-time display).
    pub started_at: Option<DateTime<Utc>>,
    /// Live download progress; `None` when no download is in progress or
    /// sizes are unknown. Left set after the download phase ends — the
    /// frontend hides it outside the `downloading` state.
    pub download: Option<DownloadProgress>,
}

impl Default for UpgradeJob {
    fn default() -> Self {
        Self {
            state: UpgradeJobState::Idle,
            message: "idle".to_string(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            target_version: None,
            started_at: None,
            download: None,
        }
    }
}

/// A cached GitHub check result plus when it was produced.
///
/// The TTL (1h, enforced by the upgrade handlers) protects the unauthenticated
/// GitHub API rate limit of 60 requests/hour per IP.
#[derive(Debug, Clone)]
pub struct UpgradeCache {
    pub check: crate::upgrade::UpdateCheck,
    pub cached_at: DateTime<Utc>,
}

/// A cached models.dev catalog plus when it was produced.
///
/// The TTL (24h, enforced by the catalog handlers) keeps the interactive
/// endpoints snappy and models.dev traffic negligible; on fetch failure the
/// handlers serve the stale disk cache, then the stale in-memory entry, then
/// the builtin static catalog — the endpoints never error on an outage.
#[derive(Debug, Clone)]
pub struct CatalogCache {
    pub catalog: Arc<crate::catalog::Catalog>,
    pub cached_at: DateTime<Utc>,
}

/// In-memory store for the models.dev provider catalog.
///
/// Deliberately separate from [`UpgradeStore`]: the catalog has its own TTL
/// and disk fallback, and reusing the upgrade cache would couple unrelated
/// refresh cycles.
pub struct CatalogStore {
    /// The cached catalog plus its fetch timestamp; the TTL gate lives in
    /// the catalog handlers. Never held across an `.await`.
    pub cache: RwLock<Option<CatalogCache>>,
    /// Single-flight gate for network refreshes: concurrent requests that
    /// find the cache expired queue here behind one fetch instead of each
    /// hitting models.dev independently (thundering herd). A Tokio mutex
    /// because it is held across the async fetch; waiters re-check the cache
    /// after acquiring it in case a competitor already refreshed.
    pub fetch_lock: tokio::sync::Mutex<()>,
}

impl CatalogStore {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(None),
            fetch_lock: tokio::sync::Mutex::new(()),
        }
    }
}

impl Default for CatalogStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Lightweight single-flight store for the self-upgrade API.
///
/// Deliberately separate from [`TaskStore`]: review tasks and upgrade jobs
/// share no semantics, and reusing the review store would leak review state
/// into the upgrade path (and vice versa).
pub struct UpgradeStore {
    /// The current upgrade job; the single-flight gate lives in this lock.
    pub job: RwLock<UpgradeJob>,
    /// Install method resolved once at startup (reuses `upgrade::install_method`).
    pub install_method: InstallMethod,
    /// Cached GitHub check result with its timestamp.
    pub cache: RwLock<Option<UpgradeCache>>,
}

impl UpgradeStore {
    /// Resolve the install method once. Honors `REVIEW_UPGRADE_METHOD`
    /// (`binary|plain|brew|docker|cargo|unknown`) as a test/deployment seam;
    /// otherwise falls back to `InstallMethod::detect()`.
    pub fn new() -> Self {
        let install_method = std::env::var("REVIEW_UPGRADE_METHOD")
            .ok()
            .and_then(|v| parse_install_method_override(&v))
            .unwrap_or_else(InstallMethod::detect);
        Self {
            job: RwLock::new(UpgradeJob::default()),
            install_method,
            cache: RwLock::new(None),
        }
    }

    /// Test seam: force a specific install method.
    pub fn with_install_method(method: InstallMethod) -> Self {
        Self {
            job: RwLock::new(UpgradeJob::default()),
            install_method: method,
            cache: RwLock::new(None),
        }
    }
}

/// Map a `REVIEW_UPGRADE_METHOD` value onto an [`InstallMethod`].
fn parse_install_method_override(value: &str) -> Option<InstallMethod> {
    match value.trim().to_ascii_lowercase().as_str() {
        "binary" | "plain" => Some(InstallMethod::Plain),
        "brew" | "homebrew" => Some(InstallMethod::Brew),
        "docker" => Some(InstallMethod::Docker),
        "cargo" => Some(InstallMethod::Cargo),
        "unknown" => Some(InstallMethod::Unknown),
        _ => None,
    }
}

/// Shared application state injected into every Axum route handler.
pub struct AppState {
    /// LLM configurations available for review prompts (mutable for runtime updates).
    pub llm_configs: RwLock<Vec<LLMConfig>>,
    /// Prometheus metrics registry (optional).
    pub registry: Option<Registry>,
    /// Shared progress map for tracking review status (optional).
    pub progress_map: Option<crate::progress::ProgressMap>,
    /// Background task store for async review processing (optional).
    pub task_store: Option<Arc<TaskStore>>,
    /// Resolved application configuration (optional, wrapped for runtime mutation).
    pub app_config: RwLock<Option<Arc<crate::models::AppConfig>>>,
    /// In-memory log collector for SSE streaming (optional).
    pub log_collector: Option<Arc<Mutex<LogCollector>>>,
    /// UI-facing configuration (frontend-compatible shape, persisted in-memory).
    pub ui_config: RwLock<UiConfig>,
    /// Configured git platform instances (live secrets; the UI only ever
    /// sees masked projections via `ui_config`). Hot-updated by
    /// `PUT /api/v1/config` and consulted for `gitlab_mr` credential
    /// resolution and per-instance webhook verification.
    pub git_platforms: RwLock<Vec<crate::models::GitPlatformConfig>>,
    /// Startup-recorded env/CLI git integration flags (RENG-32): the classic
    /// `--gitlab-token` / `GITLAB_TOKEN` / `--github-token` / `GITHUB_TOKEN`
    /// channels wire webhook/MR-fetch clients directly and never appear in
    /// `git_platforms`, so the dashboard health panel ORs these in when
    /// deciding whether an integration is configured. Write-once at startup
    /// before the state is shared; `false` everywhere else (tests,
    /// embedded use).
    pub env_gitlab_configured: bool,
    pub env_github_configured: bool,
    /// Where `PUT /api/v1/config` persists UI-managed state
    /// (`ui-state.toml`, see `server::api::config::persist`). `None`
    /// disables persistence (tests, embedded use) so unit tests never
    /// write to the real config dir.
    pub ui_state_path: Option<std::path::PathBuf>,
    /// Which config values came from CLI/env at startup. Consulted by the
    /// ui-state SAVE path so env-derived secrets are never persisted, and
    /// by the LOAD path so env wins over the file. `None` in tests (direct
    /// state construction) → no env filtering.
    pub ui_state_env: Option<crate::server::api::config::persist::UiStateEnvOverrides>,
    /// Finding feedback store for user verdicts (optional).
    pub feedback_store: Option<Arc<FeedbackStore>>,
    /// Persistent database handle (0.10.0; PG primary / SQLite fallback).
    /// `None` = 0.9 behaviour (pure in-memory + ui-state.toml file), used by
    /// tests, embedded use, and the `REVIEW_DISABLE_DB=1` escape hatch. Set
    /// after pool + migrate succeed at startup, before the config replay.
    pub db: Option<Arc<crate::store::SqlxStore>>,
    /// Self-upgrade single-flight store + GitHub check cache + install method.
    pub upgrade: UpgradeStore,
    /// In-memory models.dev catalog cache (24h TTL enforced by handlers).
    pub catalog: CatalogStore,
    /// Cached per-provider LLM connectivity health (RENG-36): the single
    /// source both `GET /api/v1/llm/providers` and the dashboard health
    /// section report from, keyed by the exact provider config a probe ran
    /// with so a credential change can never be answered by the previous
    /// config's status.
    pub llm_health: Arc<crate::server::api::llm_health::LlmHealthStore>,
    /// WebUI expert overrides (RENG-69), keyed by expert name. The source of
    /// truth for what `PUT /api/v1/system/experts/{id}` changed, kept beside
    /// the persisted `app_settings` row so every review dispatch can re-apply
    /// it to its own freshly resolved config (neither `run_review` nor
    /// `run_review_common` reads `app_config`; both re-resolve the config
    /// file). Empty when no DB is attached (`REVIEW_DISABLE_DB=1`, tests) —
    /// expert edits are then memory-only, exactly as before 0.10.24.
    pub expert_overrides: RwLock<Arc<crate::config::ExpertOverrides>>,
    /// The `[review_experts]` team as the config file resolved it, captured
    /// before any WebUI override was applied (RENG-93). [`Self::set_expert_overrides`]
    /// re-applies the override map over THIS snapshot on every edit, so
    /// removing an override (a cleared prompt) restores the file value instead
    /// of leaving the previous override baked into the running `app_config`.
    /// `None` until the first override application — at that point whatever
    /// the state was seeded with IS the base (the startup replay runs before
    /// any PUT, and tests seed the file values directly).
    pub expert_base: RwLock<Option<HashMap<String, ExpertTomlDef>>>,
    /// RENG-95: the WebUI-set report-level aggregation flag (`report.aggregated`).
    /// `Some(_)` when the experts page (or a persisted `ui` row) decided it;
    /// `None` when only the config file / inline request TOML speaks. Every
    /// review path re-resolves its config from the file and never reads
    /// `app_config`, so this override is threaded to them exactly like
    /// [`Self::expert_overrides`] and applied over the config they resolve.
    pub report_aggregated: RwLock<Option<bool>>,
}

impl AppState {
    /// Create a new `AppState` with the given LLM configs.
    ///
    /// Optional fields except `task_store` are initialised to `None`; set
    /// them directly or with builder-style methods as needed. The task store
    /// is created eagerly so EVERY `AppState` can record review tasks — the
    /// old `None` default left webhook-dispatched reviews (and every test
    /// state) without a store, which is what made the dashboard / queue /
    /// `/reviews` pages empty.
    pub fn new(llm_configs: Vec<LLMConfig>) -> Self {
        Self {
            llm_configs: RwLock::new(llm_configs),
            registry: None,
            progress_map: None,
            task_store: Some(Arc::new(TaskStore::new())),
            app_config: RwLock::new(None),
            log_collector: None,
            ui_config: RwLock::new(UiConfig::default()),
            git_platforms: RwLock::new(Vec::new()),
            env_gitlab_configured: false,
            env_github_configured: false,
            ui_state_path: None,
            ui_state_env: None,
            feedback_store: None,
            db: None,
            upgrade: UpgradeStore::new(),
            catalog: CatalogStore::new(),
            llm_health: Arc::new(crate::server::api::llm_health::LlmHealthStore::new()),
            expert_overrides: RwLock::new(Arc::new(crate::config::ExpertOverrides::default())),
            expert_base: RwLock::new(None),
            report_aggregated: RwLock::new(None),
        }
    }

    /// The authoritative provider chain a review runs on: the persisted
    /// primary selection first, then the remaining providers in their stored
    /// order (RENG-55, [`crate::llm::ordered_llm_configs`]), with DISABLED
    /// providers skipped entirely (RENG-75) — a disabled provider is never
    /// used by a review. Every review-executing entry point (REST submit,
    /// repo review, GitLab/GitHub webhooks) must take its configs from here —
    /// reading `llm_configs` directly reintroduces the bug where the primary
    /// was ignored.
    pub fn ordered_llm_configs(&self) -> Vec<LLMConfig> {
        // Sequential reads (never nested) so a concurrent `PUT /config` — which
        // writes `llm_configs` then `ui_config` — cannot deadlock against us.
        let primary = self.ui_config.read().unwrap().llm.primary_provider.clone();
        let configs = self.llm_configs.read().unwrap();
        crate::llm::ordered_llm_configs(&primary, &configs)
    }

    /// Snapshot of the WebUI expert overrides (RENG-69). Cheap: the map is
    /// immutable behind an `Arc`, so a review dispatch clones the handle, not
    /// the data. Review paths take this snapshot at enqueue time and apply it
    /// to the config they resolve themselves.
    pub fn expert_overrides_snapshot(&self) -> Arc<crate::config::ExpertOverrides> {
        self.expert_overrides.read().unwrap().clone()
    }

    /// The WebUI-set report-level aggregation override (RENG-95): `Some(_)`
    /// when the experts page (or a persisted `ui` row) decided
    /// `report.aggregated`, `None` when only the config file / inline request
    /// TOML speaks. Review dispatches snapshot it at enqueue time and apply it
    /// over the config they resolve for themselves — the aggregation-flag
    /// counterpart of [`Self::expert_overrides_snapshot`].
    pub fn aggregation_override(&self) -> Option<bool> {
        *self.report_aggregated.read().unwrap()
    }

    /// Set the WebUI report-level aggregation override (RENG-95). `None`
    /// clears it — only the config file / inline request TOML decides again.
    pub fn set_aggregation_override(&self, value: Option<bool>) {
        *self.report_aggregated.write().unwrap() = value;
    }

    /// Publish `overrides` as the runtime override map AND apply them to the
    /// current `app_config` (so `GET /system/experts` and the
    /// `app_config`-consuming paths — repo scans, `inject_agents_md` — see the
    /// edited values immediately). Returns the number of experts patched.
    ///
    /// The map is re-applied over [`Self::expert_base`] — the config-file
    /// resolution captured on the first application — not over the previous
    /// result, so removing a field from the map (a cleared prompt) restores the
    /// file value instead of leaving the prior override baked in.
    ///
    /// Lock order is fixed here (`app_config` → `expert_base`, then
    /// `expert_overrides` alone after the block) and this is the only place
    /// the expert locks are written, so it cannot deadlock against the readers
    /// of any of them.
    pub fn set_expert_overrides(&self, overrides: crate::config::ExpertOverrides) -> usize {
        let applied = {
            let mut cfg_opt = self.app_config.write().unwrap();
            let mut base_opt = self.expert_base.write().unwrap();
            match cfg_opt.as_mut() {
                Some(arc) => {
                    let cfg = Arc::make_mut(arc);
                    let base = base_opt.get_or_insert_with(|| cfg.review_experts.clone());
                    // Re-apply over the base, never over the previous result:
                    // a field the new map no longer covers (a cleared prompt)
                    // must fall back to the config-file value, not keep the
                    // previous override baked into the running config.
                    cfg.review_experts = base.clone();
                    overrides.apply_to(cfg)
                }
                None => 0,
            }
        };
        *self.expert_overrides.write().unwrap() = Arc::new(overrides);
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_new_empty() {
        let state = AppState::new(vec![]);
        assert!(state.llm_configs.read().unwrap().is_empty());
        assert!(state.registry.is_none());
        assert!(state.progress_map.is_none());
        // The task store is initialized eagerly so every AppState can record
        // review tasks (webhook + REST); the old `None` default is what made
        // the dashboard / queue / reviews pages empty.
        assert!(state.task_store.is_some());
        assert!(state.app_config.read().unwrap().is_none());
        assert!(state.log_collector.is_none());
        assert!(state.feedback_store.is_none());
    }

    #[test]
    fn test_app_state_new_with_configs() {
        let configs = vec![LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key: "sk-test".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.7,
            disable_thinking: None,
            disabled: false,
        }];
        let state = AppState::new(configs);
        let llm = state.llm_configs.read().unwrap();
        assert_eq!(llm.len(), 1);
        assert_eq!(llm[0].provider, "openai");
        assert!(state.registry.is_none());
    }

    #[test]
    fn test_app_state_fields_are_pub() {
        // Verify that fields are accessible (they're pub)
        let state = AppState::new(vec![]);
        let _llm: &RwLock<Vec<LLMConfig>> = &state.llm_configs;
        let _reg: &Option<Registry> = &state.registry;
        let _upgrade: &UpgradeStore = &state.upgrade;
    }

    // ─── upgrade store ─────────────────────────────────────────

    #[test]
    fn upgrade_job_defaults_to_idle_with_current_version() {
        let job = UpgradeJob::default();
        assert_eq!(job.state, UpgradeJobState::Idle);
        assert_eq!(job.current_version, env!("CARGO_PKG_VERSION"));
        assert!(job.target_version.is_none());
        assert!(job.download.is_none());
    }

    #[test]
    fn download_progress_serializes_camel_case_contract() {
        let started_at = DateTime::parse_from_rfc3339("2026-08-19T11:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let progress = DownloadProgress {
            downloaded_bytes: 1_234_567,
            total_bytes: 14_901_232,
            started_at,
        };
        let value = serde_json::to_value(&progress).unwrap();
        assert_eq!(value["downloadedBytes"], 1_234_567);
        assert_eq!(value["totalBytes"], 14_901_232);
        assert!(value.get("downloaded_bytes").is_none(), "snake_case key must not leak");
        assert!(value.get("total_bytes").is_none(), "snake_case key must not leak");
        // startedAt must be RFC3339/ISO8601 UTC.
        let parsed = DateTime::parse_from_rfc3339(value["startedAt"].as_str().unwrap()).unwrap();
        assert_eq!(parsed.with_timezone(&Utc), started_at);
        assert!(value["startedAt"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn upgrade_state_running_semantics() {
        assert!(UpgradeJobState::Checking.is_running());
        assert!(UpgradeJobState::Downloading.is_running());
        assert!(UpgradeJobState::Verifying.is_running());
        assert!(UpgradeJobState::Installing.is_running());
        assert!(!UpgradeJobState::Idle.is_running());
        assert!(!UpgradeJobState::Done.is_running());
        assert!(!UpgradeJobState::Failed.is_running());
        assert!(!UpgradeJobState::NotSupported.is_running());
    }

    #[test]
    fn upgrade_state_serializes_to_contract_names() {
        assert_eq!(serde_json::to_value(UpgradeJobState::Idle).unwrap(), "idle");
        assert_eq!(serde_json::to_value(UpgradeJobState::Checking).unwrap(), "checking");
        assert_eq!(
            serde_json::to_value(UpgradeJobState::Downloading).unwrap(),
            "downloading"
        );
        assert_eq!(serde_json::to_value(UpgradeJobState::Verifying).unwrap(), "verifying");
        assert_eq!(serde_json::to_value(UpgradeJobState::Installing).unwrap(), "installing");
        assert_eq!(serde_json::to_value(UpgradeJobState::Done).unwrap(), "done");
        assert_eq!(serde_json::to_value(UpgradeJobState::Failed).unwrap(), "failed");
        assert_eq!(
            serde_json::to_value(UpgradeJobState::NotSupported).unwrap(),
            "notSupported"
        );
    }

    #[test]
    fn install_method_override_mapping() {
        assert_eq!(parse_install_method_override("binary"), Some(InstallMethod::Plain));
        assert_eq!(parse_install_method_override("plain"), Some(InstallMethod::Plain));
        assert_eq!(parse_install_method_override("Brew"), Some(InstallMethod::Brew));
        assert_eq!(parse_install_method_override("docker"), Some(InstallMethod::Docker));
        assert_eq!(parse_install_method_override("cargo"), Some(InstallMethod::Cargo));
        assert_eq!(parse_install_method_override("unknown"), Some(InstallMethod::Unknown));
        assert_eq!(parse_install_method_override("nonsense"), None);
        assert_eq!(parse_install_method_override(""), None);
    }

    #[test]
    fn upgrade_store_with_forced_method() {
        let store = UpgradeStore::with_install_method(InstallMethod::Docker);
        assert_eq!(store.install_method, InstallMethod::Docker);
        assert_eq!(store.job.read().unwrap().state, UpgradeJobState::Idle);
    }

    // ─── authoritative provider chain (RENG-55) ────────────────

    fn llm(provider: &str) -> LLMConfig {
        LLMConfig {
            provider: provider.to_string(),
            model: format!("{provider}-model"),
            api_key: "k".to_string(),
            api_base: format!("https://api.{provider}.example/v1"),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        }
    }

    /// `AppState::ordered_llm_configs` is the single producer of the runtime
    /// chain: the persisted `llm.primaryProvider` leads, the rest keeps the
    /// stored order.
    #[test]
    fn ordered_llm_configs_puts_the_persisted_primary_first() {
        let state = AppState::new(vec![llm("xiaomi"), llm("deepseek")]);
        // Default UI config carries no primary → stored order.
        assert_eq!(
            state
                .ordered_llm_configs()
                .iter()
                .map(|c| c.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["xiaomi", "deepseek"]
        );

        state.ui_config.write().unwrap().llm.primary_provider = "deepseek".to_string();
        assert_eq!(
            state
                .ordered_llm_configs()
                .iter()
                .map(|c| c.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek", "xiaomi"],
            "the persisted primary must lead the chain"
        );
    }

    /// RENG-75: the runtime chain skips disabled providers — disabling the
    /// recorded primary moves the head to the next enabled entry, and the
    /// recorded primary cannot pull a disabled provider back in.
    #[test]
    fn ordered_llm_configs_skips_disabled_providers() {
        let mut configs = vec![llm("xiaomi"), llm("deepseek"), llm("openai")];
        configs[0].disabled = true;
        let state = AppState::new(configs);

        // Recorded primary names the DISABLED head: it is skipped, the first
        // enabled entry leads.
        state.ui_config.write().unwrap().llm.primary_provider = "xiaomi".to_string();
        assert_eq!(
            state
                .ordered_llm_configs()
                .iter()
                .map(|c| c.provider.as_str())
                .collect::<Vec<_>>(),
            vec!["deepseek", "openai"],
            "a disabled recorded primary is skipped like any disabled entry"
        );

        // All disabled → empty chain (the REST gate turns this into the
        // named all-disabled 422 before a review is enqueued).
        for c in state.llm_configs.write().unwrap().iter_mut() {
            c.disabled = true;
        }
        assert!(state.ordered_llm_configs().is_empty());
    }
}
