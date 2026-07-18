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

use crate::models::DiffFile;

/// Per-file byte cap for injected contents. Content beyond the cap is
/// truncated and the truncation is noted inline.
pub(crate) const MAX_SINGLE_FILE_BYTES: usize = 20_000;

/// Build the `## Full File Contents` body for the given diff files.
///
/// Files are ordered by change size (additions + deletions, descending, path
/// as tie-break) so the most heavily modified files win the `max_total_bytes`
/// budget; files that no longer fit are listed under an "Omitted" heading.
/// Returns an empty string when the budget is zero, the project path is not
/// a readable local directory (e.g. remote reviews), or no file content
/// could be read.
pub(crate) fn build_file_contents_section(files: &[DiffFile], project_path: &str, max_total_bytes: usize) -> String {
    if max_total_bytes == 0 || files.is_empty() {
        return String::new();
    }
    let root = std::path::Path::new(project_path);
    if !root.is_dir() {
        return String::new();
    }

    let mut ordered: Vec<&DiffFile> = files.iter().collect();
    ordered.sort_by(|a, b| {
        (b.additions + b.deletions)
            .cmp(&(a.additions + a.deletions))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut out = String::new();
    let mut used = 0usize;
    let mut omitted: Vec<&str> = Vec::new();

    for file in ordered {
        let Some(content) = read_changed_file(root, &file.path, MAX_SINGLE_FILE_BYTES) else {
            continue; // unsafe path, unreadable, or non-UTF-8 → skip silently
        };
        if used + content.len() > max_total_bytes {
            omitted.push(&file.path);
            continue;
        }
        used += content.len();
        let ext = std::path::Path::new(&file.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        out.push_str(&format!(
            "### `{}`\n```{}\n{}\n```\n\n",
            file.path,
            ext,
            content.trim_end()
        ));
    }

    if !omitted.is_empty() {
        out.push_str("### Omitted (context budget exhausted)\n");
        for path in omitted {
            out.push_str(&format!("- `{}`\n", path));
        }
    }

    out.trim_end().to_string()
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
