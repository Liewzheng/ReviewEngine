//! `SqlxStore` implementations of the domain traits in [`crate::store::traits`].
//! All SQL lives in this file (design/persistence.md §4.1).
//!
//! Dialect discipline (§3.1): `?` placeholders only, no `RETURNING`, JSON as
//! bound `String`, timestamps via `encode_ts` / `decode_ts` (RFC 3339 TEXT).
//! Every statement passes through [`crate::store::adapt_sql`] exactly once
//! before construction: PostgreSQL gets `?` rewritten to `$1..$n` (the Any
//! driver passes SQL through verbatim), SQLite borrows the text unchanged.
//! NOTE: this file is itself named `sqlx.rs` — the sibling module shadows
//! the extern crate lexically, so every reference to the real sqlx crate
//! must use the absolute `::sqlx::` path.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::models::{GitPlatformConfig, LLMConfig};
use crate::server::api::config::persist::{PersistedGitlabConfig, UiStateFile};
use crate::server::task_queue::{SourceMeta, TaskEntry};

use super::rows;
use super::traits::{ConfigStore, DiscussionNote, DiscussionStore, ReviewListQuery, ReviewStore};
use super::{adapt_sql, encode_ts, BackendKind, SqlxStore};

const LEGACY_GITLAB_KEY: &str = "gitlab";
const UI_KEY: &str = "ui";

type AnyTx<'a> = ::sqlx::Transaction<'a, ::sqlx::Any>;

/// DELETE + re-INSERT the whole git_platforms set inside `tx`.
async fn replace_git_platforms_in(
    tx: &mut AnyTx<'_>,
    kind: BackendKind,
    platforms: &[GitPlatformConfig],
    key: &[u8; 32],
) -> Result<()> {
    let now = encode_ts(&Utc::now());
    let delete = adapt_sql(kind, "DELETE FROM git_platforms");
    ::sqlx::query(&delete)
        .execute(&mut **tx)
        .await
        .context("clear git_platforms")?;
    let insert = adapt_sql(
        kind,
        "INSERT INTO git_platforms (id, name, type, base_url, internal_base_url, token, \
         webhook_secret, webhook_signing_secret, enabled, raw, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    for platform in platforms {
        let row = rows::git_platform_to_row(platform, uuid::Uuid::new_v4().to_string(), now.clone(), key)?;
        ::sqlx::query(&insert)
            .bind(&row.id)
            .bind(&row.name)
            .bind(&row.platform_type)
            .bind(&row.base_url)
            .bind(&row.internal_base_url)
            .bind(&row.token)
            .bind(&row.webhook_secret)
            .bind(&row.webhook_signing_secret)
            .bind(i64::from(row.enabled))
            .bind(&row.raw)
            .bind(&row.updated_at)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("insert git_platform {:?}", platform.name))?;
    }
    Ok(())
}

/// DELETE + re-INSERT the whole llm_providers set inside `tx`.
async fn replace_llm_providers_in(
    tx: &mut AnyTx<'_>,
    kind: BackendKind,
    providers: &[LLMConfig],
    key: &[u8; 32],
) -> Result<()> {
    let now = encode_ts(&Utc::now());
    let delete = adapt_sql(kind, "DELETE FROM llm_providers");
    ::sqlx::query(&delete)
        .execute(&mut **tx)
        .await
        .context("clear llm_providers")?;
    let insert = adapt_sql(
        kind,
        "INSERT INTO llm_providers (id, provider, model, api_base, api_key, max_tokens, \
         temperature, raw, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    for (position, config) in providers.iter().enumerate() {
        let row = rows::llm_to_row(config, position, uuid::Uuid::new_v4().to_string(), now.clone(), key)?;
        ::sqlx::query(&insert)
            .bind(&row.id)
            .bind(&row.provider)
            .bind(&row.model)
            .bind(&row.api_base)
            .bind(&row.api_key)
            .bind(row.max_tokens)
            .bind(row.temperature)
            .bind(&row.raw)
            .bind(&row.updated_at)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("insert llm_provider {:?}", config.provider))?;
    }
    Ok(())
}

/// Upsert one app_settings row inside `tx`. Syntax is shared by PG and
/// SQLite (≥3.24); no RETURNING.
async fn upsert_setting_in(tx: &mut AnyTx<'_>, kind: BackendKind, key: &str, value: &serde_json::Value) -> Result<()> {
    let upsert = adapt_sql(
        kind,
        "INSERT INTO app_settings (key, value, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    );
    ::sqlx::query(&upsert)
        .bind(key)
        .bind(value.to_string())
        .bind(encode_ts(&Utc::now()))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to save app_setting {key:?}"))?;
    Ok(())
}

async fn delete_setting_in(tx: &mut AnyTx<'_>, kind: BackendKind, key: &str) -> Result<()> {
    let delete = adapt_sql(kind, "DELETE FROM app_settings WHERE key = ?");
    ::sqlx::query(&delete)
        .bind(key)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("failed to delete app_setting {key:?}"))?;
    Ok(())
}

