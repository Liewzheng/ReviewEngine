//! Pre-review MR discussion-context injection (design/persistence.md §7.2).
//!
//! Before the expert run starts, the review task loads the MR's discussion
//! history — DB-first (`mr_discussions`, fed by the Note webhook ingestion of
//! §7.1), falling back to the GitLab discussions API when the DB has nothing
//! (fresh instance, webhook not wired) and back-filling the DB from that
//! fetch. The notes are rendered into a fixed, prefix-stable markdown section
//! that is attached to `MRInfo::discussion_context` and injected into the
//! review user template between the fixed MR/project context and the diff.
//!
//! Degradation contract: EVERY failure path (DB error, API error, empty
//! result, oversized render, missing task row) logs and yields `None` — the
//! review then runs exactly as in 0.9. Injection must never fail a review.

use std::sync::Arc;

use anyhow::Result;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::server::gitlab::{
    is_command_note, is_self_report, parse_note_created_at, review_base_url, self_user_id_cached, url_origin,
};
use crate::store::traits::{DiscussionNote, DiscussionStore, ReviewStore};
use crate::store::SqlxStore;

/// `review_contexts.kind` value for the discussion-history section.
pub(crate) const DISCUSSION_KIND: &str = "mr_discussions";

/// Hard cap on the rendered section. Beyond this the prompt would drown the
/// diff; skip injection entirely (the review keeps full diff fidelity).
pub(crate) const MAX_CONTEXT_BYTES: usize = 128 * 1024;

/// Per-note body cap (chars, not bytes — note text is user-facing unicode).
const MAX_NOTE_BODY_CHARS: usize = 2000;

const SECTION_HEADER: &str = "## MR Discussion History\n\n";

/// Plumbing for one review task's discussion tap: the DB handle plus the
/// platform identity (`DiscussionNote.platform`) and the instance root used
/// for the API fallback and the self-echo guard. Cheap to clone; threaded
/// through the webhook dispatch chain as `Option<DiscussionTap>` (`None` =
/// no DB → 0.9 behaviour). `pub` only because the webhook dispatch fns are
/// `pub`; construction and use stay crate-internal.
#[derive(Clone)]
pub struct DiscussionTap {
    db: Arc<SqlxStore>,
    platform: String,
    instance_base: String,
}

impl DiscussionTap {
    /// Build a tap for `payload_url` (the MR URL as the review will fetch it).
    /// `platform`, when matched, fixes both the `platform` key (its `name`)
    /// and the reachable instance base (`internal_base_url` preferred);
    /// otherwise the key is `"default"` and the base is the URL's origin.
    pub(crate) fn new(
        db: Arc<SqlxStore>,
        platform: Option<&crate::models::GitPlatformConfig>,
        payload_url: &str,
    ) -> Self {
        let (platform, instance_base) = match platform {
            Some(p) => (p.name.clone(), review_base_url(p).to_string()),
            None => ("default".to_string(), url_origin(payload_url).unwrap_or_default()),
        };
        Self {
            db,
            platform,
            instance_base,
        }
    }

    /// Load, render, and persist the discussion section for one review task.
    /// `Some(section)` is attached to `MRInfo::discussion_context`; `None` =
    /// degrade to the 0.9 prompt. `task_id` must be a live `reviews` row
    /// (FK target of `review_contexts`); callers without a task store skip
    /// the tap entirely.
    pub(crate) async fn inject(
        &self,
        task_id: Uuid,
        project: &str,
        mr_iid: u64,
        gitlab_token: &str,
        mr_url: &str,
    ) -> Option<String> {
        let instance_base = self.instance_base.clone();
        let platform = self.platform.clone();
        let token = gitlab_token.to_string();
        let url = mr_url.to_string();
        let notes = load_discussion_notes(&self.db, &self.platform, project, mr_iid, move || async move {
            fetch_notes_via_api(&platform, &instance_base, project, mr_iid, &token, &url).await
        })
        .await?;
        if notes.is_empty() {
            return None;
        }
        let section = render_discussion_context(&notes);
        if section.len() > MAX_CONTEXT_BYTES {
            tracing::warn!(
                task_id = %task_id,
                bytes = section.len(),
                "MR discussion context exceeds {} bytes; skipping injection",
                MAX_CONTEXT_BYTES
            );
            return None;
        }
        // Persist the rendered context (content-addressed by sha256) so a
        // re-review of the same MR can detect reuse. Best-effort: the prompt
        // injection above must not depend on this write succeeding.
        let content_hash = sha256_hex(&section);
        let token_estimate = (section.len() / 4) as i64;
        if let Err(e) = self
            .db
            .upsert_review_context(task_id, DISCUSSION_KIND, &section, &content_hash, token_estimate)
            .await
        {
            tracing::warn!(task_id = %task_id, "failed to persist review_context {DISCUSSION_KIND}: {e:#}");
        }
        Some(section)
    }
}

