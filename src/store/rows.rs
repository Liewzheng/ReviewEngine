//! Row structure ⇄ domain structure codecs for the configuration domain.
//!
//! This module is the `enc:` encryption boundary (design/persistence.md
//! §4.1): domain values are live plaintext, row values are the at-rest form.
//! Encrypted at rest: `git_platforms.token / webhook_secret /
//! webhook_signing_secret`, `llm_providers.api_key` (newly inside the
//! boundary — 0.9 stored it plaintext), and each field of the legacy
//! `gitlab` settings JSON. Empty strings stay empty (never encrypted).
//! Values read back WITHOUT the `enc:` prefix are legacy plaintext and pass
//! through unchanged (`decrypt_secret`'s existing semantics).

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::secrets::{decrypt_secret, encrypt_secret};
use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::api::config::persist::PersistedGitlabConfig;

/// At-rest form of one `git_platforms` row.
#[derive(Debug)]
pub(crate) struct GitPlatformRow {
    pub id: String,
    pub name: String,
    pub platform_type: String,
    pub base_url: String,
    pub internal_base_url: String,
    pub token: String,
    pub webhook_secret: String,
    pub webhook_signing_secret: String,
    pub enabled: bool,
    /// JSON fallback bag for non-columnized fields (`allowed_projects`).
    pub raw: String,
    pub updated_at: String,
}

/// At-rest form of one `llm_providers` row.
#[derive(Debug)]
pub(crate) struct LlmProviderRow {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub api_base: String,
    pub api_key: String,
    pub max_tokens: i64,
    pub temperature: f64,
    /// JSON fallback bag: `disable_thinking`, plus `position` — the list
    /// index, because provider order is semantically meaningful (first entry
    /// is the fallback primary) and the table has no sequence column.
    pub raw: String,
    pub updated_at: String,
}

fn encrypt_non_empty(value: &str, key: &[u8; 32]) -> Result<String> {
    if value.is_empty() {
        Ok(String::new())
    } else {
        encrypt_secret(value, key)
    }
}

pub(crate) fn git_platform_to_row(
    platform: &GitPlatformConfig,
    id: String,
    updated_at: String,
    key: &[u8; 32],
) -> Result<GitPlatformRow> {
    // `enabled` has no domain counterpart yet (GitPlatformConfig carries no
    // such field); the column is future-proofing and always written TRUE.
    let raw = if platform.allowed_projects.is_empty() {
        json!({})
    } else {
        json!({ "allowed_projects": platform.allowed_projects })
    };
    Ok(GitPlatformRow {
        id,
        name: platform.name.clone(),
        platform_type: platform.platform_type.clone(),
        base_url: platform.base_url.clone(),
        internal_base_url: platform.internal_base_url.clone(),
        token: encrypt_non_empty(&platform.token, key)?,
        webhook_secret: encrypt_non_empty(&platform.webhook_secret, key)?,
        webhook_signing_secret: encrypt_non_empty(&platform.webhook_signing_secret, key)?,
        enabled: true,
        raw: raw.to_string(),
        updated_at,
    })
}

pub(crate) fn git_platform_from_row(row: GitPlatformRow, key: &[u8; 32]) -> Result<GitPlatformConfig> {
    let raw: Value = serde_json::from_str(&row.raw)
        .with_context(|| format!("git_platforms row {:?} has invalid raw JSON", row.name))?;
    let allowed_projects = raw
        .get("allowed_projects")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(GitPlatformConfig {
        name: row.name,
        platform_type: row.platform_type,
        base_url: row.base_url,
        internal_base_url: row.internal_base_url,
        token: decrypt_secret(&row.token, key)?,
        webhook_secret: decrypt_secret(&row.webhook_secret, key)?,
        webhook_signing_secret: decrypt_secret(&row.webhook_signing_secret, key)?,
        allowed_projects,
    })
}

pub(crate) fn llm_to_row(
    config: &LLMConfig,
    position: usize,
    id: String,
    updated_at: String,
    key: &[u8; 32],
) -> Result<LlmProviderRow> {
    let mut raw = json!({ "position": position as i64 });
    if let Some(disable_thinking) = config.disable_thinking {
        raw["disable_thinking"] = json!(disable_thinking);
    }
    Ok(LlmProviderRow {
        id,
        provider: config.provider.clone(),
        model: config.model.clone(),
        api_base: config.api_base.clone(),
        api_key: encrypt_non_empty(&config.api_key, key)?,
        max_tokens: i64::from(config.max_tokens),
        temperature: f64::from(config.temperature),
        raw: raw.to_string(),
        updated_at,
    })
}

