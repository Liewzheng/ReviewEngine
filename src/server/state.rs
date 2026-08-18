//! Application state shared across all HTTP route handlers.
//!
//! [`AppState`] is injected into every Axum route via
//! `axum::extract::State`. It holds LLM configurations, the Prometheus
//! metrics registry, review progress tracking, the background task
//! store, and the resolved application configuration.

use prometheus::Registry;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, Utc};

use crate::feedback::FeedbackStore;
use crate::models::LLMConfig;
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
}

impl Default for UpgradeJob {
    fn default() -> Self {
        Self {
            state: UpgradeJobState::Idle,
            message: "idle".to_string(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            target_version: None,
            started_at: None,
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
/// handlers serve the stale disk cache before erroring.
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
    /// the catalog handlers.
    pub cache: RwLock<Option<CatalogCache>>,
}

impl CatalogStore {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(None),
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
    /// Finding feedback store for user verdicts (optional).
    pub feedback_store: Option<Arc<FeedbackStore>>,
    /// Self-upgrade single-flight store + GitHub check cache + install method.
    pub upgrade: UpgradeStore,
    /// In-memory models.dev catalog cache (24h TTL enforced by handlers).
    pub catalog: CatalogStore,
}

impl AppState {
    /// Create a new `AppState` with the given LLM configs.
    ///
    /// All optional fields are initialised to `None`; set them directly
    /// or with builder-style methods as needed.
    pub fn new(llm_configs: Vec<LLMConfig>) -> Self {
        Self {
            llm_configs: RwLock::new(llm_configs),
            registry: None,
            progress_map: None,
            task_store: None,
            app_config: RwLock::new(None),
            log_collector: None,
            ui_config: RwLock::new(UiConfig::default()),
            feedback_store: None,
            upgrade: UpgradeStore::new(),
            catalog: CatalogStore::new(),
        }
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
        assert!(state.task_store.is_none());
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
}