/// Render notes (already ordered `(created_at, note_id)` ascending — the
/// `list_notes` contract) into the prompt section. Deterministic: identical
/// input yields byte-identical output, which is what makes `content_hash`
/// reuse detection meaningful. Bodies are truncated at
/// [`MAX_NOTE_BODY_CHARS`] chars.
pub(crate) fn render_discussion_context(notes: &[DiscussionNote]) -> String {
    let mut out = String::from(SECTION_HEADER);
    for note in notes {
        let body: String = note.body.chars().take(MAX_NOTE_BODY_CHARS).collect();
        out.push_str(&format!(
            "- [{} @ {}]: {}\n",
            note.author,
            crate::store::encode_ts(&note.created_at),
            body
        ));
    }
    out
}

pub(crate) fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// DB-first note load with an API fallback (§7.2): when `mr_discussions` has
/// rows for this MR they win (webhook-ingested history is authoritative and
/// free); when empty, `fallback` fetches from the provider API and every
/// fetched note is upserted (per-note failure is logged, not fatal) so the
/// next review is DB-served. `None` on any failure — never an error.
pub(crate) async fn load_discussion_notes<F, Fut>(
    db: &SqlxStore,
    platform: &str,
    project: &str,
    mr_iid: u64,
    fallback: F,
) -> Option<Vec<DiscussionNote>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<DiscussionNote>>>,
{
    let stored = match db.list_notes(platform, project, mr_iid).await {
        Ok(notes) => notes,
        Err(e) => {
            tracing::warn!("discussion tap: list_notes({platform}/{project} !{mr_iid}) failed: {e:#}");
            return None;
        }
    };
    if !stored.is_empty() {
        return Some(stored);
    }
    match fallback().await {
        Ok(mut notes) => {
            for note in &notes {
                if let Err(e) = db.upsert_note(note).await {
                    tracing::error!(
                        "discussion tap: failed to back-fill note {} for {platform}/{project} !{mr_iid}: {e:#}",
                        note.note_id
                    );
                }
            }
            // The API returns discussions in its own order; enforce the same
            // (created_at, note_id) order `list_notes` guarantees.
            notes.sort_by_key(|n| (n.created_at, n.note_id));
            Some(notes)
        }
        Err(e) => {
            tracing::warn!("discussion tap: API fallback for {platform}/{project} !{mr_iid} failed: {e:#}");
            None
        }
    }
}

