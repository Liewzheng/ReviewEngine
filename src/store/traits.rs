//! Domain traits: `ReviewStore` (reviews / expert_reports / review_contexts),
//! `ConfigStore` (git_platforms / llm_providers / app_settings),
//! `DiscussionStore` (mr_discussions).
//!
//! `ConfigStore` landed in step 2, `ReviewStore` in step 4 of the 0.10.0
//! persistence rollout; `DiscussionStore` lands with its implementation.
//!
//! Semantics follow `UiStateFile` (design/persistence.md §4.2, §6.2): each
//! domain is read and replaced AS A WHOLE — `PUT /config` resolves the full
//! intended set in memory first, then persists it atomically. Per-row partial
//! updates are deliberately not offered.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::api::config::persist::{PersistedGitlabConfig, UiStateFile};
use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState};

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

/// Persistence boundary for review tasks (`reviews` + `expert_reports`).
///
/// Write-through contract (design/persistence.md §5): the in-memory
/// `TaskStore` stays the hot path / SSE source; every lifecycle transition
/// is mirrored here synchronously so the DB is the source of truth for
/// history across restarts. Callers treat failures as non-fatal (log and
/// continue) — a missing history row must never block a review.
#[async_trait]
pub trait ReviewStore: Send + Sync {
    /// INSERT a new `reviews` row from the freshly created entry
    /// (`state = 'pending'`, `request` / `source_meta` serialized).
    async fn create(&self, entry: &TaskEntry) -> Result<()>;

    /// `UPDATE state='running', started_at=?` for a task claimed by a worker.
    async fn mark_started(&self, task_id: Uuid, started_at: DateTime<Utc>) -> Result<()>;

    /// `UPDATE source_meta=?` plus the materialized `project` / `repository`
    /// filter columns (§3.2: read path must never touch JSON extraction).
    async fn fill_source_meta(&self, task_id: Uuid, meta: &SourceMeta) -> Result<()>;

    /// Terminal write, in ONE transaction: `UPDATE reviews` with
    /// state/result/error/completed_at/progress, then replace the task's
    /// `expert_reports` rows (delete + re-INSERT per `result.reports` entry,
    /// so a retried-then-completed task cannot hit the PK).
    async fn complete(&self, entry: &TaskEntry) -> Result<()>;

    /// `UPDATE state='cancelled', completed_at=?` (cancel semantics of
    /// `TaskStore::delete`).
    async fn mark_cancelled(&self, task_id: Uuid, completed_at: DateTime<Utc>) -> Result<()>;

    /// `UPDATE state='pending', error=NULL, completed_at=NULL` (retry:
    /// `Failed → Pending`).
    async fn mark_retry(&self, task_id: Uuid) -> Result<()>;

    /// Startup sweep (§5.3): every row still `pending` / `running` when the
    /// previous process died becomes `failed` with
    /// `error='interrupted: server restarted'`. Returns affected rows.
    async fn mark_interrupted(&self, now: DateTime<Utc>) -> Result<u64>;

    /// History list (§8.1): newest first (`ORDER BY created_at DESC`),
    /// paginated, plus the total row count under the same filters. The DB is
    /// the only data source — in-flight tasks are present via write-through,
    /// no memory merge.
    async fn list_reviews(&self, query: &ReviewListQuery) -> Result<(Vec<TaskEntry>, u64)>;

    /// Single history row; `None` when the task is unknown.
    async fn get_review(&self, task_id: Uuid) -> Result<Option<TaskEntry>>;
}

/// Handler-normalized history-list parameters (design/persistence.md §8.1 —
/// the DB takes over what `TaskStore::list` filtered in memory in 0.9):
/// `page` is 1-based (≥ 1), `per_page` is already clamped to ≤ 100.
#[derive(Debug, Clone, Default)]
pub struct ReviewListQuery {
    pub status: Option<TaskState>,
    pub page: u64,
    pub per_page: u64,
    pub q: Option<String>,
    pub project: Option<String>,
    pub repository: Option<String>,
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
}

/// One MR discussion note (`mr_discussions` row, design/persistence.md
/// §3.2). The primary key `(platform, project, mr_iid, note_id)` is the
/// idempotency key: webhook redelivery dedups, note edits update in place.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscussionNote {
    /// `GitPlatformConfig.name` of the instance the note came from
    /// ("default" when no platform matched the payload).
    pub platform: String,
    /// `project.path_with_namespace`.
    pub project: String,
    pub mr_iid: u64,
    pub note_id: u64,
    pub author: String,
    pub body: String,
    /// The note's own creation time (from the webhook payload), NOT the
    /// ingestion time.
    pub created_at: DateTime<Utc>,
}

/// Persistence boundary for MR discussion notes (design/persistence.md
/// §7.1). Written by the Note webhook handler; read by the review-time
/// context injection (§7.2, step 6b).
#[async_trait]
pub trait DiscussionStore: Send + Sync {
    /// Idempotent upsert on `(platform, project, mr_iid, note_id)`:
    /// redelivery dedups; an edited note updates `body` / `author`.
    async fn upsert_note(&self, note: &DiscussionNote) -> Result<()>;

    /// All notes of one MR, ordered `(created_at, note_id)` ascending — the
    /// append-only order the context-injection renderer (§7.2) relies on for
    /// prefix-stable output.
    async fn list_notes(&self, platform: &str, project: &str, mr_iid: u64) -> Result<Vec<DiscussionNote>>;
}
