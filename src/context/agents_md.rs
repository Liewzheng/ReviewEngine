//! Shared AGENTS.md prompt-section helpers (RENG-18).
//!
//! Pure read/render logic used by both the server review path and the CLI
//! `--local-path` review path. Kept crate-public so the CLI binary can import
//! it; the server module additionally persists the rendered section to
//! `review_contexts`.

use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::Path;

/// Hard cap on the rendered section. Beyond this the prompt would drown the
/// diff; skip injection entirely.
pub(crate) const MAX_CONTEXT_BYTES: usize = 128 * 1024;

/// Default cap on the raw file read. AGENTS.md is typically short project
/// guidance; 4000 bytes matches the README/manifest excerpt strategy.
pub const DEFAULT_MAX_FILE_BYTES: usize = 4000;

const SECTION_HEADER: &str = "## Agent Guidelines\n\n";

/// Render raw AGENTS.md content into the fixed-header prompt section.
/// Returns `None` when the rendered section exceeds [`MAX_CONTEXT_BYTES`].
pub fn render_agents_md(content: &str) -> Option<String> {
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

/// One row of the `## Injected Context` report section (M18-W5).
///
/// Every row describes a source that was *actually embedded* in the expert
/// prompt — the audit's P2-2 review note: a file that the total-budget
/// check rejected no longer appears here, so the table cannot lie about
/// injection.
///
/// * `source`: where the bytes came from — `"AGENTS.md"`, `"changed file
///   src/foo.rs"`, etc.
/// * `original_bytes`: total bytes on disk before any truncation. Always
///   `≥ injected_bytes` — a row with `original_bytes == 0` is unreachable
///   in practice (the producer paths read the file before pushing the
///   row), but the renderer treats it defensively as a "stat failed"
///   signal so a future bug never renders "0 / 0 bytes (—)".
/// * `injected_bytes`: bytes actually sent to the LLM (post-truncation).
///   Identical to the slice that was hashed into `sha256` and is embedded
///   into the prompt body.
/// * `truncated`: `true` when `injected_bytes < original_bytes`.
/// * `sha256`: hex fingerprint of the *injected* bytes, so the reader can
///   confirm two reports covered the same surface.
///
/// The fingerprint is computed over what the LLM saw, not the on-disk bytes:
/// if the report surfaces "AGENTS.md hashed `abcd…`" the reader can compare
/// it against the *next* run's hash and notice a silent change in the
/// injected slice — exactly the audit's RENG-19 "截断不可见" pain point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectedContextEntry {
    /// Human-readable label (matches what the prompt called this source).
    pub source: String,
    /// Bytes on disk before any truncation (≥ `injected_bytes`).
    pub original_bytes: usize,
    /// Bytes actually shipped to the LLM (post-truncation, when any).
    pub injected_bytes: usize,
    /// `true` when `injected_bytes < original_bytes`.
    pub truncated: bool,
    /// Hex SHA-256 of the *injected* bytes (the same slice whose length is
    /// `injected_bytes`). `""` is reserved for a future "no bytes embedded"
    /// state; today's producer paths always populate it.
    pub sha256: String,
}

impl InjectedContextEntry {
    /// Render this entry as a one-line markdown row.
    pub fn render_row(&self) -> String {
        // P3-4 (review r1): drop the "original_bytes == 0 ⇒ '— missing'"
        // branch. The producer paths never push a row with
        // `original_bytes == 0` (AGENTS.md missing → `None`; file path
        // stat failed → not pushed either). A future bug that lets one
        // through gets "stat failed" instead of "missing" — accurate, and
        // it can't be mistaken for the AGENTS.md-missing case (which is
        // an absent row, not a sentinel row).
        let pct = if self.original_bytes == 0 {
            "—".to_string()
        } else {
            format!(
                "{:.0}%",
                (self.injected_bytes as f64 / self.original_bytes as f64) * 100.0
            )
        };
        let status = if self.original_bytes == 0 {
            "— stat failed"
        } else if self.truncated {
            "⚠ truncated"
        } else {
            "ok"
        };
        format!(
            "| `{}` | {} / {} bytes ({}) | {} | `{}` |",
            self.source,
            self.injected_bytes,
            self.original_bytes,
            pct,
            status,
            if self.sha256.is_empty() {
                "—"
            } else {
                &self.sha256[..16.min(self.sha256.len())]
            }
        )
    }
}

/// Read `AGENTS.md` from a local repository path, bounded to `max_bytes`.
/// Returns `None` when the file is missing, is a symlink, or cannot be read.
/// A symlinked `AGENTS.md` is skipped so a repo cannot point the read at a
/// file outside the checkout; regular files only.
///
/// For the M18-W5 `## Injected Context` report section use
/// [`read_local_agents_md_with_meta`]: it returns both the truncated content
/// AND the meta-row so the renderer doesn't have to re-stat the file.
pub fn read_local_agents_md(repo_path: &Path, max_bytes: usize) -> Option<String> {
    read_local_agents_md_with_meta(repo_path, max_bytes).map(|r| r.content)
}

