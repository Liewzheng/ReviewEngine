//! Shared AGENTS.md prompt-section helpers (RENG-18).
//!
//! Pure read/render logic used by both the server review path and the CLI
//! `--local-path` review path. Kept crate-public so the CLI binary can import
//! it; the server module additionally persists the rendered section to
//! `review_contexts`.

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

/// Read `AGENTS.md` from a local repository path, bounded to `max_bytes`.
/// Returns `None` when the file is missing, is a symlink, or cannot be read.
/// A symlinked `AGENTS.md` is skipped so a repo cannot point the read at a
/// file outside the checkout; regular files only.
pub fn read_local_agents_md(repo_path: &Path, max_bytes: usize) -> Option<String> {
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
            let content = String::from_utf8_lossy(&bytes).to_string();
            if content.len() > max_bytes {
                tracing::warn!(
                    path = %path.display(),
                    bytes = content.len(),
                    max_bytes,
                    "AGENTS.md exceeds {} bytes; truncating injected context",
                    max_bytes
                );
            }
            Some(truncate_string(content, max_bytes))
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to read AGENTS.md: {e}");
            None
        }
    }
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
    }
}
