//! Pre-review AGENTS.md context injection (RENG-18).
//!
//! Loads the target repository's `AGENTS.md` (local filesystem for `--local-path`
//! reviews, GitLab API for MR/webhook reviews), renders it into a fixed-header
//! markdown section, and injects it into expert prompts. The rendered section is
//! persisted to `review_contexts` with kind `agents_md` so re-runs can reuse the
//! same content hash.
//!
//! Degradation contract: every failure path (missing file, API error, oversized
//! render, DB write failure) logs a warning and yields `None` — the review then
//! runs without AGENTS.md context, identical to the pre-RENG-18 behaviour.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::store::traits::ReviewStore;
use crate::store::SqlxStore;

/// `review_contexts.kind` value for the AGENTS.md section.
pub(crate) const AGENTS_MD_KIND: &str = "agents_md";

/// Hard cap on the rendered section. Beyond this the prompt would drown the
/// diff; skip injection entirely.
pub(crate) const MAX_CONTEXT_BYTES: usize = 128 * 1024;

/// Default cap on the raw file read. AGENTS.md is typically short project
/// guidance; 4000 bytes matches the README/manifest excerpt strategy.
pub(crate) const DEFAULT_MAX_FILE_BYTES: usize = 4000;

const SECTION_HEADER: &str = "## Agent Guidelines\n\n";

/// Render raw AGENTS.md content into the fixed-header prompt section.
/// Returns `None` when the rendered section exceeds [`MAX_CONTEXT_BYTES`].
pub(crate) fn render_agents_md(content: &str) -> Option<String> {
    let mut out = String::from(SECTION_HEADER);
    out.push_str(content);
    if out.len() > MAX_CONTEXT_BYTES {
        tracing::warn!(
            bytes = out.len(),
            "AGENTS.md context exceeds {} bytes; skipping injection",
            MAX_CONTEXT_BYTES
        );
        return None;
    }
    Some(out)
}

/// Read `AGENTS.md` from a local repository path, bounded to `max_bytes`.
/// Returns `None` when the file is missing or cannot be read.
pub(crate) fn read_local_agents_md(repo_path: &Path, max_bytes: usize) -> Option<String> {
    let path = repo_path.join("AGENTS.md");
    if !path.is_file() {
        return None;
    }
    match std::fs::read(&path) {
        Ok(bytes) => Some(truncate_string(String::from_utf8_lossy(&bytes).to_string(), max_bytes)),
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to read AGENTS.md: {e}");
            None
        }
    }
}

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

/// Compute a sha256 hex digest of `content`.
pub(crate) fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Persist the rendered section to `review_contexts`. Best-effort: failures are
/// logged and ignored; the caller still injects the section into the prompt.
pub(crate) async fn persist_agents_md(db: Option<&Arc<SqlxStore>>, task_id: Uuid, section: &str) {
    let Some(db) = db else {
        return;
    };
    let content_hash = sha256_hex(section);
    let token_estimate = (section.len() / 4) as i64;
    if let Err(e) = db
        .upsert_review_context(task_id, AGENTS_MD_KIND, section, &content_hash, token_estimate)
        .await
    {
        tracing::warn!(task_id = %task_id, "failed to persist review_context {AGENTS_MD_KIND}: {e:#}");
    }
}

/// Inject AGENTS.md context for a local repository review.
pub(crate) async fn inject_local(db: Option<&Arc<SqlxStore>>, task_id: Uuid, repo_path: &Path) -> Option<String> {
    let content = read_local_agents_md(repo_path, DEFAULT_MAX_FILE_BYTES)?;
    let section = render_agents_md(&content)?;
    persist_agents_md(db, task_id, &section).await;
    Some(section)
}

/// Inject AGENTS.md context for a GitLab MR review.
pub(crate) async fn inject_remote(
    db: Option<&Arc<SqlxStore>>,
    task_id: Uuid,
    client: &crate::git_provider::gitlab::client::Client,
    git_ref: &str,
) -> Option<String> {
    let content = fetch_remote_agents_md(client, git_ref).await?;
    let section = render_agents_md(&content)?;
    persist_agents_md(db, task_id, &section).await;
    Some(section)
}

/// Truncate a string at a safe UTF-8 boundary.
fn truncate_string(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && s.get(..boundary).is_none() {
        boundary -= 1;
    }
    s.get(..boundary).map(|s| s.to_string()).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_agents_md_adds_header() {
        let section = render_agents_md("Run tests before merging.").unwrap();
        assert!(section.starts_with(SECTION_HEADER));
        assert!(section.contains("Run tests before merging."));
    }

    #[test]
    fn render_agents_md_skips_oversized() {
        let huge = "x".repeat(MAX_CONTEXT_BYTES + 1);
        assert!(render_agents_md(&huge).is_none());
    }

    #[test]
    fn read_local_agents_md_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Agent Guidelines\n\nBe kind.").unwrap();
        let content = read_local_agents_md(dir.path(), DEFAULT_MAX_FILE_BYTES).unwrap();
        assert!(content.contains("Be kind"));
    }

    #[test]
    fn read_local_agents_md_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_local_agents_md(dir.path(), DEFAULT_MAX_FILE_BYTES).is_none());
    }

    #[test]
    fn read_local_agents_md_truncates_multibyte() {
        let dir = tempfile::tempdir().unwrap();
        // 6 bytes total; truncate at 5 should land on the third byte boundary.
        std::fs::write(dir.path().join("AGENTS.md"), "Hello 世界").unwrap();
        let content = read_local_agents_md(dir.path(), 5);
        assert!(content.is_some());
        assert!(content.unwrap().len() <= 5);
    }

    #[test]
    fn sha256_hex_is_stable() {
        let a = sha256_hex("hello");
        let b = sha256_hex("hello");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }
}