/// List position recorded by [`llm_to_row`]; `None` for rows written by
/// other means (sorts after positioned rows).
pub(crate) fn llm_row_position(row: &LlmProviderRow) -> Option<i64> {
    serde_json::from_str::<Value>(&row.raw).ok()?.get("position")?.as_i64()
}

pub(crate) fn llm_from_row(row: LlmProviderRow, key: &[u8; 32]) -> Result<LLMConfig> {
    let raw: Value = serde_json::from_str(&row.raw)
        .with_context(|| format!("llm_providers row {:?} has invalid raw JSON", row.provider))?;
    let disable_thinking = raw.get("disable_thinking").and_then(Value::as_bool);
    Ok(LLMConfig {
        provider: row.provider,
        model: row.model,
        api_key: decrypt_secret(&row.api_key, key)?,
        api_base: row.api_base,
        max_tokens: u32::try_from(row.max_tokens)
            .with_context(|| format!("llm_providers.max_tokens out of range: {}", row.max_tokens))?,
        temperature: row.temperature as f32,
        disable_thinking,
    })
}

/// Legacy GitLab credentials ⇄ the `app_settings` row at key `gitlab`.
/// Each field is individually `enc:`-encrypted inside the JSON (§3.2 note).
pub(crate) fn legacy_gitlab_to_value(gitlab: &PersistedGitlabConfig, key: &[u8; 32]) -> Result<Value> {
    Ok(json!({
        "token": encrypt_non_empty(&gitlab.token, key)?,
        "webhook_secret": encrypt_non_empty(&gitlab.webhook_secret, key)?,
        "webhook_signing_secret": encrypt_non_empty(&gitlab.webhook_signing_secret, key)?,
    }))
}

pub(crate) fn legacy_gitlab_from_value(value: &Value, key: &[u8; 32]) -> Result<PersistedGitlabConfig> {
    let field = |name: &str| -> Result<String> {
        match value.get(name).and_then(Value::as_str) {
            Some(s) => decrypt_secret(s, key),
            None => Ok(String::new()),
        }
    };
    Ok(PersistedGitlabConfig {
        token: field("token")?,
        webhook_secret: field("webhook_secret")?,
        webhook_signing_secret: field("webhook_signing_secret")?,
    })
}

// ─── Review domain (step 4): reviews / expert_reports ⇄ TaskEntry ───

use crate::server::task_queue::{SourceMeta, TaskEntry, TaskState};
use crate::store::{decode_ts, encode_ts};
use uuid::Uuid;

/// At-rest form of one `reviews` row (design/persistence.md §3.2). JSON
/// columns (`source_meta`, `request`, `result`) are serialized TEXT;
/// timestamps are RFC 3339 UTC strings via `encode_ts` / `decode_ts`.
#[derive(Debug)]
pub(crate) struct ReviewRow {
    pub task_id: String,
    pub state: String,
    pub source_meta: String,
    /// Materialized filter columns kept in sync with `source_meta` (§3.2).
    pub project: Option<String>,
    pub repository: Option<String>,
    pub request: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub progress: Option<i64>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

fn opt_json(value: &Option<Value>, what: &str) -> Result<Option<String>> {
    value
        .as_ref()
        .map(|v| serde_json::to_string(v).with_context(|| format!("serialize {what}")))
        .transpose()
}

/// `TaskState` → the `reviews.state` string. Single source of truth is the
/// API projection mapping (`task_status_str`); the store reuses it so the
/// DB vocabulary can never drift from the SSE / API vocabulary (§5.3).
pub(crate) fn task_state_str(state: &TaskState) -> &'static str {
    crate::server::api::review::task_status_str(state)
}

pub(crate) fn task_state_from_str(s: &str) -> Result<TaskState> {
    match s {
        "pending" => Ok(TaskState::Pending),
        "running" => Ok(TaskState::Running),
        "completed" => Ok(TaskState::Completed),
        "failed" => Ok(TaskState::Failed),
        "cancelled" => Ok(TaskState::Cancelled),
        other => anyhow::bail!("unknown reviews.state value: {other:?}"),
    }
}

pub(crate) fn encode_source_meta(meta: &SourceMeta) -> Result<String> {
    serde_json::to_string(meta).context("serialize source_meta")
}

pub(crate) fn decode_source_meta(raw: &str) -> Result<SourceMeta> {
    serde_json::from_str(raw).with_context(|| format!("reviews.source_meta holds invalid JSON: {raw:?}"))
}