/// Same as [`read_local_agents_md`] but also returns the
/// [`InjectedContextEntry`] the report will surface (M18-W5).
pub fn read_local_agents_md_with_meta(repo_path: &Path, max_bytes: usize) -> Option<InjectedReadResult> {
    let path = repo_path.join("AGENTS.md");
    // `is_file()` follows symlinks, so reject them explicitly to avoid reading
    // a file outside the checkout via a malicious `AGENTS.md` symlink.
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::warn!(path = %path.display(), "AGENTS.md is a symlink; skipping injection");
            return None;
        }
        Ok(meta) if !meta.is_file() => return None,
        Ok(_) => {}
        Err(_) => return None,
    }
    match std::fs::read(&path) {
        Ok(bytes) => {
            let original_bytes = bytes.len();
            let content = String::from_utf8_lossy(&bytes).to_string();
            let truncated = content.len() > max_bytes;
            if truncated {
                tracing::warn!(
                    path = %path.display(),
                    bytes = original_bytes,
                    max_bytes,
                    "AGENTS.md exceeds max_bytes; truncating injected context"
                );
            }
            let injected = truncate_string(content, max_bytes);
            let injected_bytes = injected.len();
            let sha = sha256_hex(&injected);
            Some(InjectedReadResult {
                content: injected,
                meta: InjectedContextEntry {
                    source: "AGENTS.md".to_string(),
                    original_bytes,
                    injected_bytes,
                    truncated,
                    sha256: sha,
                },
            })
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to read AGENTS.md: {e}");
            None
        }
    }
}

/// Result of [`read_local_agents_md_with_meta`]: the truncated content plus
/// the [`InjectedContextEntry`] the report's "## Injected Context" section
/// needs.
#[derive(Debug, Clone)]
pub struct InjectedReadResult {
    /// Truncated content (what the LLM saw).
    pub content: String,
    /// Meta row for the report's "## Injected Context" section.
    pub meta: InjectedContextEntry,
}

/// Compute a sha256 hex digest of `content`.
pub fn sha256_hex(content: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
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
    fn read_local_agents_md_with_meta_truncation_is_visible() {
        // RENG-19 pain point: AGENTS.md 13171 bytes / default cap 4000 used
        // to log a WARN and silently ship the head 4000 bytes. The meta row
        // must surface original_bytes / injected_bytes / truncated + hash.
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(10_000);
        std::fs::write(dir.path().join("AGENTS.md"), &big).unwrap();
        let res = read_local_agents_md_with_meta(dir.path(), 1000).unwrap();
        assert_eq!(res.meta.original_bytes, 10_000);
        assert_eq!(res.meta.injected_bytes, 1000);
        assert!(res.meta.truncated, "truncated must be true when bytes < original_bytes");
        assert_eq!(res.meta.sha256.len(), 64, "full SHA-256 hex (we slice on render)");
        assert_eq!(res.content.len(), 1000);
    }

    #[test]
    fn read_local_agents_md_with_meta_short_file_is_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "small").unwrap();
        let res = read_local_agents_md_with_meta(dir.path(), 1000).unwrap();
        assert_eq!(res.meta.original_bytes, 5);
        assert_eq!(res.meta.injected_bytes, 5);
        assert!(!res.meta.truncated);
    }

    #[test]
    fn read_local_agents_md_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_local_agents_md(dir.path(), DEFAULT_MAX_FILE_BYTES).is_none());
        assert!(read_local_agents_md_with_meta(dir.path(), DEFAULT_MAX_FILE_BYTES).is_none());
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

    #[test]
    fn injected_entry_render_row_marks_truncation() {
        let entry = InjectedContextEntry {
            source: "AGENTS.md".to_string(),
            original_bytes: 13_171,
            injected_bytes: 4_000,
            truncated: true,
            sha256: "0123456789abcdef".repeat(4),
        };
        let row = entry.render_row();
        assert!(row.contains("`AGENTS.md`"));
        assert!(row.contains("4000 / 13171 bytes"));
        assert!(row.contains("⚠ truncated"));
        // 16 hex chars of the full fingerprint — the function slices sha256[..16].
        assert!(
            row.contains("0123456789abcdef"),
            "the row carries the 16-char fingerprint prefix, got: {row}"
        );
    }

    // RENG-18 symlink guard: a symlinked AGENTS.md must be skipped so the read
    // cannot escape the checkout.
    #[cfg(unix)]
    #[test]
    fn read_local_agents_md_skips_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("secret.md");
        std::fs::write(&target, "secret").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("AGENTS.md")).unwrap();

        assert!(
            read_local_agents_md(dir.path(), DEFAULT_MAX_FILE_BYTES).is_none(),
            "symlinked AGENTS.md must not be read"
        );
        assert!(
            read_local_agents_md_with_meta(dir.path(), DEFAULT_MAX_FILE_BYTES).is_none(),
            "symlinked AGENTS.md must not be read (meta variant)"
        );
    }
}