#[async_trait]
impl ConfigStore for SqlxStore {
    async fn load_git_platforms(&self) -> Result<Vec<GitPlatformConfig>> {
        let sql = self.sql(
            "SELECT id, name, type, base_url, internal_base_url, token, webhook_secret, \
             webhook_signing_secret, enabled, raw, updated_at FROM git_platforms ORDER BY name",
        );
        let rows = ::sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                i64,
                String,
                String,
            ),
        >(&sql)
        .fetch_all(self.pool())
        .await
        .context("failed to load git_platforms")?;
        rows.into_iter()
            .map(
                |(
                    id,
                    name,
                    platform_type,
                    base_url,
                    internal_base_url,
                    token,
                    webhook_secret,
                    webhook_signing_secret,
                    enabled,
                    raw,
                    updated_at,
                )| {
                    rows::git_platform_from_row(
                        rows::GitPlatformRow {
                            id,
                            name,
                            platform_type,
                            base_url,
                            internal_base_url,
                            token,
                            webhook_secret,
                            webhook_signing_secret,
                            // Any driver cannot decode SQLite BOOLEAN-declared
                            // columns; the column is INTEGER 0/1.
                            enabled: enabled != 0,
                            raw,
                            updated_at,
                        },
                        &self.key,
                    )
                },
            )
            .collect()
    }

    async fn replace_git_platforms(&self, platforms: &[GitPlatformConfig]) -> Result<()> {
        let mut tx = self.pool().begin().await.context("begin replace_git_platforms")?;
        replace_git_platforms_in(&mut tx, self.kind, platforms, &self.key).await?;
        tx.commit().await.context("commit replace_git_platforms")?;
        Ok(())
    }

    async fn load_llm_providers(&self) -> Result<Vec<LLMConfig>> {
        let sql = self.sql(
            "SELECT id, provider, model, api_base, api_key, max_tokens, temperature, raw, \
             updated_at FROM llm_providers ORDER BY provider",
        );
        let rows = ::sqlx::query_as::<_, (String, String, String, String, String, i64, f64, String, String)>(&sql)
            .fetch_all(self.pool())
            .await
            .context("failed to load llm_providers")?;
        let mut rows: Vec<rows::LlmProviderRow> = rows
            .into_iter()
            .map(
                |(id, provider, model, api_base, api_key, max_tokens, temperature, raw, updated_at)| {
                    rows::LlmProviderRow {
                        id,
                        provider,
                        model,
                        api_base,
                        api_key,
                        max_tokens,
                        temperature,
                        raw,
                        updated_at,
                    }
                },
            )
            .collect();
        // Stable sort by the recorded list position; rows without one keep
        // their deterministic `provider` order at the tail.
        rows.sort_by_key(|r| rows::llm_row_position(r).unwrap_or(i64::MAX));
        rows.into_iter().map(|r| rows::llm_from_row(r, &self.key)).collect()
    }

    async fn replace_llm_providers(&self, providers: &[LLMConfig]) -> Result<()> {
        let mut tx = self.pool().begin().await.context("begin replace_llm_providers")?;
        replace_llm_providers_in(&mut tx, self.kind, providers, &self.key).await?;
        tx.commit().await.context("commit replace_llm_providers")?;
        Ok(())
    }

    async fn load_legacy_gitlab(&self) -> Result<PersistedGitlabConfig> {
        match self.load_setting(LEGACY_GITLAB_KEY).await? {
            Some(value) => rows::legacy_gitlab_from_value(&value, &self.key),
            None => Ok(PersistedGitlabConfig::default()),
        }
    }

    async fn save_legacy_gitlab(&self, gitlab: &PersistedGitlabConfig) -> Result<()> {
        let value = rows::legacy_gitlab_to_value(gitlab, &self.key)?;
        self.save_setting(LEGACY_GITLAB_KEY, &value).await
    }

    async fn load_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let sql = self.sql("SELECT value FROM app_settings WHERE key = ?");
        let raw: Option<String> = ::sqlx::query_scalar(&sql)
            .bind(key)
            .fetch_optional(self.pool())
            .await
            .with_context(|| format!("failed to load app_setting {key:?}"))?;
        raw.map(|s| serde_json::from_str(&s).with_context(|| format!("app_setting {key:?} holds invalid JSON")))
            .transpose()
    }

    async fn save_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        let mut tx = self.pool().begin().await.context("begin save_setting")?;
        upsert_setting_in(&mut tx, self.kind, key, value).await?;
        tx.commit().await.context("commit save_setting")?;
        Ok(())
    }

    async fn save_ui_state(&self, state: &UiStateFile) -> Result<()> {
        let mut tx = self.pool().begin().await.context("begin save_ui_state")?;
        replace_git_platforms_in(&mut tx, self.kind, &state.git_platforms, &self.key).await?;
        replace_llm_providers_in(&mut tx, self.kind, &state.llm, &self.key).await?;
        let gitlab = &state.gitlab;
        if gitlab.token.is_empty() && gitlab.webhook_secret.is_empty() && gitlab.webhook_signing_secret.is_empty() {
            // Unset is unset: an all-empty legacy gitlab value removes the row
            // instead of storing an empty JSON shell.
            delete_setting_in(&mut tx, self.kind, LEGACY_GITLAB_KEY).await?;
        } else {
            let value = rows::legacy_gitlab_to_value(gitlab, &self.key)?;
            upsert_setting_in(&mut tx, self.kind, LEGACY_GITLAB_KEY, &value).await?;
        }
        if let Some(ui) = &state.ui {
            let value = serde_json::to_value(ui).context("serialize ui projection")?;
            upsert_setting_in(&mut tx, self.kind, UI_KEY, &value).await?;
        }
        tx.commit().await.context("commit save_ui_state")?;
        Ok(())
    }

    async fn config_tables_empty(&self) -> Result<bool> {
        let sql = self.sql(
            "SELECT (SELECT COUNT(*) FROM git_platforms), \
             (SELECT COUNT(*) FROM llm_providers), \
             (SELECT COUNT(*) FROM app_settings)",
        );
        let (gp, lp, st): (i64, i64, i64) = ::sqlx::query_as(&sql)
            .fetch_one(self.pool())
            .await
            .context("failed to count config tables")?;
        Ok(gp == 0 && lp == 0 && st == 0)
    }
}

