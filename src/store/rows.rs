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
use std::collections::BTreeMap;

use crate::config::secrets::{decrypt_secret, encrypt_secret};
use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::api::config::persist::PersistedGitlabConfig;
use crate::store::traits::ProviderUsageStats;

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
    /// JSON fallback bag: `disable_thinking`, `disabled` (RENG-75: the
    /// provider is kept but excluded from the chain and never probed), plus
    /// `position` — the index of this provider in the STORED list. Order is
    /// semantically meaningful (RENG-55: the list order is the fallback order
    /// behind the persisted primary, which is recorded separately as
    /// `ui.llm.primaryProvider`) and the table has no sequence column.
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
    // RENG-75: only written when true, so rows saved by older versions (no
    // key at all) and enabled providers share the exact same shape.
    if config.disabled {
        raw["disabled"] = json!(true);
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
        // Absent (every pre-RENG-75 row) means enabled.
        disabled: raw.get("disabled").and_then(Value::as_bool).unwrap_or(false),
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
    /// RENG-38: deduplicated `[{provider, model}]` JSON snapshot of the LLMs
    /// that produced this review (`ReviewOutput::llm_usages`), materialized
    /// at write time so the history list never parses `result` (§8.1).
    pub llm_summary: Option<String>,
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

/// RENG-38: compute the `reviews.llm_summary` TEXT (JSON array of
/// [`crate::models::LlmUsage`]) from a serialized `ReviewOutput` result.
/// `None` when the result is absent, not a `ReviewOutput`, or carries no
/// llm snapshots (pre-0.10.2 records, all-experts-failed runs) — a
/// non-ReviewOutput result is a legitimate shape (`complete` only warns and
/// skips the expert_reports split), never a store error.
///
/// RENG-75: each entry also carries `fp`, the serving card's entry
/// fingerprint. `LlmUsage::fp` is `skip_serializing` (the API can never leak
/// it), so the column writer emits it EXPLICITLY here — this function is the
/// only place a fingerprint is ever serialized.
pub(crate) fn llm_summary_json(result: &Value) -> Option<String> {
    let output: crate::models::ReviewOutput = serde_json::from_value(result.clone()).ok()?;
    let usages = output.llm_usages();
    if usages.is_empty() {
        return None;
    }
    let entries: Vec<Value> = usages
        .iter()
        .map(|u| {
            let mut entry = json!({ "provider": u.provider, "model": u.model });
            if let Some(fp) = &u.fp {
                entry["fp"] = json!(fp);
            }
            entry
        })
        .collect();
    serde_json::to_string(&entries).ok()
}

/// `TaskState` → the `reviews.state` string. Single source of truth is the
/// API projection mapping (`task_status_str`); the store reuses it so the
/// DB vocabulary can never drift from the SSE / API vocabulary (§5.3).
pub(crate) fn task_state_str(state: &TaskState) -> &'static str {
    crate::server::api::review::task_status_str(state)
}

/// One aggregated input row of [`aggregate_llm_usage`]:
/// `(task_id, state, created_at, llm_summary)` as selected by
/// [`ReviewStore::llm_usage_since`](super::traits::ReviewStore::llm_usage_since).
pub(crate) type LlmUsageRowTuple = (String, String, String, Option<String>);

