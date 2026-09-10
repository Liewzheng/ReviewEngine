//! Server-side AGENTS.md context persistence (RENG-18).
//!
//! The pure read/render helpers live in [`crate::context::agents_md`] (shared
//! with the CLI `--local-path` path). This module adds the server-only step:
//! persisting the rendered section to `review_contexts` with kind `agents_md`
//! so re-runs can reuse the same content hash.

use std::sync::Arc;

use uuid::Uuid;

use crate::context::agents_md;
use crate::store::traits::ReviewStore;
use crate::store::SqlxStore;

/// `review_contexts.kind` value for the AGENTS.md section.
pub(crate) const AGENTS_MD_KIND: &str = "agents_md";

// Re-export the shared helpers so callers (resolve.rs, cli) can reach them via
// a single path.
pub use crate::context::agents_md::{read_local_agents_md, render_agents_md, DEFAULT_MAX_FILE_BYTES};

/// Fetch `AGENTS.md` from the GitLab repository at the given git ref.
/// Returns `None` on any API or parsing failure.
pub(crate) async fn fetch_remote_agents_md(
    client: &crate::git_provider::gitlab::client::Client,
    git_ref: &str,
) -> Option<String> {
    match client.fetch_file_raw("AGENTS.md", git_ref).await {
        Ok(content) => Some(content),
        Err(e) => {
            tracing::warn!(git_ref, "failed to fetch AGENTS.md from GitLab: {e:#}");
            None
        }
    }
}

/// Persist the rendered section to `review_contexts`. Best-effort: failures are
/// logged and ignored; the caller still injects the section into the prompt.
pub(crate) async fn persist_agents_md(db: Option<&Arc<SqlxStore>>, task_id: Uuid, section: &str) {
    let Some(db) = db else {
        return;
    };
    let content_hash = agents_md::sha256_hex(section);
    let token_estimate = (section.len() / 4) as i64;
    if let Err(e) = db
        .upsert_review_context(task_id, AGENTS_MD_KIND, section, &content_hash, token_estimate)
        .await
    {
        tracing::warn!(task_id = %task_id, "failed to persist review_context {AGENTS_MD_KIND}: {e:#}");
    }
}
