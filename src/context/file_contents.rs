//! Full-file content injection for expert prompts (repo-aware review, A2).
//!
//! Experts historically reviewed the unified diff alone, which made
//! "X is missing"-style hallucinations common — claims that could have been
//! disproven by simply reading the changed file. For local reviews the
//! current contents of the changed files are appended to the expert user
//! prompt (a `## Full File Contents` section), capped per file and in total
//! so the prompt stays within a sane size budget. Files that cannot be read
//! — remote reviews, deleted files, non-UTF-8 content, unsafe paths — are
//! skipped silently.

use crate::context::agents_md::InjectedContextEntry;
use crate::models::DiffFile;

/// Per-file byte cap for injected contents. Content beyond the cap is
/// truncated and the truncation is noted inline.
pub(crate) const MAX_SINGLE_FILE_BYTES: usize = 20_000;

/// Build the `## Full File Contents` body for the given diff files, plus a
/// meta report (`FileContentsReport`) the lead summary surfaces.
///
/// Files are ordered by change size (additions + deletions, descending, path
/// as tie-break) so the most heavily modified files win the `max_total_bytes`
/// budget; files that no longer fit are listed under an "Omitted" heading.
/// Returns an empty string when the budget is zero, the project path is not
/// a readable local directory (e.g. remote reviews), or no file content
/// could be read.
///
/// `meta` is built unconditionally (even when the section is empty), so the
/// lead summary can render "no changed files were injected" honestly.
pub(crate) fn build_file_contents_section(files: &[DiffFile], project_path: &str, max_total_bytes: usize) -> String {
    build_file_contents_section_with_meta(files, project_path, max_total_bytes).body
}

