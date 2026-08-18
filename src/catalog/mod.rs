//! models.dev provider catalog integration.
//!
//! Fetches `https://models.dev/api.json` — a community-maintained registry of
//! hundreds of LLM providers and their models — so users no longer need
//! hand-maintained provider presets. The catalog backs two consumers:
//!
//! - the REST endpoints under `/api/v1/catalog` ([`crate::server::api::catalog`]),
//! - the interactive `review-engine init` flow ([`crate::actions::init`]).
//!
//! Only providers carrying an `api` base URL are usable: review-engine's
//! `ProviderRegistry` treats every non-Anthropic provider as an
//! OpenAI-compatible HTTP passthrough, so SDK-only catalog entries (no `api`
//! field) are filtered out everywhere via [`usable_providers`].

pub mod client;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use client::CatalogClient;

/// Result alias for catalog operations.
pub type Result<T> = std::result::Result<T, CatalogError>;

/// Error type for the catalog subsystem.
///
/// Kept local to `catalog` (rather than extending `crate::error`) so the
/// module stays self-contained — mirrors `upgrade::UpgradeError`.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// Transport-level failure (DNS, connect, read, TLS, timeout).
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// HTTP 4xx/5xx from the catalog endpoint.
    #[error("models.dev API returned {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body (best effort; may be truncated/empty).
        body: String,
    },

    /// Filesystem I/O error (disk cache).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Canonical models.dev API base (the catalog document lives at `{base}/api.json`).
pub const DEFAULT_API_BASE: &str = "https://models.dev";

/// Env override for the catalog API base (tests, mirrors) — mirrors the
/// `REVIEW_UPGRADE_API_BASE` precedent in the upgrade flow.
pub const API_BASE_ENV: &str = "REVIEW_MODELS_DEV_API_BASE";

/// Env override for the disk cache file location — mirrors the
/// `REVIEW_FEEDBACK_PATH` / `REVIEW_DISPATCH_STATE` precedent.
pub const CACHE_PATH_ENV: &str = "REVIEW_MODELS_DEV_CACHE";

/// Disk cache filename under `~/.config/review-engine/`.
const CACHE_FILE_NAME: &str = "models-dev-cache.json";

/// The parsed catalog: provider id → provider entry.
pub type Catalog = BTreeMap<String, CatalogProvider>;

/// `npm` package values whose providers speak the OpenAI-compatible HTTP API
/// and therefore need the `/v1` suffix on their base URL.
pub const OPENAI_COMPATIBLE_NPMS: &[&str] = &["@ai-sdk/openai-compatible", "@ai-sdk/openai"];

/// One provider entry in the models.dev catalog.
///
/// Unknown fields are ignored by serde (no `deny_unknown_fields`): the
/// catalog evolves independently of this binary, and every field is optional
/// in practice even though the schema documents them as present.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogProvider {
    /// Provider id (e.g. `"deepseek"`).
    #[serde(default)]
    pub id: String,
    /// Human-readable name (e.g. `"DeepSeek"`).
    #[serde(default)]
    pub name: String,
    /// ai-sdk npm package (e.g. `"@ai-sdk/openai-compatible"`); drives the
    /// `/v1` suffix decision in [`normalize_api_base`].
    #[serde(default)]
    pub npm: Option<String>,
    /// OpenAI-compatible HTTP base URL; `None` = SDK-only provider (excluded).
    #[serde(default)]
    pub api: Option<String>,
    /// Environment variables the provider reads credentials from.
    #[serde(default)]
    pub env: Vec<String>,
    /// Pricing/docs URL.
    #[serde(default)]
    pub doc: Option<String>,
    /// Model id → model entry.
    #[serde(default)]
    pub models: BTreeMap<String, CatalogModel>,
}

/// One model entry inside a [`CatalogProvider`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogModel {
    /// Model id (e.g. `"deepseek-chat"`).
    #[serde(default)]
    pub id: String,
    /// Human-readable name (e.g. `"DeepSeek Chat"`).
    #[serde(default)]
    pub name: String,
    /// Context/output token limits.
    #[serde(default)]
    pub limit: Option<ModelLimit>,
    /// Per-token pricing (kept for future cost display; extra keys ignored).
    #[serde(default)]
    pub cost: Option<ModelCost>,
    /// Whether the model performs chain-of-thought reasoning.
    #[serde(default)]
    pub reasoning: Option<bool>,
    /// Whether the model supports tool/function calling.
    #[serde(default)]
    pub tool_call: Option<bool>,
    /// Input/output modalities.
    #[serde(default)]
    pub modalities: Option<ModelModalities>,
}

/// Token limits for a model.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelLimit {
    /// Total context window in tokens.
    #[serde(default)]
    pub context: Option<u64>,
    /// Maximum output tokens.
    #[serde(default)]
    pub output: Option<u64>,
}

/// Per-million-token pricing. Only the fields the UI may display are
/// modelled; the catalog carries more (cache read/write, etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCost {
    /// Input cost per million tokens.
    #[serde(default)]
    pub input: Option<f64>,
    /// Output cost per million tokens.
    #[serde(default)]
    pub output: Option<f64>,
}

