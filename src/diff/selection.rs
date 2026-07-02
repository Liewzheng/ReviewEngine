use crate::models::{DiffFile, DiffLineKind};

/// Compress deletion-only hunks into a compact list format.
/// Returns (kept_files, deleted_files_list).
pub fn compress_deletions(files: Vec<DiffFile>) -> (Vec<DiffFile>, Vec<String>) {
    let mut kept = Vec::new();
    let mut deleted = Vec::new();

    for file in files {
        let all_deletions = file
            .hunks
            .iter()
            .all(|h| h.lines.iter().all(|l| matches!(l.kind, DiffLineKind::Delete)));
        if all_deletions && !file.hunks.is_empty() {
            deleted.push(file.old_path.clone());
        } else {
            kept.push(file);
        }
    }

    (kept, deleted)
}

/// Sort files by primary language group, then by change size descending.
pub fn sort_files_by_language_and_size(files: &mut Vec<DiffFile>) {
    files.sort_by(|a, b| {
        let lang_a = detect_language_from_diff_path(&a.new_path);
        let lang_b = detect_language_from_diff_path(&b.new_path);
        lang_a.cmp(&lang_b).then_with(|| {
            let size_a = a.additions + a.deletions;
            let size_b = b.additions + b.deletions;
            size_b.cmp(&size_a)
        })
    });
}

/// Detect primary language from file path.
pub fn detect_language_from_diff_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "ts" | "tsx" | "jsx" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "rb" => "Ruby",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "c" | "h" => "C",
        "cpp" | "hpp" | "cc" | "cxx" => "C++",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DiffFile, DiffHunk, DiffLine, DiffLineKind};

    fn make_file(path: &str, hunks: Vec<DiffHunk>, additions: u32, deletions: u32) -> DiffFile {
        DiffFile {
            old_path: path.to_string(),
            new_path: path.to_string(),
            path: path.to_string(),
            status: "modified".to_string(),
            additions,
            deletions,
            hunks,
        }
    }

    fn make_hunk(lines: Vec<DiffLine>) -> DiffHunk {
        DiffHunk {
            header: "@@ -1 +1 @@".to_string(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            lines,
        }
    }

    fn make_line(kind: DiffLineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
            old_line_no: None,
            new_line_no: None,
        }
    }

    #[test]
    fn test_compress_deletions_all_deletions() {
        let file = DiffFile {
            old_path: "removed.rs".to_string(),
            new_path: "removed.rs".to_string(),
            path: "removed.rs".to_string(),
            status: "deleted".to_string(),
            additions: 0,
            deletions: 5,
            hunks: vec![DiffHunk {
                header: "@@ -1 +0,0 @@".to_string(),
                old_start: 1,
                old_lines: 1,
                new_start: 0,
                new_lines: 0,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Delete,
                    content: "-old code".to_string(),
                    old_line_no: Some(1),
                    new_line_no: None,
                }],
            }],
        };
        let (kept, deleted) = compress_deletions(vec![file]);
        assert!(kept.is_empty());
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0], "removed.rs");
    }

    #[test]
    fn test_compress_deletions_mixed() {
        let file = make_file(
            "mixed.rs",
            vec![make_hunk(vec![
                make_line(DiffLineKind::Delete, "-old"),
                make_line(DiffLineKind::Add, "+new"),
            ])],
            1,
            1,
        );
        let (kept, deleted) = compress_deletions(vec![file]);
        assert_eq!(kept.len(), 1);
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_compress_deletions_empty_input() {
        let (kept, deleted) = compress_deletions(vec![]);
        assert!(kept.is_empty());
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_compress_deletions_multiple_files() {
        let del_file = make_file(
            "gone.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Delete, "-remove")])],
            0,
            1,
        );
        let keep_file = make_file(
            "stay.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Context, " keep")])],
            0,
            0,
        );
        let (kept, deleted) = compress_deletions(vec![del_file, keep_file]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].new_path, "stay.rs");
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0], "gone.rs");
    }

    #[test]
    fn test_compress_deletions_empty_hunks_kept() {
        let file = make_file("empty.rs", vec![], 0, 0);
        let (kept, deleted) = compress_deletions(vec![file]);
        assert_eq!(kept.len(), 1);
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_sort_files_by_language_and_size() {
        let mut files = vec![
            DiffFile {
                old_path: "a.py".to_string(),
                new_path: "a.py".to_string(),
                path: "a.py".to_string(),
                status: "modified".to_string(),
                additions: 100,
                deletions: 0,
                hunks: vec![],
            },
            DiffFile {
                old_path: "b.rs".to_string(),
                new_path: "b.rs".to_string(),
                path: "b.rs".to_string(),
                status: "modified".to_string(),
                additions: 10,
                deletions: 0,
                hunks: vec![],
            },
            DiffFile {
                old_path: "c.rs".to_string(),
                new_path: "c.rs".to_string(),
                path: "c.rs".to_string(),
                status: "modified".to_string(),
                additions: 50,
                deletions: 0,
                hunks: vec![],
            },
        ];
        sort_files_by_language_and_size(&mut files);
        assert_eq!(files[0].new_path, "a.py");
        assert_eq!(files[1].new_path, "c.rs");
        assert_eq!(files[2].new_path, "b.rs");
    }

    #[test]
    fn test_sort_files_by_language_grouping() {
        let mut files = vec![make_file("main.rs", vec![], 10, 0), make_file("lib.py", vec![], 5, 0)];
        sort_files_by_language_and_size(&mut files);
        assert_eq!(files[0].new_path, "lib.py");
        assert_eq!(files[1].new_path, "main.rs");
    }

    #[test]
    fn test_detect_language_from_diff_path() {
        assert_eq!(detect_language_from_diff_path("src/main.rs"), "Rust");
        assert_eq!(detect_language_from_diff_path("app.py"), "Python");
        assert_eq!(detect_language_from_diff_path("main.ts"), "TypeScript");
        assert_eq!(detect_language_from_diff_path("README"), "Other");
    }
}