// ─── ReviewStore (reviews / expert_reports, step 4) ───

/// Warn when a per-task UPDATE matched no row — the create write-through
/// must have failed earlier, so the task's history is already lost; the
/// warning keeps that visible without failing the review path.
fn warn_missing_row(op: &str, task_id: &uuid::Uuid, rows_affected: u64) {
    if rows_affected == 0 {
        tracing::warn!("{op}: no reviews row for task {task_id} (earlier write-through presumably failed)");
    }
}

#[async_trait]
impl ReviewStore for SqlxStore {
    async fn create(&self, entry: &TaskEntry) -> Result<()> {
        let row = rows::task_entry_to_row(entry)?;
        let sql = self.sql(
            "INSERT INTO reviews (task_id, state, source_meta, project, repository, request, \
             result, error, progress, created_at, started_at, completed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        );
        ::sqlx::query(&sql)
            .bind(&row.task_id)
            .bind(&row.state)
            .bind(&row.source_meta)
            .bind(&row.project)
            .bind(&row.repository)
            .bind(&row.request)
            .bind(&row.result)
            .bind(&row.error)
            .bind(row.progress)
            .bind(&row.created_at)
            .bind(&row.started_at)
            .bind(&row.completed_at)
            .execute(self.pool())
            .await
            .with_context(|| format!("insert review {}", row.task_id))?;
        Ok(())
    }

    async fn mark_started(&self, task_id: uuid::Uuid, started_at: DateTime<Utc>) -> Result<()> {
        let sql = self.sql("UPDATE reviews SET state = 'running', started_at = ? WHERE task_id = ?");
        let res = ::sqlx::query(&sql)
            .bind(encode_ts(&started_at))
            .bind(task_id.to_string())
            .execute(self.pool())
            .await
            .with_context(|| format!("mark review {task_id} started"))?;
        warn_missing_row("mark_started", &task_id, res.rows_affected());
        Ok(())
    }

    async fn fill_source_meta(&self, task_id: uuid::Uuid, meta: &SourceMeta) -> Result<()> {
        let sql = self.sql("UPDATE reviews SET source_meta = ?, project = ?, repository = ? WHERE task_id = ?");
        let res = ::sqlx::query(&sql)
            .bind(rows::encode_source_meta(meta)?)
            .bind(&meta.project)
            .bind(&meta.repository)
            .bind(task_id.to_string())
            .execute(self.pool())
            .await
            .with_context(|| format!("fill source_meta for review {task_id}"))?;
        warn_missing_row("fill_source_meta", &task_id, res.rows_affected());
        Ok(())
    }

    async fn complete(&self, entry: &TaskEntry) -> Result<()> {
        let row = rows::task_entry_to_row(entry)?;
        let report_created_at = row.completed_at.clone().unwrap_or_else(|| encode_ts(&Utc::now()));
        let mut tx = self.pool().begin().await.context("begin complete review")?;
        let update = self.sql(
            "UPDATE reviews SET state = ?, result = ?, error = ?, completed_at = ?, progress = ? \
             WHERE task_id = ?",
        );
        let res = ::sqlx::query(&update)
            .bind(&row.state)
            .bind(&row.result)
            .bind(&row.error)
            .bind(&row.completed_at)
            .bind(row.progress)
            .bind(&row.task_id)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("complete review {}", row.task_id))?;
        warn_missing_row("complete", &entry.task_id, res.rows_affected());
        // Replace (not upsert) so a retried-then-completed task cannot hit
        // the (task_id, expert_name) PK with stale rows.
        let delete = self.sql("DELETE FROM expert_reports WHERE task_id = ?");
        ::sqlx::query(&delete)
            .bind(&row.task_id)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("clear expert_reports for {}", row.task_id))?;
        if let Some(result) = &entry.result {
            match rows::expert_report_rows(&entry.task_id, result, report_created_at) {
                Ok(report_rows) => {
                    let insert = self.sql(
                        "INSERT INTO expert_reports (task_id, expert_name, report, duration_ms, created_at) \
                         VALUES (?, ?, ?, ?, ?)",
                    );
                    for report in &report_rows {
                        ::sqlx::query(&insert)
                            .bind(&report.task_id)
                            .bind(&report.expert_name)
                            .bind(&report.report)
                            .bind(report.duration_ms)
                            .bind(&report.created_at)
                            .execute(&mut *tx)
                            .await
                            .with_context(|| {
                                format!("insert expert_report {:?} for {}", report.expert_name, report.task_id)
                            })?;
                    }
                }
                // A result that is not a serialized ReviewOutput is not a
                // transient failure — keep the terminal review row, drop the
                // per-expert split.
                Err(e) => tracing::warn!(
                    "could not split expert reports for task {}: {e:#}; storing the review row only",
                    entry.task_id
                ),
            }
        }
        tx.commit()
            .await
            .with_context(|| format!("commit complete review {}", row.task_id))?;
        Ok(())
    }

    async fn mark_cancelled(&self, task_id: uuid::Uuid, completed_at: DateTime<Utc>) -> Result<()> {
        let sql = self.sql("UPDATE reviews SET state = 'cancelled', completed_at = ? WHERE task_id = ?");
        let res = ::sqlx::query(&sql)
            .bind(encode_ts(&completed_at))
            .bind(task_id.to_string())
            .execute(self.pool())
            .await
            .with_context(|| format!("mark review {task_id} cancelled"))?;
        warn_missing_row("mark_cancelled", &task_id, res.rows_affected());
        Ok(())
    }

    async fn mark_retry(&self, task_id: uuid::Uuid) -> Result<()> {
        let sql = self.sql("UPDATE reviews SET state = 'pending', error = NULL, completed_at = NULL WHERE task_id = ?");
        let res = ::sqlx::query(&sql)
            .bind(task_id.to_string())
            .execute(self.pool())
            .await
            .with_context(|| format!("mark review {task_id} retried"))?;
        warn_missing_row("mark_retry", &task_id, res.rows_affected());
        Ok(())
    }

    async fn mark_interrupted(&self, now: DateTime<Utc>) -> Result<u64> {
        let sql = self.sql(
            "UPDATE reviews SET state = 'failed', error = 'interrupted: server restarted', completed_at = ? \
             WHERE state IN ('pending', 'running')",
        );
        let res = ::sqlx::query(&sql)
            .bind(encode_ts(&now))
            .execute(self.pool())
            .await
            .context("interrupted-task sweep failed")?;
        Ok(res.rows_affected())
    }

    async fn list_reviews(&self, query: &ReviewListQuery) -> Result<(Vec<TaskEntry>, u64)> {
        let (where_sql, binds) = review_where(query);

        let count_raw = format!("SELECT COUNT(*) FROM reviews {where_sql}");
        let count_sql = self.sql(&count_raw);
        let mut count_q = ::sqlx::query_scalar::<_, i64>(&count_sql);
        for value in &binds {
            count_q = count_q.bind(value);
        }
        let total = count_q.fetch_one(self.pool()).await.context("count reviews")?;

        // Every bind value in `binds` is a String (state/q/project/repository
        // and both timestamps), so the COUNT and the page SELECT share the
        // same positional parameter list; LIMIT/OFFSET trail as two more.
        let offset = query.page.saturating_sub(1).saturating_mul(query.per_page);
        let list_raw = format!(
            "SELECT {} FROM reviews {where_sql} ORDER BY created_at DESC, task_id DESC LIMIT ? OFFSET ?",
            rows::REVIEW_COLUMNS
        );
        let list_sql = self.sql(&list_raw);
        let mut list_q = ::sqlx::query_as::<_, rows::ReviewRowTuple>(&list_sql);
        for value in &binds {
            list_q = list_q.bind(value);
        }
        let rows = list_q
            .bind(query.per_page as i64)
            .bind(offset as i64)
            .fetch_all(self.pool())
            .await
            .context("list reviews")?;
        let entries = rows
            .into_iter()
            .map(|tuple| rows::review_from_row(tuple.into()))
            .collect::<Result<Vec<_>>>()
            .context("decode reviews rows")?;
        Ok((entries, total as u64))
    }

    async fn get_review(&self, task_id: uuid::Uuid) -> Result<Option<TaskEntry>> {
        let raw = format!("SELECT {} FROM reviews WHERE task_id = ?", rows::REVIEW_COLUMNS);
        let sql = self.sql(&raw);
        let row = ::sqlx::query_as::<_, rows::ReviewRowTuple>(&sql)
            .bind(task_id.to_string())
            .fetch_optional(self.pool())
            .await
            .with_context(|| format!("load review {task_id}"))?;
        row.map(|tuple| rows::review_from_row(tuple.into()))
            .transpose()
            .with_context(|| format!("decode review row {task_id}"))
    }

    async fn upsert_review_context(
        &self,
        task_id: uuid::Uuid,
        kind: &str,
        content: &str,
        content_hash: &str,
        token_estimate: i64,
    ) -> Result<()> {
        let sql = self.sql(
            "INSERT INTO review_contexts (task_id, kind, content, content_hash, token_estimate, created_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (task_id, kind) DO UPDATE SET \
             content = excluded.content, content_hash = excluded.content_hash, \
             token_estimate = excluded.token_estimate",
        );
        ::sqlx::query(&sql)
            .bind(task_id.to_string())
            .bind(kind)
            .bind(content)
            .bind(content_hash)
            .bind(token_estimate)
            .bind(encode_ts(&Utc::now()))
            .execute(self.pool())
            .await
            .with_context(|| format!("upsert review_context {kind} for {task_id}"))?;
        Ok(())
    }
}