/// Model input/output modalities.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelModalities {
    /// Accepted input modalities (e.g. `["text", "image"]`).
    #[serde(default)]
    pub input: Vec<String>,
    /// Produced output modalities.
    #[serde(default)]
    pub output: Vec<String>,
}

// ─── Disk cache ─────────────────────────────────────────────────

/// On-disk cache document: the parsed catalog plus when it was fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskCache {
    /// When the catalog was successfully fetched from the network.
    pub fetched_at: DateTime<Utc>,
    /// The cached catalog payload.
    pub providers: Catalog,
}

/// Disk cache location: `REVIEW_MODELS_DEV_CACHE` or
/// `~/.config/review-engine/models-dev-cache.json`. `None` when no home
/// directory is resolvable (degraded environments run cache-less).
pub fn default_cache_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(CACHE_PATH_ENV) {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    home::home_dir().map(|dir| dir.join(".config").join("review-engine").join(CACHE_FILE_NAME))
}

/// Load the disk cache. Missing files yield `None`; corrupt files are
/// ignored with a warn log (mirrors `feedback::load_entries`).
pub fn load_disk_cache(path: &Path) -> Option<DiskCache> {
    let content = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&content) {
        Ok(cache) => Some(cache),
        Err(e) => {
            tracing::warn!("Catalog: ignoring corrupt cache file {}: {e}", path.display());
            None
        }
    }
}

/// Persist the catalog to `path` atomically via a temp file + rename, so a
/// crash mid-write never leaves a truncated cache behind (mirrors
/// `feedback::write_entries_atomic`).
pub fn write_disk_cache(path: &Path, catalog: &Catalog) -> std::io::Result<()> {
    let cache = DiskCache {
        fetched_at: Utc::now(),
        providers: catalog.clone(),
    };
    let json = serde_json::to_string(&cache).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ─── Fetch with fallback ────────────────────────────────────────

/// Where a resolved catalog came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSource {
    /// Freshly fetched from the network (and written to the disk cache).
    Network,
    /// Served from the stale disk cache after a fetch failure; carries the
    /// original fetch timestamp.
    DiskCache(DateTime<Utc>),
}

/// Fetch the catalog via `client`, falling back to the stale disk cache on
/// failure. A successful fetch is persisted to `cache_path` (best effort —
/// write failures only log). Shared by the server endpoints (which add an
/// in-memory TTL layer on top) and the CLI `init` flow.
pub async fn fetch_or_disk_fallback(
    client: &CatalogClient,
    cache_path: Option<&Path>,
) -> Result<(Catalog, CatalogSource)> {
    match client.fetch_catalog().await {
        Ok(catalog) => {
            if let Some(path) = cache_path {
                if let Err(e) = write_disk_cache(path, &catalog) {
                    tracing::warn!("Catalog: failed to write disk cache {}: {e}", path.display());
                }
            }
            Ok((catalog, CatalogSource::Network))
        }
        Err(e) => match cache_path.and_then(load_disk_cache) {
            Some(disk) => {
                tracing::warn!(
                    "Catalog: fetch failed ({e}); serving stale disk cache from {}",
                    disk.fetched_at
                );
                Ok((disk.providers, CatalogSource::DiskCache(disk.fetched_at)))
            }
            None => Err(e),
        },
    }
}

// ─── Pure helpers ───────────────────────────────────────────────

/// Providers usable through review-engine's OpenAI-compatible passthrough:
/// those carrying an `api` base URL, sorted by display name (stable on id).
pub fn usable_providers(catalog: &Catalog) -> Vec<&CatalogProvider> {
    let mut providers: Vec<&CatalogProvider> = catalog.values().filter(|p| p.api.is_some()).collect();
    providers.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    providers
}

/// A provider's models sorted by display name (stable on id).
pub fn sorted_models(provider: &CatalogProvider) -> Vec<&CatalogModel> {
    let mut models: Vec<&CatalogModel> = provider.models.values().collect();
    models.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    models
}

/// Normalize a catalog `api` base URL for review-engine's provider registry.
///
/// Many catalog `api` values lack the `/v1` suffix that OpenAI-compatible
/// chat-completions calls need. When the provider's `npm` package marks it as
/// OpenAI-compatible (`@ai-sdk/openai-compatible` / `@ai-sdk/openai`), append
/// `/v1` unless already present. Anything else (Anthropic's native API,
/// unknown packages) is passed through with only trailing slashes trimmed —
/// `provider = "anthropic"` is special-cased downstream in
/// `ProviderRegistry::from_configs` and takes no `/v1`.
pub fn normalize_api_base(npm: Option<&str>, api: &str) -> String {
    let trimmed = api.trim_end_matches('/');
    if let Some(npm) = npm {
        if OPENAI_COMPATIBLE_NPMS.contains(&npm) && !trimmed.ends_with("/v1") {
            return format!("{trimmed}/v1");
        }
    }
    trimmed.to_string()
}