pub(crate) fn task_entry_to_row(entry: &TaskEntry) -> Result<ReviewRow> {
    Ok(ReviewRow {
        task_id: entry.task_id.to_string(),
        state: task_state_str(&entry.state).to_string(),
        source_meta: encode_source_meta(&entry.source_meta)?,
        project: entry.source_meta.project.clone(),
        repository: entry.source_meta.repository.clone(),
        request: opt_json(&entry.request, "reviews.request")?,
        result: opt_json(&entry.result, "reviews.result")?,
        error: entry.error.clone(),
        progress: entry.progress.map(i64::from),
        created_at: encode_ts(&entry.created_at),
        started_at: entry.started_at.as_ref().map(encode_ts),
        completed_at: entry.completed_at.as_ref().map(encode_ts),
    })
}

/// Column list of the shared `reviews` SELECT used by the read path
/// (`sqlx.rs`); the order matches [`ReviewRowTuple`].
pub(crate) const REVIEW_COLUMNS: &str = "task_id, state, source_meta, project, repository, request, \
     result, error, progress, created_at, started_at, completed_at";

/// Raw decode target for a `SELECT {REVIEW_COLUMNS}` query, in column order.
#[allow(clippy::type_complexity)]
pub(crate) type ReviewRowTuple = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    String,
    Option<String>,
    Option<String>,
);

impl From<ReviewRowTuple> for ReviewRow {
    fn from(
        (
            task_id,
            state,
            source_meta,
            project,
            repository,
            request,
            result,
            error,
            progress,
            created_at,
            started_at,
            completed_at,
        ): ReviewRowTuple,
    ) -> Self {
        Self {
            task_id,
            state,
            source_meta,
            project,
            repository,
            request,
            result,
            error,
            progress,
            created_at,
            started_at,
            completed_at,
        }
    }
}

/// `TaskStore::fill_source_meta`'s blank definition: `None` or
/// whitespace-only. A blank projection field may take the materialized
/// column's value; a non-blank `source_meta` value always wins.
fn backfill_from_column(field: &mut Option<String>, column: Option<String>) {
    let blank = field.as_deref().map(str::trim).unwrap_or_default().is_empty();
    if blank {
        *field = column.filter(|v| !v.trim().is_empty());
    }
}

/// Decode a `reviews` row back into a [`TaskEntry`]. Used by the history
/// read path (`ReviewStore::list_reviews` / `get_review`, §8.1) and by tests.
///
/// Projection reads `source_meta` (the full metadata JSON); the materialized
/// `project`/`repository` columns exist for indexed filtering (§5.2 keeps
/// them in sync on every write). If a row has nevertheless drifted — a
/// failed `fill_source_meta` UPDATE is only logged, never retried, and
/// hand-seeded/legacy rows bypass the codec — the column is the last copy
/// of the value, so blank JSON fields are back-filled from it: a row that
/// matches `?project=X` must never display a blank project.
pub(crate) fn review_from_row(row: ReviewRow) -> Result<TaskEntry> {
    fn opt_ts(raw: Option<String>, what: &str) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        raw.as_deref()
            .map(|s| decode_ts(s).with_context(|| format!("reviews.{what}")))
            .transpose()
    }
    let state = task_state_from_str(&row.state)?;
    let mut source_meta = decode_source_meta(&row.source_meta)?;
    backfill_from_column(&mut source_meta.project, row.project);
    backfill_from_column(&mut source_meta.repository, row.repository);
    Ok(TaskEntry {
        task_id: Uuid::parse_str(&row.task_id)
            .with_context(|| format!("reviews.task_id is not a UUID: {:?}", row.task_id))?,
        state,
        created_at: decode_ts(&row.created_at).context("reviews.created_at")?,
        started_at: opt_ts(row.started_at, "started_at")?,
        completed_at: opt_ts(row.completed_at, "completed_at")?,
        result: row
            .result
            .map(|s| serde_json::from_str(&s).context("reviews.result holds invalid JSON"))
            .transpose()?,
        error: row.error,
        request: row
            .request
            .map(|s| serde_json::from_str(&s).context("reviews.request holds invalid JSON"))
            .transpose()?,
        source_meta,
        progress: row
            .progress
            .map(|p| u8::try_from(p).with_context(|| format!("reviews.progress out of range: {p}")))
            .transpose()?,
        // Live-only fields: the DB is the history source, the in-memory
        // `expert_name` (current active expert) is not persisted.
        expert_name: None,
    })
}

/// At-rest form of one `expert_reports` row.
#[derive(Debug)]
pub(crate) struct ExpertReportRow {
    pub task_id: String,
    pub expert_name: String,
    pub report: String,
    /// Per-expert duration: always NULL for now — `TaskEntry` does not track
    /// it yet (design/persistence.md §5.4 note).
    pub duration_ms: Option<i64>,
    pub created_at: String,
}