// ─── DiscussionStore (mr_discussions, step 6a) ───

#[async_trait]
impl DiscussionStore for SqlxStore {
    async fn upsert_note(&self, note: &DiscussionNote) -> Result<()> {
        let (mr_iid, note_id) = rows::discussion_ids(note)?;
        let sql = self.sql(
            "INSERT INTO mr_discussions (platform, project, mr_iid, note_id, author, body, created_at, ingested_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (platform, project, mr_iid, note_id) DO UPDATE SET \
             body = excluded.body, author = excluded.author",
        );
        ::sqlx::query(&sql)
            .bind(&note.platform)
            .bind(&note.project)
            .bind(mr_iid)
            .bind(note_id)
            .bind(&note.author)
            .bind(&note.body)
            .bind(encode_ts(&note.created_at))
            .bind(encode_ts(&Utc::now()))
            .execute(self.pool())
            .await
            .with_context(|| {
                format!(
                    "upsert mr_discussion note {} for {} !{}",
                    note.note_id, note.project, note.mr_iid
                )
            })?;
        Ok(())
    }

    async fn list_notes(&self, platform: &str, project: &str, mr_iid: u64) -> Result<Vec<DiscussionNote>> {
        let mr_iid = i64::try_from(mr_iid).with_context(|| format!("mr_iid out of range: {mr_iid}"))?;
        let sql = self.sql(
            "SELECT platform, project, mr_iid, note_id, author, body, created_at FROM mr_discussions \
             WHERE platform = ? AND project = ? AND mr_iid = ? ORDER BY created_at, note_id",
        );
        let rows = ::sqlx::query_as::<_, rows::DiscussionRowTuple>(&sql)
            .bind(platform)
            .bind(project)
            .bind(mr_iid)
            .fetch_all(self.pool())
            .await
            .with_context(|| format!("list mr_discussions for {project} !{mr_iid}"))?;
        rows.into_iter()
            .map(rows::discussion_from_row)
            .collect::<Result<Vec<_>>>()
            .context("decode mr_discussions rows")
    }
}