/// Like [`build_file_contents_section`], but also returns the per-file
/// [`InjectedContextEntry`] rows the M18-W5 `## Injected Context` section
/// needs. M18-W5: a file whose content was injected but truncated to
/// `[MAX_SINGLE_FILE_BYTES]` is reported with its original / injected bytes
/// and a sha256 fingerprint, so a 200 KB file injected as the head 20 KB
/// does not silently count as "we read the whole thing".
pub(crate) fn build_file_contents_section_with_meta(
    files: &[DiffFile],
    project_path: &str,
    max_total_bytes: usize,
) -> FileContentsReport {
    if max_total_bytes == 0 || files.is_empty() {
        return FileContentsReport::default();
    }
    let root = std::path::Path::new(project_path);
    if !root.is_dir() {
        return FileContentsReport::default();
    }

    let mut ordered: Vec<&DiffFile> = files.iter().collect();
    ordered.sort_by(|a, b| {
        (b.additions + b.deletions)
            .cmp(&(a.additions + a.deletions))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut body = String::new();
    let mut used = 0usize;
    let mut omitted: Vec<&str> = Vec::new();
    let mut meta: Vec<InjectedContextEntry> = Vec::new();

    for file in ordered {
        let Some(injected_content) = read_changed_file(root, &file.path, MAX_SINGLE_FILE_BYTES) else {
            continue; // unsafe path, unreadable, or non-UTF-8 → skip silently
        };
        let original_bytes = std::fs::metadata(root.join(&file.path))
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let per_file_truncated = injected_content.len() < original_bytes || original_bytes > MAX_SINGLE_FILE_BYTES;

        // M35 (M32 review r2 P3-2): the displayed slice the LLM sees is
        // `injected_content.trim_end()` — the body uses the trimmed slice.
        // Trim once, up front, and feed that single `displayed` value to
        // every sink (budget check, `injected_bytes`, SHA-256 hash, body)
        // so they cannot disagree. Previously the body embedded
        // `injected_content.trim_end()` while the meta row hashed and
        // counted the un-trimmed `injected_content` — the embedded slice
        // and the fingerprint were off by a few trailing-newline bytes.
        //
        // P2-2 (review r1): the meta row was pushed *before* the
        // total-budget check before this fix, so a file that was then
        // skipped by the budget (`omitted.push(...); continue`) still got
        // a row whose `injected_bytes = content.len()` and
        // `truncated = false`, rendering as `8192 / 8192 bytes (100%) |
        // ok` while the prompt body listed it under "Omitted". The
        // budget check now runs *before* the row is pushed; files
        // rejected by the budget show up under "Omitted (context budget
        // exhausted)" in the prompt body, and their row is intentionally
        // absent from `## Injected Context` so the audit cannot disagree
        // with the body.
        let displayed = injected_content.trim_end();

        if used + displayed.len() > max_total_bytes {
            omitted.push(&file.path);
            continue;
        }
        used += displayed.len();

        meta.push(InjectedContextEntry {
            source: format!("changed file {}", file.path),
            original_bytes,
            injected_bytes: displayed.len(),
            truncated: per_file_truncated,
            sha256: crate::context::agents_md::sha256_hex(displayed),
        });

        let ext = std::path::Path::new(&file.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        body.push_str(&format!("### `{}`\n```{}\n{}\n```\n\n", file.path, ext, displayed,));
    }

    if !omitted.is_empty() {
        body.push_str("### Omitted (context budget exhausted)\n");
        for path in omitted {
            body.push_str(&format!("- `{}`\n", path));
        }
    }

    FileContentsReport {
        body: body.trim_end().to_string(),
        entries: meta,
    }
}

/// What [`build_file_contents_section_with_meta`] returns: the prompt section
/// plus the meta rows the M18-W5 `## Injected Context` block surfaces.
///
/// `entries` is consumed by the orchestrator (M35), which feeds it into
/// `ConsolidatedReport::injected_context` for the lead-summary renderer.
/// Splitting the two outputs (body / entries) lets the per-task prompt
/// builders continue to use `build_file_contents_section` (body-only)
/// without dragging the meta rows through their call sites.
#[derive(Debug, Clone, Default)]
pub(crate) struct FileContentsReport {
    /// The actual prompt-body markdown (same shape as
    /// [`build_file_contents_section`] returns).
    pub body: String,
    /// Per-changed-file `## Injected Context` rows.
    pub entries: Vec<InjectedContextEntry>,
}

/// Read a changed file from the local checkout, capped at `max_bytes`.
///
/// Returns `None` for paths that escape the project root (absolute paths or
/// `..` components, mirroring the verifier's `load_file_context` safety
/// rules), unreadable files, and non-UTF-8 content — callers skip those
/// silently.
fn read_changed_file(root: &std::path::Path, path: &str, max_bytes: usize) -> Option<String> {
    let rel = std::path::Path::new(path);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    let bytes = std::fs::read(root.join(rel)).ok()?;
    let mut text = String::from_utf8(bytes).ok()?;
    if text.len() > max_bytes {
        let boundary = text.floor_char_boundary(max_bytes);
        text.truncate(boundary);
        text.push_str(&format!("\n... (file truncated: exceeded {max_bytes} bytes)"));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::DiffHunk;

    fn make_diff_file(path: &str, additions: u32, deletions: u32) -> DiffFile {
        DiffFile {
            path: path.to_string(),
            old_path: path.to_string(),
            new_path: path.to_string(),
            status: "modified".to_string(),
            additions,
            deletions,
            hunks: vec![DiffHunk {
                header: "@@ -1 +1 @@".to_string(),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                lines: vec![],
            }],
        }
    }

    #[test]
    fn test_section_contains_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let files = vec![make_diff_file("src/main.rs", 3, 1)];
        let section = build_file_contents_section(&files, dir.path().to_str().unwrap(), 60_000);

        assert!(section.contains("### `src/main.rs`"));
        assert!(section.contains("```rs"));
        assert!(section.contains("fn main() {}"));
        assert!(!section.contains("Omitted"));
    }

    #[test]
    fn test_remote_or_missing_project_path_yields_empty() {
        let files = vec![make_diff_file("src/main.rs", 3, 1)];
        // "owner/repo" style project path does not exist locally → no injection.
        let section = build_file_contents_section(&files, "owner/repo-does-not-exist-xyz", 60_000);
        assert!(section.is_empty());
    }

    #[test]
    fn test_zero_budget_disables_injection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        let files = vec![make_diff_file("a.rs", 1, 0)];
        assert!(build_file_contents_section(&files, dir.path().to_str().unwrap(), 0).is_empty());
    }

    #[test]
    fn test_single_file_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_SINGLE_FILE_BYTES + 500);
        std::fs::write(dir.path().join("big.rs"), &big).unwrap();

        let files = vec![make_diff_file("big.rs", 10, 0)];
        let section = build_file_contents_section(&files, dir.path().to_str().unwrap(), 60_000);

        assert!(section.contains("file truncated: exceeded 20000 bytes"));
        // Truncated content (20000 + note) fits in the section.
        assert!(section.len() < MAX_SINGLE_FILE_BYTES + 1000);
    }

    #[test]
    fn test_budget_prioritizes_largest_changes_and_omits_rest() {
        let dir = tempfile::tempdir().unwrap();
        // Three 100-byte files; budget fits only two.
        std::fs::write(dir.path().join("small.rs"), "s".repeat(100)).unwrap();
        std::fs::write(dir.path().join("mid.rs"), "m".repeat(100)).unwrap();
        std::fs::write(dir.path().join("large.rs"), "l".repeat(100)).unwrap();

        let files = vec![
            make_diff_file("small.rs", 1, 0),
            make_diff_file("large.rs", 90, 10),
            make_diff_file("mid.rs", 40, 5),
        ];
        let section = build_file_contents_section(&files, dir.path().to_str().unwrap(), 210);

        // Priority: large (100 changes) → mid (45) → small (1, omitted).
        assert!(section.contains("### `large.rs`"));
        assert!(section.contains("### `mid.rs`"));
        assert!(!section.contains("### `small.rs`"));
        assert!(section.contains("### Omitted (context budget exhausted)"));
        assert!(section.contains("- `small.rs`"));
        // large.rs section must appear before mid.rs.
        let large_pos = section.find("### `large.rs`").unwrap();
        let mid_pos = section.find("### `mid.rs`").unwrap();
        assert!(large_pos < mid_pos);
    }

    /// M35 (M32 review r2 P3-3): the `_with_meta` producer's omitted-file
    /// behavior — a file rejected by the budget check must NOT receive a
    /// row in `entries`. The audit's P2-2 worry was that an admitted row
    /// could render "8192 / 8192 bytes (100%) | ok" while the prompt body
    /// listed the file under "Omitted"; that's the bug surface this test
    /// pins so the orchestrator (M35) wiring can rely on the invariant.
    #[test]
    fn test_omitted_file_has_no_meta_row() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fits.rs"), "fn fits() {}\n".repeat(50)).unwrap(); // ~700 bytes
        std::fs::write(dir.path().join("huge.rs"), "h".repeat(5000)).unwrap();

        let files = vec![
            make_diff_file("huge.rs", 100, 0), // huge.rs beats fits.rs to the budget
            make_diff_file("fits.rs", 1, 0),
        ];
        // Budget that admits huge.rs (5000 bytes) but rejects fits.rs.
        let report = build_file_contents_section_with_meta(&files, dir.path().to_str().unwrap(), 5_000);

        assert!(report.body.contains("### `huge.rs`"));
        assert!(!report.body.contains("### `fits.rs`"));
        assert!(report.body.contains("### Omitted (context budget exhausted)"));
        assert!(report.body.contains("- `fits.rs`"));

        // huge.rs has a row; fits.rs deliberately does not.
        let sources: Vec<&str> = report.entries.iter().map(|e| e.source.as_str()).collect();
        assert!(
            sources.iter().any(|s| s.contains("huge.rs")),
            "huge.rs is admitted and must carry a meta row; got {sources:?}"
        );
        assert!(
            !sources.iter().any(|s| s.contains("fits.rs")),
            "fits.rs is omitted and must NOT carry a meta row; got {sources:?}"
        );
    }

    /// M35 (M32 review r2 P3-2): the body, `injected_bytes`, and
    /// `sha256` all reference the *trimmed* slice the LLM actually sees,
    /// not the post-truncation `read_changed_file` return value (which
    /// retains trailing whitespace from `read_to_string`). Pin the
    /// invariant so a future refactor that diverges again is caught.
    #[test]
    fn test_meta_row_hash_and_bytes_match_trimmed_body() {
        let dir = tempfile::tempdir().unwrap();
        // File ends with newline + trailing whitespace; trim_end must
        // strip them from the meta row, hash, and body simultaneously.
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n\n\n").unwrap();

        let files = vec![make_diff_file("a.rs", 1, 0)];
        let report = build_file_contents_section_with_meta(&files, dir.path().to_str().unwrap(), 60_000);

        assert_eq!(report.entries.len(), 1);
        let entry = &report.entries[0];
        // Original on-disk bytes include the trailing newlines; trimmed
        // displayed bytes do not. The row's `injected_bytes` matches the
        // body the LLM actually saw (the trimmed one).
        assert_eq!(entry.original_bytes, "fn a() {}\n\n\n".len());
        assert!(
            entry.injected_bytes < entry.original_bytes,
            "trim_end should shorten the injected byte count"
        );

        // The body fences `displayed` in ```<ext>\n{displayed}\n```,
        // so the displayed slice appears between the language tag and
        // the closing fence. Pin the substring that includes the
        // ext tag so a regression that re-introduces the trailing
        // newlines is caught.
        let displayed = "fn a() {}";
        let expected_segment = format!("```rs\n{displayed}\n```");
        assert!(
            report.body.contains(&expected_segment),
            "the body must contain `{expected_segment}`; body was: {report_body}",
            report_body = report.body
        );
        // Body length is strictly the displayed bytes, not the
        // un-trimmed `injected_content`. (The body has additional
        // "### `a.rs`" header + final `trim_end()` so we don't check an
        // exact match — only the substring.)
        let expected_hash = crate::context::agents_md::sha256_hex(displayed);
        assert_eq!(entry.sha256, expected_hash);
    }

    #[test]
    fn test_budget_falls_through_to_smaller_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("huge.rs"), "h".repeat(500)).unwrap();
        std::fs::write(dir.path().join("tiny.rs"), "t".repeat(50)).unwrap();

        let files = vec![make_diff_file("huge.rs", 100, 0), make_diff_file("tiny.rs", 1, 0)];
        // Budget fits tiny.rs but not huge.rs.
        let section = build_file_contents_section(&files, dir.path().to_str().unwrap(), 100);

        assert!(section.contains("### `tiny.rs`"));
        assert!(!section.contains("### `huge.rs`"));
        assert!(section.contains("- `huge.rs`"));
    }

    #[test]
    fn test_missing_files_are_skipped_silently() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("exists.rs"), "fn e() {}\n").unwrap();

        let files = vec![make_diff_file("deleted.rs", 0, 20), make_diff_file("exists.rs", 5, 0)];
        let section = build_file_contents_section(&files, dir.path().to_str().unwrap(), 60_000);

        assert!(section.contains("### `exists.rs`"));
        // No omitted note for files that simply could not be read.
        assert!(!section.contains("deleted.rs"));
    }

    #[test]
    fn test_unsafe_paths_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.rs"), "fn ok() {}\n").unwrap();

        let files = vec![
            make_diff_file("../escape.rs", 50, 0),
            make_diff_file("/etc/passwd", 40, 0),
            make_diff_file("ok.rs", 1, 0),
        ];
        let section = build_file_contents_section(&files, dir.path().to_str().unwrap(), 60_000);

        assert!(section.contains("### `ok.rs`"));
        assert!(!section.contains("escape.rs"));
        assert!(!section.contains("passwd"));
    }

    #[test]
    fn test_read_changed_file_rejects_traversal_and_absolute() {
        let root = std::path::Path::new("/tmp");
        assert!(read_changed_file(root, "../secret", 1000).is_none());
        assert!(read_changed_file(root, "/etc/passwd", 1000).is_none());
        assert!(read_changed_file(root, "", 1000).is_none());
    }

    #[test]
    fn test_read_changed_file_rejects_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin.rs"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        assert!(read_changed_file(dir.path(), "bin.rs", 1000).is_none());
    }
}