/// RENG-56: fold `(task_id, state, created_at, llm_summary)` rows into
/// per-provider usage, ordered by provider name.
///
/// RENG-75 revision: the fold key is the `(provider, model, fp)` triple —
/// the entry fingerprint the snapshot recorded (`None` for pre-RENG-75 rows,
/// the unmarked bucket the API layer attributes by its own rule) — so two
/// same-named cards each get their own numbers. One review still contributes
/// at most one usage per triple, and the buckets come back in
/// `(provider, model, fp)` order (the `BTreeMap` key order, `None` first).
///
/// Review-level granularity: a triple is counted once per review that
/// recorded it (the report dimension lives in `expert_reports`, not in the
/// history contract the page is cross-checked against). `cancelled` /
/// `pending` / `running` rows count as usage but land in neither outcome
/// bucket — a user-cancelled review is not a provider failure.
///
/// Rows without a summary, with a blank provider, or with unparsable JSON are
/// skipped (the latter logged): a single unreadable row must not take the
/// whole LLM page down, and the write path guarantees the JSON shape.
pub(crate) fn aggregate_llm_usage(rows: impl IntoIterator<Item = LlmUsageRowTuple>) -> Vec<ProviderUsageStats> {
    let mut by_entry: BTreeMap<(String, String, Option<String>), ProviderUsageStats> = BTreeMap::new();
    for (task_id, state, created_at, summary) in rows {
        let Some(summary) = summary else { continue };
        let usages: Vec<crate::models::LlmUsage> = match serde_json::from_str(&summary) {
            Ok(usages) => usages,
            Err(e) => {
                tracing::warn!("review {task_id}: unreadable llm_summary JSON ({e}); skipped in the usage aggregate");
                continue;
            }
        };
        // One review contributes at most one usage per (provider, model, fp).
        let mut triples: Vec<(String, String, Option<String>)> = Vec::new();
        for usage in &usages {
            if usage.provider.is_empty() {
                continue;
            }
            let triple = (usage.provider.clone(), usage.model.clone(), usage.fp.clone());
            if !triples.contains(&triple) {
                triples.push(triple);
            }
        }
        let used_at = super::decode_ts(&created_at).ok();
        for (provider, model, fp) in triples {
            let entry = by_entry
                .entry((provider.clone(), model.clone(), fp.clone()))
                .or_insert_with(|| ProviderUsageStats {
                    provider,
                    model,
                    fp,
                    usage_count: 0,
                    completed_count: 0,
                    failed_count: 0,
                    last_used_at: None,
                });
            entry.usage_count += 1;
            match state.as_str() {
                "completed" => entry.completed_count += 1,
                "failed" => entry.failed_count += 1,
                _ => {}
            }
            if let Some(ts) = used_at {
                if entry.last_used_at.is_none_or(|prev| ts > prev) {
                    entry.last_used_at = Some(ts);
                }
            }
        }
    }
    by_entry.into_values().collect()
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
        // The entry's live field wins (filled on terminal update); when it is
        // absent (e.g. the create-time write-through) compute from `result`.
        llm_summary: entry
            .llm_summary
            .clone()
            .or_else(|| entry.result.as_ref().and_then(llm_summary_json)),
        created_at: encode_ts(&entry.created_at),
        started_at: entry.started_at.as_ref().map(encode_ts),
        completed_at: entry.completed_at.as_ref().map(encode_ts),
    })
}

/// Column list of the shared `reviews` SELECT used by the read path
/// (`sqlx.rs`); the order matches [`ReviewRowTuple`].
pub(crate) const REVIEW_COLUMNS: &str = "task_id, state, source_meta, project, repository, request, \
     result, error, progress, llm_summary, created_at, started_at, completed_at";

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
    Option<String>,
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
            llm_summary,
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
            llm_summary,
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
        llm_summary: row.llm_summary,
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
    /// RENG-38: LLM name snapshots denormalized from the report JSON so the
    /// per-expert provider/model is queryable without parsing `report`.
    /// NULL for pre-0.10.2 rows and non-LLM reports.
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
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
                llm_provider: report.llm_provider.clone(),
                llm_model: report.llm_model.clone(),
                created_at: created_at.clone(),
            })
        })
        .collect()
}

// ─── Discussion domain (step 6a): mr_discussions ⇄ DiscussionNote ───

use crate::store::traits::DiscussionNote;

/// Raw decode target for `SELECT platform, project, mr_iid, note_id, author,
/// author_id, author_avatar_url, author_bot, body, created_at FROM
/// mr_discussions`, in column order. `author_id` is the TEXT-stored provider
/// id; `author_bot` is the INTEGER 0/1 bool.
pub(crate) type DiscussionRowTuple = (
    String,
    String,
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    i64,
    String,
    String,
);

fn u64_from_i64(value: i64, what: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("mr_discussions.{what} out of range: {value}"))
}

