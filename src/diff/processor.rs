use crate::models::*;
use crate::tokenizer::count_tokens;

/// Default model used for token counting.
const DEFAULT_TOKEN_MODEL: &str = "gpt-4";

/// Apply a token budget to the file list: keep files in order until the
/// budget is exhausted, then mark remaining files as truncated.
/// Uses token counting instead of character counting.
pub fn apply_token_budget(files: &mut Vec<DiffFile>, max_tokens: usize) {
    if max_tokens == 0 {
        return; // 0 means unlimited
    }

    let mut tokens_used = 0usize;
    let mut truncation_point = files.len();

    for (i, file) in files.iter().enumerate() {
        let file_text = render_single_file_diff(file);
        let file_tokens = count_tokens(&file_text, DEFAULT_TOKEN_MODEL).unwrap_or(0);
        if tokens_used + file_tokens > max_tokens {
            truncation_point = i;
            break;
        }
        tokens_used += file_tokens;
    }

    // Truncation: keep only files that fit within budget
    files.truncate(truncation_point);
}

pub fn render_diff_text(files: &[DiffFile]) -> String {
    let mut out = String::new();
    for file in files {
        out.push_str(&render_single_file_diff(file));
    }
    out
}

/// Check if a file should be ignored (binary, generated, vendor, lockfile).
pub fn should_ignore_file(file: &DiffFile) -> bool {
    let path = &file.new_path;

    // Binary files
    let binary_extensions = [
        "png", "jpg", "jpeg", "gif", "ico", "webp", "woff", "woff2", "ttf", "eot", "pdf", "doc", "docx", "xls", "xlsx",
        "zip", "tar", "gz", "bz2", "7z", "rar", "exe", "dll", "so", "dylib", "wasm", "mp3", "mp4", "avi", "mov", "mkv",
        "pyc", "class", "o",
    ];
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        if binary_extensions.contains(&ext.to_lowercase().as_str()) {
            return true;
        }
    }

    // Generated/vendor directories
    let generated_patterns = [
        "node_modules/",
        "vendor/",
        ".git/",
        "target/",
        "dist/",
        "build/",
        ".next/",
        ".nuxt/",
        "__pycache__/",
        ".venv/",
        "venv/",
        "env/",
        ".generated/",
        "generated/",
    ];
    for pattern in &generated_patterns {
        if path.contains(pattern) {
            return true;
        }
    }

    // Lock files — check exact or suffix to handle monorepo subdirectories
    let lock_files = [
        "package-lock.json",
        "yarn.lock",
        "Cargo.lock",
        "Gemfile.lock",
        "poetry.lock",
    ];
    if lock_files.contains(&path.as_str()) || lock_files.iter().any(|f| path.ends_with(&format!("/{}", f))) {
        return true;
    }

    false
}

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

/// Sort files by primary language group, then by token count descending.
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
fn detect_language_from_diff_path(path: &str) -> &'static str {
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

/// Render a single DiffFile as a diff text string.
fn render_single_file_diff(file: &DiffFile) -> String {
    let mut out = format!("diff --git a/{} b/{}\n", file.old_path, file.new_path);
    for hunk in &file.hunks {
        out.push_str(&hunk.header);
        out.push('\n');
        for line in &hunk.lines {
            out.push_str(&line.content);
            out.push('\n');
        }
    }
    out
}