/// Split a serialized `ReviewOutput` (`reviews.result`) into one
/// `expert_reports` row per `reports[]` entry.
pub(crate) fn expert_report_rows(task_id: &Uuid, result: &Value, created_at: String) -> Result<Vec<ExpertReportRow>> {
    let output: crate::models::ReviewOutput =
        serde_json::from_value(result.clone()).context("reviews.result is not a serialized ReviewOutput")?;
    output
        .reports
        .iter()
        .map(|report| {
            Ok(ExpertReportRow {
                task_id: task_id.to_string(),
                expert_name: report.expert_name.clone(),
                report: serde_json::to_string(report)
                    .with_context(|| format!("serialize expert report {:?}", report.expert_name))?,
                duration_ms: None,
                created_at: created_at.clone(),
            })
        })
        .collect()
}

// ─── Discussion domain (step 6a): mr_discussions ⇄ DiscussionNote ───

use crate::store::traits::DiscussionNote;

/// Raw decode target for `SELECT platform, project, mr_iid, note_id, author,
/// body, created_at FROM mr_discussions`, in column order.
pub(crate) type DiscussionRowTuple = (String, String, i64, i64, String, String, String);

fn u64_from_i64(value: i64, what: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("mr_discussions.{what} out of range: {value}"))
}

/// `DiscussionNote.mr_iid` / `note_id` as bindable i64 (BIGINT columns).
pub(crate) fn discussion_ids(note: &DiscussionNote) -> Result<(i64, i64)> {
    Ok((
        i64::try_from(note.mr_iid).with_context(|| format!("mr_iid out of range: {}", note.mr_iid))?,
        i64::try_from(note.note_id).with_context(|| format!("note_id out of range: {}", note.note_id))?,
    ))
}

pub(crate) fn discussion_from_row(
    (platform, project, mr_iid, note_id, author, body, created_at): DiscussionRowTuple,
) -> Result<DiscussionNote> {
    Ok(DiscussionNote {
        platform,
        project,
        mr_iid: u64_from_i64(mr_iid, "mr_iid")?,
        note_id: u64_from_i64(note_id, "note_id")?,
        author,
        body,
        created_at: decode_ts(&created_at).context("mr_discussions.created_at")?,
    })
}

// ─── tests ───

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal decodable completed row; individual fields are overridden per
    /// test.
    fn review_row(source_meta: &str, project: Option<&str>, repository: Option<&str>) -> ReviewRow {
        ReviewRow {
            task_id: Uuid::new_v4().to_string(),
            state: "completed".to_string(),
            source_meta: source_meta.to_string(),
            project: project.map(str::to_string),
            repository: repository.map(str::to_string),
            request: None,
            result: None,
            error: None,
            progress: Some(100),
            created_at: "2026-09-03T01:00:00.000000Z".to_string(),
            started_at: Some("2026-09-03T01:00:01.000000Z".to_string()),
            completed_at: Some("2026-09-03T01:00:42.000000Z".to_string()),
        }
    }

    /// §8.1 projection semantics (E2E-A 观察点 4): the materialized
    /// project/repository columns exist for filtering, `source_meta` is the
    /// projection source — but a drifted row (column set, JSON blank) must
    /// not lose the value the filter matched on.
    #[test]
    fn review_from_row_backfills_blank_meta_from_materialized_columns() {
        let entry = review_from_row(review_row("{}", Some("grp/proj"), Some("grp/proj"))).unwrap();
        assert_eq!(entry.source_meta.project.as_deref(), Some("grp/proj"));
        assert_eq!(entry.source_meta.repository.as_deref(), Some("grp/proj"));
    }

    /// A non-blank `source_meta` value is authoritative; the column is only
    /// a fallback and never clobbers it.
    #[test]
    fn review_from_row_source_meta_wins_over_materialized_columns() {
        let meta = r#"{"project":"json/wins","repository":"json-repo"}"#;
        let entry = review_from_row(review_row(meta, Some("grp/proj"), Some("grp/proj"))).unwrap();
        assert_eq!(entry.source_meta.project.as_deref(), Some("json/wins"));
        assert_eq!(entry.source_meta.repository.as_deref(), Some("json-repo"));
    }

    /// Both sides blank stays absent (no empty-string fabrication), and a
    /// whitespace-only JSON value counts as blank.
    #[test]
    fn review_from_row_backfill_never_fabricates_values() {
        let entry = review_from_row(review_row("{}", None, None)).unwrap();
        assert!(entry.source_meta.project.is_none());
        assert!(entry.source_meta.repository.is_none());

        let meta = r#"{"project":"   "}"#;
        let entry = review_from_row(review_row(meta, Some("grp/proj"), Some(""))).unwrap();
        assert_eq!(entry.source_meta.project.as_deref(), Some("grp/proj"));
        assert!(
            entry.source_meta.repository.is_none(),
            "blank column must not back-fill"
        );
    }
}