/// Shared WHERE clause + positional binds for the history list (§8.1). All
/// bind values are Strings so the COUNT and the page SELECT can share them.
///
/// `q` keeps the 0.9 semantics — a case-insensitive literal substring match
/// — applied to the serialized `source_meta` TEXT (design prescribes
/// `LOWER(...) LIKE LOWER(?)`; LIKE wildcards in the needle are escaped so
/// the match stays literal).
fn review_where(query: &ReviewListQuery) -> (String, Vec<String>) {
    let mut clauses: Vec<&str> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    if let Some(status) = &query.status {
        clauses.push("state = ?");
        binds.push(rows::task_state_str(status).to_string());
    }
    if let Some(q) = &query.q {
        clauses.push("LOWER(source_meta) LIKE LOWER(?) ESCAPE '\\'");
        let needle = q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        binds.push(format!("%{needle}%"));
    }
    if let Some(project) = &query.project {
        clauses.push("project = ?");
        binds.push(project.clone());
    }
    if let Some(repository) = &query.repository {
        clauses.push("repository = ?");
        binds.push(repository.clone());
    }
    if let Some(from) = &query.date_from {
        // Fixed-width RFC 3339 UTC: lexicographic compare == chronological
        // compare (§3.1 timestamp row).
        clauses.push("created_at >= ?");
        binds.push(encode_ts(from));
    }
    if let Some(to) = &query.date_to {
        clauses.push("created_at <= ?");
        binds.push(encode_ts(to));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_sql, binds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::decode_ts;
    use chrono::{SubsecRound, TimeZone};

    async fn fresh_store() -> SqlxStore {
        let store = SqlxStore::new_in_memory().await.unwrap();
        store.migrate().await.unwrap();
        store
    }

    fn sample_platforms() -> Vec<GitPlatformConfig> {
        vec![
            GitPlatformConfig {
                name: "internal".into(),
                platform_type: "gitlab".into(),
                base_url: "https://gitlab.internal.example".into(),
                internal_base_url: "http://gitlab.svc:8080".into(),
                token: "glpat-internal-token".into(),
                webhook_secret: "wh-internal".into(),
                webhook_signing_secret: "whsec_internal".into(),
                allowed_projects: vec!["group/a".into(), "group/b".into()],
            },
            GitPlatformConfig {
                name: "public".into(),
                platform_type: "gitlab".into(),
                base_url: "https://gitlab.com".into(),
                ..Default::default()
            },
        ]
    }

    fn llm_eq(a: &LLMConfig, b: &LLMConfig) -> bool {
        // LLMConfig has no PartialEq (custom Debug masks the key); compare
        // field by field.
        a.provider == b.provider
            && a.model == b.model
            && a.api_key == b.api_key
            && a.api_base == b.api_base
            && a.max_tokens == b.max_tokens
            && a.temperature == b.temperature
            && a.disable_thinking == b.disable_thinking
    }

    #[tokio::test]
    async fn git_platforms_round_trip_with_encrypted_secrets() {
        let store = fresh_store().await;
        let platforms = sample_platforms();
        store.replace_git_platforms(&platforms).await.unwrap();

        // At rest: every secret column of the populated entry is `enc:`-prefixed.
        let (token, wh, whs, raw): (String, String, String, String) = ::sqlx::query_as(
            "SELECT token, webhook_secret, webhook_signing_secret, raw FROM git_platforms \
             WHERE name = 'internal'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert!(token.starts_with("enc:"), "token not encrypted: {token}");
        assert!(wh.starts_with("enc:"), "webhook_secret not encrypted");
        assert!(whs.starts_with("enc:"), "webhook_signing_secret not encrypted");
        assert!(!token.contains("glpat-internal-token"));
        let raw_json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(raw_json["allowed_projects"], serde_json::json!(["group/a", "group/b"]));

        // Read back: field-level equality, deterministic name order.
        let loaded = store.load_git_platforms().await.unwrap();
        let mut expected = platforms.clone();
        expected.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(loaded, expected);

        // Replace semantics: second replace swaps the whole set atomically.
        store.replace_git_platforms(&platforms[1..]).await.unwrap();
        let loaded = store.load_git_platforms().await.unwrap();
        assert_eq!(loaded, vec![platforms[1].clone()]);
    }

    #[tokio::test]
    async fn git_platforms_legacy_plaintext_passes_through() {
        let store = fresh_store().await;
        store.replace_git_platforms(&sample_platforms()).await.unwrap();
        // Simulate a legacy / hand-written plaintext secret in the DB.
        ::sqlx::query("UPDATE git_platforms SET token = 'plain-legacy-token' WHERE name = 'internal'")
            .execute(store.pool())
            .await
            .unwrap();
        let loaded = store.load_git_platforms().await.unwrap();
        let internal = loaded.iter().find(|p| p.name == "internal").unwrap();
        assert_eq!(internal.token, "plain-legacy-token");
    }

    #[tokio::test]
    async fn llm_providers_round_trip_with_encrypted_api_key_and_order() {
        let store = fresh_store().await;
        let providers = vec![
            LLMConfig {
                provider: "openai".into(),
                model: "gpt-5".into(),
                api_key: "sk-live-key".into(),
                api_base: "https://api.openai.com/v1".into(),
                max_tokens: 8192,
                temperature: 0.3,
                disable_thinking: None,
            },
            LLMConfig {
                provider: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                api_key: "ds-key".into(),
                api_base: "https://api.deepseek.com".into(),
                max_tokens: 4096,
                temperature: 0.7,
                disable_thinking: Some(true),
            },
        ];
        store.replace_llm_providers(&providers).await.unwrap();

        // At rest: api_key is `enc:`-prefixed (newly inside the encryption
        // boundary — 0.9 stored it plaintext).
        let (api_key, raw): (String, String) =
            ::sqlx::query_as("SELECT api_key, raw FROM llm_providers WHERE provider = 'openai'")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert!(
            api_key.starts_with("enc:"),
            "api_key not encrypted at rest (missing enc: prefix)"
        );
        assert!(!api_key.contains("sk-live-key"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&raw).unwrap()["position"], 0);

        // Read back: order preserved (openai first), field-level equality.
        let loaded = store.load_llm_providers().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(llm_eq(&loaded[0], &providers[0]), "entry 0 mismatch: {loaded:?}");
        assert!(llm_eq(&loaded[1], &providers[1]), "entry 1 mismatch: {loaded:?}");

        // Legacy plaintext api_key passes through on read.
        ::sqlx::query("UPDATE llm_providers SET api_key = 'plain-legacy-key' WHERE provider = 'openai'")
            .execute(store.pool())
            .await
            .unwrap();
        let loaded = store.load_llm_providers().await.unwrap();
        assert_eq!(loaded[0].api_key, "plain-legacy-key");
    }

    /// F1 regression gate on real PG: `temperature` is DOUBLE PRECISION
    /// (float8) and the store binds/decodes f64 — a float4 (REAL) column
    /// fails the read-back with `mismatched types`, which silently emptied
    /// GET /config after a restart. Requires `DATABASE_URL`:
    /// `DATABASE_URL=postgres://... cargo test store -- --ignored`
    #[tokio::test]
    #[ignore = "requires DATABASE_URL pointing at a scratch PostgreSQL"]
    async fn llm_providers_temperature_round_trip_on_postgres() {
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let store = SqlxStore::connect(&url).await.unwrap();
        store.migrate().await.unwrap();

        let providers = vec![
            LLMConfig {
                provider: "pg-f1-openai".into(),
                model: "gpt-5".into(),
                api_key: "sk-f1".into(),
                api_base: "https://api.openai.com/v1".into(),
                max_tokens: 8192,
                temperature: 0.3,
                disable_thinking: None,
            },
            LLMConfig {
                provider: "pg-f1-deepseek".into(),
                model: "deepseek-v4-flash".into(),
                api_key: "ds-f1".into(),
                api_base: "https://api.deepseek.com".into(),
                max_tokens: 4096,
                temperature: 0.7,
                disable_thinking: Some(true),
            },
        ];
        store.replace_llm_providers(&providers).await.unwrap();

        // The exact read path that failed on float4: f64 decode of the
        // temperature column.
        let loaded = store.load_llm_providers().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(llm_eq(&loaded[0], &providers[0]), "entry 0 mismatch: {loaded:?}");
        assert!(llm_eq(&loaded[1], &providers[1]), "entry 1 mismatch: {loaded:?}");

        // Clean up: the enc: rows are keyed by the runner's local
        // ~/.config secrets key and are undecryptable garbage for anyone
        // else sharing this scratch database.
        let cleanup = store.sql("DELETE FROM llm_providers WHERE provider IN ('pg-f1-openai', 'pg-f1-deepseek')");
        ::sqlx::query(&cleanup).execute(store.pool()).await.unwrap();
    }

    #[tokio::test]
    async fn legacy_gitlab_round_trip_with_encrypted_fields() {
        let store = fresh_store().await;

        // Missing row → all-empty default.
        let loaded = store.load_legacy_gitlab().await.unwrap();
        assert_eq!(loaded.token, "");
        assert_eq!(loaded.webhook_secret, "");
        assert_eq!(loaded.webhook_signing_secret, "");

        let gitlab = PersistedGitlabConfig {
            token: "glpat-legacy".into(),
            webhook_secret: "wh-legacy".into(),
            webhook_signing_secret: String::new(),
        };
        store.save_legacy_gitlab(&gitlab).await.unwrap();

        // At rest: every non-empty field inside the JSON is `enc:`-prefixed;
        // empty stays empty.
        let raw: String = ::sqlx::query_scalar("SELECT value FROM app_settings WHERE key = 'gitlab'")
            .fetch_one(store.pool())
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(value["token"].as_str().unwrap().starts_with("enc:"));
        assert!(value["webhook_secret"].as_str().unwrap().starts_with("enc:"));
        assert_eq!(value["webhook_signing_secret"], "");
        assert!(!raw.contains("glpat-legacy"));

        let loaded = store.load_legacy_gitlab().await.unwrap();
        assert_eq!(loaded.token, "glpat-legacy");
        assert_eq!(loaded.webhook_secret, "wh-legacy");
        assert_eq!(loaded.webhook_signing_secret, "");
    }

    #[tokio::test]
    async fn app_settings_arbitrary_json_round_trip() {
        let store = fresh_store().await;

        assert_eq!(store.load_setting("ui").await.unwrap(), None);

        let ui = serde_json::json!({
            "rules": {"maxFindings": 50},
            "advanced": {"parallelExperts": 4},
            "nested": {"list": [1, 2, 3], "flag": true}
        });
        store.save_setting("ui", &ui).await.unwrap();
        assert_eq!(store.load_setting("ui").await.unwrap(), Some(ui));

        // Upsert overwrites.
        let updated = serde_json::json!({"rules": {"maxFindings": 20}});
        store.save_setting("ui", &updated).await.unwrap();
        assert_eq!(store.load_setting("ui").await.unwrap(), Some(updated));

        // updated_at is a decodable RFC 3339 timestamp.
        let ts: String = ::sqlx::query_scalar("SELECT updated_at FROM app_settings WHERE key = 'ui'")
            .fetch_one(store.pool())
            .await
            .unwrap();
        decode_ts(&ts).unwrap();
    }

    #[tokio::test]
    async fn config_tables_empty_flag() {
        let store = fresh_store().await;
        assert!(store.config_tables_empty().await.unwrap());
        store.save_setting("ui", &serde_json::json!({})).await.unwrap();
        assert!(!store.config_tables_empty().await.unwrap());
    }

    // ─── ReviewStore (step 4) ───

    use crate::server::task_queue::{TaskEntry, TaskState};

    /// (d) the startup sweep flips ONLY pending/running rows to
    /// failed/interrupted; terminal rows are untouched.
    #[tokio::test]
    async fn mark_interrupted_sweeps_only_pending_and_running() {
        let store = fresh_store().await;
        let now = Utc::now();
        for (id, state) in [
            ("t-pending", "pending"),
            ("t-running", "running"),
            ("t-completed", "completed"),
            ("t-failed", "failed"),
            ("t-cancelled", "cancelled"),
        ] {
            ::sqlx::query("INSERT INTO reviews (task_id, state, created_at) VALUES (?, ?, ?)")
                .bind(id)
                .bind(state)
                .bind(encode_ts(&now))
                .execute(store.pool())
                .await
                .unwrap();
        }

        let swept = ReviewStore::mark_interrupted(&store, now).await.unwrap();
        assert_eq!(swept, 2, "only pending + running are swept");

        let rows: Vec<(String, String, Option<String>, Option<String>)> =
            ::sqlx::query_as("SELECT task_id, state, error, completed_at FROM reviews ORDER BY task_id")
                .fetch_all(store.pool())
                .await
                .unwrap();
        for (id, state, error, completed_at) in &rows {
            match id.as_str() {
                "t-pending" | "t-running" => {
                    assert_eq!(state, "failed");
                    assert_eq!(error.as_deref(), Some("interrupted: server restarted"));
                    assert!(completed_at.is_some(), "sweep stamps completed_at");
                }
                other => {
                    assert_eq!(state, &other[2..], "terminal row {other} must be untouched");
                    assert!(error.is_none() && completed_at.is_none());
                }
            }
        }
    }

    /// (task_id, state, source_meta, project, repository, request, result,
    /// error, progress, created_at, started_at, completed_at)
    type RawReviewRow = (
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

    /// reviews row codec: create → read raw columns → decode back to a
    /// TaskEntry that matches the original.
    #[tokio::test]
    async fn review_row_codec_round_trip() {
        let store = fresh_store().await;
        // Deterministic timestamp with sub-microsecond digits: `Utc::now()`
        // returns nanoseconds on Linux (clock_gettime) but only microseconds
        // on macOS (gettimeofday), which made this test platform-flaky.
        let created_at = Utc
            .with_ymd_and_hms(2026, 9, 4, 9, 34, 12)
            .unwrap()
            + chrono::Duration::nanoseconds(524_657_367);
        let entry = TaskEntry {
            task_id: uuid::Uuid::new_v4(),
            state: TaskState::Pending,
            created_at,
            started_at: None,
            completed_at: None,
            result: None,
            error: None,
            request: Some(serde_json::json!({"mr_url": "https://gitlab.example/g/p/-/merge_requests/7"})),
            source_meta: crate::server::task_queue::SourceMeta {
                mr_title: Some("Add login".into()),
                project: Some("g/p".into()),
                repository: Some("p".into()),
                ..Default::default()
            },
            progress: None,
            expert_name: None,
        };
        ReviewStore::create(&store, &entry).await.unwrap();

        let (
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
        ): RawReviewRow = ::sqlx::query_as(
            "SELECT task_id, state, source_meta, project, repository, request, result, error, \
             progress, created_at, started_at, completed_at FROM reviews WHERE task_id = ?",
        )
        .bind(entry.task_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();
        // Materialized filter columns are in sync with source_meta.
        assert_eq!(project.as_deref(), Some("g/p"));
        assert_eq!(repository.as_deref(), Some("p"));
        let decoded = rows::review_from_row(rows::ReviewRow {
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
        })
        .unwrap();
        assert_eq!(decoded.task_id, entry.task_id);
        assert_eq!(decoded.state, entry.state);
        assert_eq!(decoded.request, entry.request);
        assert_eq!(decoded.source_meta.mr_title.as_deref(), Some("Add login"));
        assert_eq!(decoded.source_meta.project.as_deref(), Some("g/p"));
        // Codec contract (encode_ts): timestamps are stored at microsecond
        // precision; sub-micro digits are truncated, not rounded.
        assert_ne!(
            entry.created_at.timestamp_subsec_nanos() % 1_000,
            0,
            "test input must carry sub-microsecond digits"
        );
        assert_eq!(decoded.created_at, entry.created_at.trunc_subsecs(6));
        assert!(decoded.expert_name.is_none(), "live-only field is not persisted");
    }

    // ─── DiscussionStore (step 6a) ───

    fn note(note_id: u64, created_at: &str, body: &str) -> DiscussionNote {
        DiscussionNote {
            platform: "default".into(),
            project: "group/proj".into(),
            mr_iid: 7,
            note_id,
            author: "alice".into(),
            body: body.into(),
            created_at: DateTime::parse_from_rfc3339(created_at).unwrap().with_timezone(&Utc),
        }
    }

    /// list_notes orders by (created_at, note_id) ascending — the
    /// append-only order §7.2's context renderer relies on.
    #[tokio::test]
    async fn discussion_notes_round_trip_and_ordering() {
        let store = fresh_store().await;
        // Insert out of order, including two notes sharing a timestamp
        // (note_id breaks the tie).
        store
            .upsert_note(&note(3, "2026-09-03T10:00:02Z", "third"))
            .await
            .unwrap();
        store
            .upsert_note(&note(1, "2026-09-03T10:00:00Z", "first"))
            .await
            .unwrap();
        store
            .upsert_note(&note(5, "2026-09-03T10:00:02Z", "fourth"))
            .await
            .unwrap();
        store
            .upsert_note(&note(2, "2026-09-03T10:00:00Z", "second"))
            .await
            .unwrap();
        // A different platform / MR must not leak into the result.
        let mut other = note(9, "2026-08-01T00:00:00Z", "other");
        other.platform = "public".into();
        store.upsert_note(&other).await.unwrap();

        let notes = store.list_notes("default", "group/proj", 7).await.unwrap();
        let bodies: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
        assert_eq!(bodies, vec!["first", "second", "third", "fourth"]);
        assert_eq!(notes[0].created_at.to_rfc3339(), "2026-09-03T10:00:00+00:00");

        // Edit via upsert keeps position, updates body.
        let mut edited = note(2, "2026-09-03T10:00:00Z", "second (edited)");
        edited.author = "bob".into();
        store.upsert_note(&edited).await.unwrap();
        let notes = store.list_notes("default", "group/proj", 7).await.unwrap();
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[1].body, "second (edited)");
        assert_eq!(notes[1].author, "bob");
    }
}