/// GitLab discussions API fallback: fetch all discussion notes on the MR,
/// dropping system notes and our own output — the same self-echo guards as
/// webhook ingestion (§7.1 (a) report prefix, (b) own user id) — but KEEPING
/// `/review` / `/describe` command notes (user intent, part of the history).
async fn fetch_notes_via_api(
    platform: &str,
    instance_base: &str,
    project: &str,
    mr_iid: u64,
    gitlab_token: &str,
    mr_url: &str,
) -> Result<Vec<DiscussionNote>> {
    if instance_base.is_empty() {
        anyhow::bail!("no reachable GitLab instance base for the API fallback");
    }
    let client = crate::git_provider::gitlab::client::Client::new(gitlab_token, mr_url)?;
    let discussions = client.list_discussions().await?;
    let self_id = self_user_id_cached(platform, gitlab_token, instance_base).await;

    let mut notes = Vec::new();
    for note in discussions.into_iter().flat_map(|d| d.notes) {
        if note.system {
            continue;
        }
        if !is_command_note(&note.body.to_lowercase()) {
            if is_self_report(&note.body) {
                continue;
            }
            if Some(note.author.id) == self_id {
                continue;
            }
        }
        let author = if note.author.username.is_empty() {
            if note.author.name.is_empty() {
                format!("user#{}", note.author.id)
            } else {
                note.author.name
            }
        } else {
            note.author.username
        };
        let created_at = parse_note_created_at(Some(&note.created_at)).unwrap_or_else(|| {
            tracing::warn!(
                "discussion tap: note {} has no parseable created_at, using fetch time",
                note.id
            );
            chrono::Utc::now()
        });
        notes.push(DiscussionNote {
            platform: platform.to_string(),
            project: project.to_string(),
            mr_iid,
            note_id: note.id.max(0) as u64,
            author,
            body: note.body,
            created_at,
        });
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    async fn fresh_db() -> SqlxStore {
        let db = SqlxStore::new_in_memory().await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    fn note(note_id: u64, author: &str, body: &str, secs: i64) -> DiscussionNote {
        DiscussionNote {
            platform: "default".to_string(),
            project: "group/proj".to_string(),
            mr_iid: 7,
            note_id,
            author: author.to_string(),
            body: body.to_string(),
            created_at: chrono::Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap(),
        }
    }

    /// (a) seeded notes render in (created_at, note_id) order, every entry
    /// present, oversized bodies truncated at 2000 chars.
    #[tokio::test]
    async fn render_includes_all_notes_in_order_and_truncates() {
        let db = fresh_db().await;
        let long_body = "x".repeat(5000);
        // Seed out of order; list_notes must restore (created_at, note_id).
        db.upsert_note(&note(3, "bob", "third", 30)).await.unwrap();
        db.upsert_note(&note(1, "alice", "first", 10)).await.unwrap();
        db.upsert_note(&note(2, "carol", &long_body, 20)).await.unwrap();

        let notes = db.list_notes("default", "group/proj", 7).await.unwrap();
        let section = render_discussion_context(&notes);

        assert!(section.starts_with(SECTION_HEADER));
        let pos_first = section.find("[alice @").expect("alice entry");
        let pos_second = section.find("[carol @").expect("carol entry");
        let pos_third = section.find("[bob @").expect("bob entry");
        assert!(
            pos_first < pos_second && pos_second < pos_third,
            "order must be chronological"
        );
        // Truncated at 2000 chars, not 5000.
        assert!(section.contains(&"x".repeat(2000)));
        assert!(!section.contains(&"x".repeat(2001)));
    }

    /// (b) prefix stability: the same input renders byte-identically twice,
    /// and the sha256 matches across renders.
    #[tokio::test]
    async fn render_is_byte_identical_and_hash_stable() {
        let db = fresh_db().await;
        db.upsert_note(&note(1, "alice", "first", 10)).await.unwrap();
        db.upsert_note(&note(2, "bob", "second", 20)).await.unwrap();
        let notes = db.list_notes("default", "group/proj", 7).await.unwrap();

        let a = render_discussion_context(&notes);
        let b = render_discussion_context(&notes);
        assert_eq!(a, b, "rendering must be deterministic");
        assert_eq!(sha256_hex(&a), sha256_hex(&b));
    }

    /// (c) empty DB → the fallback supplies notes, each is back-filled
    /// (list_notes reads them back) and used for rendering.
    #[tokio::test]
    async fn empty_db_uses_fallback_and_back_fills() {
        let db = fresh_db().await;
        let fetched = vec![note(9, "dave", "from api", 5)];
        let fetched_clone = fetched.clone();

        let notes = load_discussion_notes(&db, "default", "group/proj", 7, move || {
            let fetched = fetched_clone.clone();
            async move { Ok(fetched) }
        })
        .await
        .expect("fallback notes must load");

        assert_eq!(notes, fetched);
        let stored = db.list_notes("default", "group/proj", 7).await.unwrap();
        assert_eq!(stored, fetched, "fallback notes must be back-filled");
        assert!(render_discussion_context(&notes).contains("[dave @"));
    }

    /// (c2) a non-empty DB never calls the fallback (webhook-ingested history
    /// is authoritative).
    #[tokio::test]
    async fn non_empty_db_skips_fallback() {
        let db = fresh_db().await;
        db.upsert_note(&note(1, "alice", "stored", 10)).await.unwrap();

        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let notes = load_discussion_notes(&db, "default", "group/proj", 7, move || {
            let flag = flag.clone();
            async move {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Vec::new())
            }
        })
        .await
        .expect("stored notes must load");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "stored");
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "fallback must not run when the DB has notes"
        );
    }

    /// (d) a failing fallback yields None and never panics.
    #[tokio::test]
    async fn fallback_failure_degrades_to_none() {
        let db = fresh_db().await;
        let notes = load_discussion_notes(&db, "default", "group/proj", 7, || async {
            anyhow::bail!("gitlab down")
        })
        .await;
        assert!(notes.is_none());
    }

    /// (e) upsert_review_context on the same (task_id, kind) twice: no error,
    /// the content is rewritten.
    #[tokio::test]
    async fn review_context_upsert_rewrites_in_place() {
        use crate::server::task_queue::{TaskEntry, TaskState};
        let db = fresh_db().await;
        // review_contexts.task_id REFERENCES reviews(task_id) — seed the row.
        let entry = TaskEntry {
            task_id: Uuid::new_v4(),
            state: TaskState::Running,
            source_meta: Default::default(),
            request: None,
            result: None,
            error: None,
            progress: None,
            expert_name: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
        };
        db.create(&entry).await.unwrap();

        let hash_a = sha256_hex("content A");
        db.upsert_review_context(entry.task_id, DISCUSSION_KIND, "content A", &hash_a, 3)
            .await
            .unwrap();
        let hash_b = sha256_hex("content B");
        db.upsert_review_context(entry.task_id, DISCUSSION_KIND, "content B", &hash_b, 3)
            .await
            .unwrap();

        let rows: Vec<(String, String)> =
            ::sqlx::query_as("SELECT content, content_hash FROM review_contexts WHERE task_id = ? AND kind = ?")
                .bind(entry.task_id.to_string())
                .bind(DISCUSSION_KIND)
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "upsert must not duplicate the (task_id, kind) row");
        assert_eq!(rows[0].0, "content B");
        assert_eq!(rows[0].1, hash_b);
    }

    /// (f) oversized render degrades to None (no injection, no context row).
    #[tokio::test]
    async fn oversized_section_is_skipped() {
        let db = Arc::new(fresh_db().await);
        let entry = {
            use crate::server::task_queue::{TaskEntry, TaskState};
            TaskEntry {
                task_id: Uuid::new_v4(),
                state: TaskState::Running,
                source_meta: Default::default(),
                request: None,
                result: None,
                error: None,
                progress: None,
                expert_name: None,
                created_at: chrono::Utc::now(),
                started_at: None,
                completed_at: None,
            }
        };
        db.create(&entry).await.unwrap();
        // Seed one note whose rendered section exceeds the 128 KiB cap via
        // many max-length bodies... simpler: seed enough 2000-char notes.
        for i in 0..70u64 {
            db.upsert_note(&note(i + 1, "alice", &"y".repeat(2000), i as i64))
                .await
                .unwrap();
        }
        let tap = DiscussionTap {
            db: db.clone(),
            platform: "default".to_string(),
            instance_base: String::new(), // DB has rows → fallback never runs
        };
        let section = tap
            .inject(entry.task_id, "group/proj", 7, "token", "http://x/-/merge_requests/7")
            .await;
        assert!(section.is_none(), "oversized render must be skipped");
        let count: i64 = ::sqlx::query_scalar("SELECT COUNT(*) FROM review_contexts WHERE task_id = ?")
            .bind(entry.task_id.to_string())
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 0, "skipped injection must not persist a context row");
    }
}