/// `DiscussionNote.mr_iid` / `note_id` as bindable i64 (BIGINT columns), and
/// the author id as the decimal string the TEXT column stores.
pub(crate) fn discussion_binds(note: &DiscussionNote) -> Result<(i64, i64, Option<String>)> {
    Ok((
        i64::try_from(note.mr_iid).with_context(|| format!("mr_iid out of range: {}", note.mr_iid))?,
        i64::try_from(note.note_id).with_context(|| format!("note_id out of range: {}", note.note_id))?,
        note.author_id.map(|id| id.to_string()),
    ))
}

pub(crate) fn discussion_from_row(
    (platform, project, mr_iid, note_id, author, author_id, author_avatar_url, author_bot, body, created_at): DiscussionRowTuple,
) -> Result<DiscussionNote> {
    let author_id = author_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u64>()
                .with_context(|| format!("mr_discussions.author_id is not a u64: {s:?}"))
        })
        .transpose()?;
    Ok(DiscussionNote {
        platform,
        project,
        mr_iid: u64_from_i64(mr_iid, "mr_iid")?,
        note_id: u64_from_i64(note_id, "note_id")?,
        author,
        author_id,
        author_avatar_url: author_avatar_url.filter(|u| !u.trim().is_empty()),
        author_bot: author_bot != 0,
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
            llm_summary: None,
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

    // ─── RENG-56: llm usage aggregation ─────────────────────────────

    fn usage_row(task: &str, state: &str, created_at: &str, summary: Option<&str>) -> LlmUsageRowTuple {
        (
            task.to_string(),
            state.to_string(),
            created_at.to_string(),
            summary.map(str::to_string),
        )
    }

    /// Several entries and mixed states aggregate into per-TRIPLE buckets
    /// (RENG-75: `(provider, model, fp)`, `None` fp first): outcome splits
    /// and the newest timestamp each entry was used.
    #[test]
    fn aggregate_llm_usage_counts_reviews_per_entry() {
        let stats = aggregate_llm_usage(vec![
            usage_row(
                "t1",
                "completed",
                "2026-09-08T10:00:00.000000Z",
                Some(r#"[{"provider":"xiaomi","model":"mimo-v2.5","fp":"fp-x1"}]"#),
            ),
            usage_row(
                "t2",
                "completed",
                "2026-09-09T10:00:00.000000Z",
                // Two models on one provider plus a second card: three
                // triples, one usage each (same pair twice would be ONE).
                Some(
                    r#"[{"provider":"xiaomi","model":"mimo-v2.5","fp":"fp-x1"},{"provider":"deepseek","model":"deepseek-v4","fp":"fp-d1"},{"provider":"xiaomi","model":"mimo-v2-pro","fp":"fp-x1"}]"#,
                ),
            ),
            usage_row(
                "t3",
                "failed",
                "2026-09-10T10:00:00.000000Z",
                Some(r#"[{"provider":"deepseek","model":"deepseek-v4","fp":"fp-d1"}]"#),
            ),
            // No summary (failed before any report) — invisible, by design.
            usage_row("t4", "failed", "2026-09-11T10:00:00.000000Z", None),
            // Cancelled counts as usage but in neither outcome bucket.
            usage_row(
                "t5",
                "cancelled",
                "2026-09-12T10:00:00.000000Z",
                Some(r#"[{"provider":"deepseek","model":"deepseek-v4","fp":"fp-d1"}]"#),
            ),
        ]);

        assert_eq!(
            stats
                .iter()
                .map(|s| (s.provider.as_str(), s.model.as_str(), s.fp.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("deepseek", "deepseek-v4", Some("fp-d1")),
                ("xiaomi", "mimo-v2-pro", Some("fp-x1")),
                ("xiaomi", "mimo-v2.5", Some("fp-x1")),
            ],
            "buckets come back in (provider, model, fp) order"
        );
        let deepseek = &stats[0];
        assert_eq!(deepseek.usage_count, 3, "one usage per review per triple");
        assert_eq!(deepseek.completed_count, 1);
        assert_eq!(deepseek.failed_count, 1);
        assert_eq!(
            deepseek.last_used_at.unwrap().to_rfc3339(),
            "2026-09-12T10:00:00+00:00",
            "the newest usage wins, cancelled reviews included"
        );

        let pro = &stats[1];
        assert_eq!(pro.usage_count, 1, "a second model is its own bucket");
        let xiaomi = &stats[2];
        assert_eq!(xiaomi.usage_count, 2);
        assert_eq!(xiaomi.completed_count, 2);
        assert_eq!(xiaomi.failed_count, 0);
        assert_eq!(xiaomi.last_used_at.unwrap().to_rfc3339(), "2026-09-09T10:00:00+00:00");
    }

    /// RENG-75: two same-(provider, model) cards with different fingerprints
    /// are two buckets; pre-fingerprint rows (`fp` absent) form the unmarked
    /// `None` bucket alongside them.
    #[test]
    fn aggregate_llm_usage_separates_fingerprints_and_the_unmarked_bucket() {
        let stats = aggregate_llm_usage(vec![
            usage_row(
                "t1",
                "completed",
                "2026-09-08T10:00:00.000000Z",
                Some(r#"[{"provider":"acme","model":"m1","fp":"fp-a"}]"#),
            ),
            usage_row(
                "t2",
                "completed",
                "2026-09-09T10:00:00.000000Z",
                Some(r#"[{"provider":"acme","model":"m1","fp":"fp-b"}]"#),
            ),
            // Pre-RENG-75 row: no fp key → the unmarked bucket.
            usage_row(
                "t3",
                "completed",
                "2026-09-10T10:00:00.000000Z",
                Some(r#"[{"provider":"acme","model":"m1"}]"#),
            ),
        ]);

        assert_eq!(
            stats
                .iter()
                .map(|s| (s.provider.as_str(), s.model.as_str(), s.fp.as_deref(), s.usage_count))
                .collect::<Vec<_>>(),
            vec![
                ("acme", "m1", None, 1),
                ("acme", "m1", Some("fp-a"), 1),
                ("acme", "m1", Some("fp-b"), 1),
            ],
            "one bucket per fingerprint plus the unmarked one (None sorts first)"
        );
    }

    /// Rows the aggregate must not invent usage from: no summary, empty
    /// array, blank provider names, and unreadable JSON.
    #[test]
    fn aggregate_llm_usage_skips_rows_without_a_usable_provider() {
        let stats = aggregate_llm_usage(vec![
            usage_row("t1", "completed", "2026-09-08T10:00:00.000000Z", None),
            usage_row("t2", "completed", "2026-09-08T10:00:00.000000Z", Some("[]")),
            usage_row(
                "t3",
                "completed",
                "2026-09-08T10:00:00.000000Z",
                Some(r#"[{"provider":"","model":"m"}]"#),
            ),
            usage_row("t4", "completed", "2026-09-08T10:00:00.000000Z", Some("not json")),
        ]);
        assert!(stats.is_empty(), "no provider was recorded: {stats:?}");
    }

    /// A row whose `created_at` cannot be decoded still counts as usage; it
    /// simply cannot claim the `last_used_at` slot.
    #[test]
    fn aggregate_llm_usage_keeps_counts_with_an_undecodable_timestamp() {
        let stats = aggregate_llm_usage(vec![
            usage_row(
                "t1",
                "completed",
                "not-a-timestamp",
                Some(r#"[{"provider":"xiaomi","model":"m"}]"#),
            ),
            usage_row(
                "t2",
                "completed",
                "2026-09-09T10:00:00.000000Z",
                Some(r#"[{"provider":"xiaomi","model":"m"}]"#),
            ),
        ]);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].usage_count, 2);
        assert_eq!(stats[0].last_used_at.unwrap().to_rfc3339(), "2026-09-09T10:00:00+00:00");
    }

    /// An empty window yields no providers at all — never a fabricated entry.
    #[test]
    fn aggregate_llm_usage_empty_window_is_empty() {
        assert!(aggregate_llm_usage(Vec::new()).is_empty());
    }
}
