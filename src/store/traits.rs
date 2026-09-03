//! Domain traits: `ReviewStore` (reviews / expert_reports / review_contexts),
//! `ConfigStore` (git_platforms / llm_providers / app_settings),
//! `DiscussionStore` (mr_discussions).
//!
//! Only `ConfigStore` is defined so far (step 2 of the 0.10.0 persistence
//! rollout); the other two land with their implementations.
//!
//! Semantics follow `UiStateFile` (design/persistence.md §4.2, §6.2): each
//! domain is read and replaced AS A WHOLE — `PUT /config` resolves the full
//! intended set in memory first, then persists it atomically. Per-row partial
//! updates are deliberately not offered.

use anyhow::Result;
use async_trait::async_trait;

use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::api::config::persist::{PersistedGitlabConfig, UiStateFile};

/// Persistence boundary for UI-managed configuration.
///
/// All values crossing this trait are LIVE (plaintext) domain values; the
/// `enc:` at-rest encryption happens inside the store implementation
/// (`rows.rs`), keyed by the per-config-dir `secrets.key`.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// All configured git platform instances (live secrets, deterministic
    /// order by `name`).
    async fn load_git_platforms(&self) -> Result<Vec<GitPlatformConfig>>;

    /// Atomically replace the whole git platform set (mirrors how
    /// `PUT /config` resolves and persists the full list). Empty slice =
    /// clear the table.
    async fn replace_git_platforms(&self, platforms: &[GitPlatformConfig]) -> Result<()>;

    /// All configured LLM providers (live API keys). List order is
    /// preserved: the first entry is the fallback primary provider
    /// (`sync_llm_projection`, persist.rs), so order is round-tripped via a
    /// `position` marker in the row's `raw` JSON.
    async fn load_llm_providers(&self) -> Result<Vec<LLMConfig>>;

    /// Atomically replace the whole LLM provider set.
    async fn replace_llm_providers(&self, providers: &[LLMConfig]) -> Result<()>;

    /// Legacy GitLab credentials (app_settings key `gitlab`). Missing row =
    /// all-empty default.
    async fn load_legacy_gitlab(&self) -> Result<PersistedGitlabConfig>;

    /// Persist legacy GitLab credentials. All three fields are individually
    /// `enc:`-encrypted inside the stored JSON (§3.2 note).
    async fn save_legacy_gitlab(&self, gitlab: &PersistedGitlabConfig) -> Result<()>;

    /// Arbitrary JSON setting from `app_settings` (e.g. the `ui` projection).
    async fn load_setting(&self, key: &str) -> Result<Option<serde_json::Value>>;

    /// Upsert an arbitrary JSON setting.
    async fn save_setting(&self, key: &str, value: &serde_json::Value) -> Result<()>;

    /// Atomically persist a whole [`UiStateFile`] snapshot in ONE
    /// transaction: git_platforms + llm_providers are replaced wholesale, the
    /// legacy `gitlab` settings row is upserted (an all-empty value deletes
    /// the row — unset is unset), and the `ui` projection is upserted when
    /// present (`None` leaves any existing `ui` row untouched).
    ///
    /// Used by the `PUT /config` save path (§6.2) and by the one-shot
    /// ui-state.toml import (§6.1): for the import, all-or-nothing is a hard
    /// requirement — a partial import must roll back so the next startup can
    /// retry against the still-present file.
    async fn save_ui_state(&self, state: &UiStateFile) -> Result<()>;

    /// True when git_platforms + llm_providers + app_settings are all empty
    /// — the trigger condition for the one-shot `ui-state.toml` import
    /// (design/persistence.md §6.1 step 3).
    async fn config_tables_empty(&self) -> Result<bool>;
}