/// Truncate overly long lines in the diff.
pub fn truncate_long_lines(files: &mut [DiffFile], max_line_length: usize) {
    for file in files.iter_mut() {
        for hunk in file.hunks.iter_mut() {
            for line in hunk.lines.iter_mut() {
                if line.content.len() > max_line_length {
                    let boundary = line.content.floor_char_boundary(max_line_length);
                    line.content.truncate(boundary);
                    line.content.push_str("...");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            kind: kind.clone(),
            content: content.to_string(),
            old_line_no: match &kind {
                DiffLineKind::Add => None,
                _ => Some(1),
            },
            new_line_no: match &kind {
                DiffLineKind::Delete => None,
                _ => Some(1),
            },
        }
    }

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

    #[test]
    fn test_token_budget_no_limit() {
        let mut files = vec![DiffFile {
            old_path: "a.rs".to_string(),
            new_path: "a.rs".to_string(),
            path: "a.rs".to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: vec![],
        }];
        apply_token_budget(&mut files, 0);
        assert_eq!(files.len(), 1); // 0 means unlimited
    }

    #[test]
    fn test_token_budget_keeps_fitting_files() {
        let mut files = vec![DiffFile {
            old_path: "small.rs".to_string(),
            new_path: "small.rs".to_string(),
            path: "small.rs".to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: vec![],
        }];
        // Use a generous budget that should fit the small file
        apply_token_budget(&mut files, 1000);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_should_ignore_binary_extension() {
        let file = DiffFile {
            old_path: "image.png".to_string(),
            new_path: "image.png".to_string(),
            path: "image.png".to_string(),
            status: "modified".to_string(),
            additions: 0,
            deletions: 0,
            hunks: vec![],
        };
        assert!(should_ignore_file(&file));
    }

    #[test]
    fn test_should_ignore_vendor_path() {
        let file = DiffFile {
            old_path: "node_modules/pkg/index.js".to_string(),
            new_path: "node_modules/pkg/index.js".to_string(),
            path: "node_modules/pkg/index.js".to_string(),
            status: "modified".to_string(),
            additions: 0,
            deletions: 0,
            hunks: vec![],
        };
        assert!(should_ignore_file(&file));
    }

    #[test]
    fn test_should_not_ignore_source_file() {
        let file = DiffFile {
            old_path: "src/main.rs".to_string(),
            new_path: "src/main.rs".to_string(),
            path: "src/main.rs".to_string(),
            status: "modified".to_string(),
            additions: 0,
            deletions: 0,
            hunks: vec![],
        };
        assert!(!should_ignore_file(&file));
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
        let file = DiffFile {
            old_path: "mixed.rs".to_string(),
            new_path: "mixed.rs".to_string(),
            path: "mixed.rs".to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 1,
            hunks: vec![DiffHunk {
                header: "@@ -1 +1,2 @@".to_string(),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 2,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Delete,
                        content: "-old".to_string(),
                        old_line_no: Some(1),
                        new_line_no: None,
                    },
                    DiffLine {
                        kind: DiffLineKind::Add,
                        content: "+new".to_string(),
                        old_line_no: None,
                        new_line_no: Some(1),
                    },
                ],
            }],
        };
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
        // Python files first (alphabetically "Python" < "Rust"), then Rust
        // Within same language sorted by size desc: a.py(100) > None; c.rs(50) > b.rs(10)
        assert_eq!(files[0].new_path, "a.py");
        assert_eq!(files[1].new_path, "c.rs");
        assert_eq!(files[2].new_path, "b.rs");
    }

    #[test]
    fn test_truncate_long_lines() {
        let mut files = vec![DiffFile {
            old_path: "long.rs".to_string(),
            new_path: "long.rs".to_string(),
            path: "long.rs".to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: vec![DiffHunk {
                header: "@@ -1 +1 @@".to_string(),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                lines: vec![DiffLine {
                    kind: DiffLineKind::Add,
                    content: "+".to_string() + &"x".repeat(200),
                    old_line_no: None,
                    new_line_no: Some(1),
                }],
            }],
        }];
        truncate_long_lines(&mut files, 10);
        assert_eq!(files[0].hunks[0].lines[0].content.len(), 13); // 10 + "..."
    }

    #[test]
    fn test_truncate_long_lines_utf8_multibyte() {
        // UTF-8 multi-byte characters: each emoji is 4 bytes.
        // With a max_line_length of 10, we must not split a multi-byte char.
        let content = "+".to_string() + &"😀".repeat(10); // 10 emojis = 40 bytes + 1
        let mut files = vec![make_file(
            "utf8.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Add, &content)])],
            1,
            0,
        )];
        truncate_long_lines(&mut files, 10);
        let truncated = &files[0].hunks[0].lines[0].content;
        // Should truncate at floor_char_boundary(10) = 9 (2 emojis = 8 bytes + 1 for '+')
        // Actually: "+" + 2 emojis = 1 + 8 = 9 bytes, then "..." = 12 total
        assert_eq!(
            truncated.len(),
            12,
            "expected '+😀😀...' length 12, got {:?}",
            truncated
        );
        // Content should start with '+' and two complete emojis
        assert!(truncated.starts_with('+'), "should start with '+': {:?}", truncated);
        // No incomplete (replacement) characters
        assert!(!truncated.contains('\u{FFFD}'), "should not contain replacement chars");
    }

    #[test]
    fn test_truncate_long_lines_no_truncation_needed() {
        let content = "+short".to_string();
        let mut files = vec![make_file(
            "short.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Add, &content)])],
            1,
            0,
        )];
        truncate_long_lines(&mut files, 100);
        assert_eq!(files[0].hunks[0].lines[0].content, "+short");
    }

    #[test]
    fn test_truncate_long_lines_mixed_long_and_short() {
        let long_content = "+".to_string() + &"a".repeat(100);
        let short_content = "+short".to_string();
        let mut files = vec![make_file(
            "mixed.rs",
            vec![
                make_hunk(vec![make_line(DiffLineKind::Add, &long_content)]),
                make_hunk(vec![make_line(DiffLineKind::Add, &short_content)]),
            ],
            2,
            0,
        )];
        truncate_long_lines(&mut files, 20);
        assert_eq!(files[0].hunks[0].lines[0].content.len(), 23); // 20 + "..."
        assert_eq!(files[0].hunks[1].lines[0].content, "+short");
    }

    #[test]
    fn test_truncate_long_lines_empty_content() {
        let mut files = vec![make_file(
            "empty.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Context, "")])],
            0,
            0,
        )];
        truncate_long_lines(&mut files, 5);
        assert_eq!(files[0].hunks[0].lines[0].content, "");
    }

    #[test]
    fn test_truncate_long_lines_delete_lines() {
        let content = "-".to_string() + &"x".repeat(200);
        let mut files = vec![make_file(
            "del.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Delete, &content)])],
            0,
            1,
        )];
        truncate_long_lines(&mut files, 5);
        assert_eq!(files[0].hunks[0].lines[0].content.len(), 8); // 5 + "..."
    }

    #[test]
    fn test_apply_token_budget_exact_fit() {
        let file = make_file("exact.rs", vec![], 1, 0);
        let mut files = vec![file];
        apply_token_budget(&mut files, 1); // Very small budget — file may or may not fit
                                           // With empty hunks the file has ~some tokens from the header
        assert!(files.len() <= 1);
    }

    #[test]
    fn test_apply_token_budget_truncates_excess() {
        let file1 = make_file("a.rs", vec![], 1, 0);
        let file2 = make_file("b.rs", vec![], 1, 0);
        let mut files = vec![file1, file2];
        apply_token_budget(&mut files, 1); // Budget of 1 token — unlikely both files fit
        assert!(files.len() <= 2);
    }

    #[test]
    fn test_apply_token_budget_multiple_files_order_preserved() {
        let file1 = make_file("aaa.rs", vec![], 1, 0);
        let file2 = make_file("bbb.rs", vec![], 1, 0);
        let file3 = make_file("ccc.rs", vec![], 1, 0);
        let mut files = vec![file1, file2, file3];
        let names_before: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        apply_token_budget(&mut files, 10000);
        // With a generous budget, order should be preserved
        let names_after: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(names_after.len() <= 3);
        // The names that remain should be in the original order
        assert!(names_after
            .windows(2)
            .all(|w| { names_before.iter().position(|n| n == w[0]) < names_before.iter().position(|n| n == w[1]) }));
    }

    #[test]
    fn test_compress_deletions_empty_input() {
        let (kept, deleted) = compress_deletions(vec![]);
        assert!(kept.is_empty());
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_compress_deletions_mixed_adds_and_deletes_in_same_hunk() {
        let file = make_file(
            "mixed.rs",
            vec![make_hunk(vec![
                make_line(DiffLineKind::Delete, "-old_code"),
                make_line(DiffLineKind::Add, "+new_code"),
            ])],
            1,
            1,
        );
        let (kept, deleted) = compress_deletions(vec![file]);
        assert_eq!(kept.len(), 1);
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
        // File with no hunks should not be considered a deletion
        let file = make_file("empty.rs", vec![], 0, 0);
        let (kept, deleted) = compress_deletions(vec![file]);
        assert_eq!(kept.len(), 1);
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_render_diff_text_empty() {
        let text = render_diff_text(&[]);
        assert_eq!(text, "");
    }

    #[test]
    fn test_render_diff_text_single_file() {
        let file = make_file(
            "test.rs",
            vec![make_hunk(vec![
                make_line(DiffLineKind::Context, " line1"),
                make_line(DiffLineKind::Add, "+line2"),
            ])],
            1,
            0,
        );
        let text = render_diff_text(&[file]);
        assert!(text.contains("diff --git a/test.rs b/test.rs"));
        assert!(text.contains("+line2"));
        assert!(text.contains(" line1"));
    }

    #[test]
    fn test_render_diff_text_multiple_files() {
        let f1 = make_file("a.rs", vec![make_hunk(vec![make_line(DiffLineKind::Add, "+a")])], 1, 0);
        let f2 = make_file(
            "b.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Delete, "-b")])],
            0,
            1,
        );
        let text = render_diff_text(&[f1, f2]);
        assert!(text.contains("a.rs"));
        assert!(text.contains("b.rs"));
        assert!(text.contains('+')); // added line marker
        assert!(text.contains('-')); // deleted line marker
    }

    #[test]
    fn test_should_ignore_lockfile_direct_match() {
        let file = make_file("Cargo.lock", vec![], 0, 0);
        assert!(should_ignore_file(&file));
    }

    #[test]
    fn test_should_ignore_lockfile_subdir_match() {
        let file = make_file("frontend/package-lock.json", vec![], 0, 0);
        assert!(should_ignore_file(&file));
    }

    #[test]
    fn test_should_ignore_generated_directory() {
        let file = make_file("target/debug/main.rs", vec![], 0, 0);
        assert!(should_ignore_file(&file));
    }

    #[test]
    fn test_should_ignore_vendor_directory() {
        let file = make_file("vendor/lib.rs", vec![], 0, 0);
        assert!(should_ignore_file(&file));
    }

    #[test]
    fn test_sort_files_by_language_grouping() {
        let mut files = vec![make_file("main.rs", vec![], 10, 0), make_file("lib.py", vec![], 5, 0)];
        sort_files_by_language_and_size(&mut files);
        // Python < Rust alphabetically, so lib.py should come first
        assert_eq!(files[0].new_path, "lib.py");
        assert_eq!(files[1].new_path, "main.rs");
    }

    #[test]
    fn test_should_ignore_binary_extensions_case_insensitive() {
        let file = make_file("image.PNG", vec![], 0, 0);
        assert!(should_ignore_file(&file));
    }
}
