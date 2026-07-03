use crate::diff::constants::DEFAULT_TOKEN_MODEL;
use crate::diff::render::render_file_diff;
use crate::models::DiffFile;
use crate::tokenizer::count_tokens;

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
        let file_text = render_file_diff(file);
        let file_tokens = match count_tokens(&file_text, DEFAULT_TOKEN_MODEL) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to count tokens for file budget; assuming 0");
                0
            }
        };
        if tokens_used + file_tokens > max_tokens {
            truncation_point = i;
            break;
        }
        tokens_used += file_tokens;
    }

    // Truncation: keep only files that fit within budget
    files.truncate(truncation_point);
}

/// Render a full diff text for a slice of files.
pub fn render_diff_text(files: &[DiffFile]) -> String {
    let mut out = String::new();
    for file in files {
        out.push_str(&render_file_diff(file));
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
    use crate::models::{DiffFile, DiffHunk, DiffLine, DiffLineKind};

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
        assert_eq!(files.len(), 1);
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
        apply_token_budget(&mut files, 1000);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_token_budget_exact_fit() {
        let file = make_file("exact.rs", vec![], 1, 0);
        let mut files = vec![file];
        apply_token_budget(&mut files, 1);
        assert!(files.len() <= 1);
    }

    #[test]
    fn test_token_budget_truncates_excess() {
        let file1 = make_file("a.rs", vec![], 1, 0);
        let file2 = make_file("b.rs", vec![], 1, 0);
        let mut files = vec![file1, file2];
        apply_token_budget(&mut files, 1);
        assert!(files.len() <= 2);
    }

    #[test]
    fn test_token_budget_multiple_files_order_preserved() {
        let file1 = make_file("aaa.rs", vec![], 1, 0);
        let file2 = make_file("bbb.rs", vec![], 1, 0);
        let file3 = make_file("ccc.rs", vec![], 1, 0);
        let mut files = vec![file1, file2, file3];
        let names_before: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        apply_token_budget(&mut files, 10000);
        let names_after: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(names_after.len() <= 3);
        assert!(names_after
            .windows(2)
            .all(|w| { names_before.iter().position(|n| n == w[0]) < names_before.iter().position(|n| n == w[1]) }));
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
        assert!(text.contains('+'));
        assert!(text.contains('-'));
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
        assert_eq!(files[0].hunks[0].lines[0].content.len(), 13);
    }

    #[test]
    fn test_truncate_long_lines_utf8_multibyte() {
        let content = "+".to_string() + &"😀".repeat(10);
        let mut files = vec![make_file(
            "utf8.rs",
            vec![make_hunk(vec![make_line(DiffLineKind::Add, &content)])],
            1,
            0,
        )];
        truncate_long_lines(&mut files, 10);
        let truncated = &files[0].hunks[0].lines[0].content;
        assert_eq!(
            truncated.len(),
            12,
            "expected '+😀😀...' length 12, got {:?}",
            truncated
        );
        assert!(truncated.starts_with('+'), "should start with '+': {:?}", truncated);
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
        assert_eq!(files[0].hunks[0].lines[0].content.len(), 23);
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
        assert_eq!(files[0].hunks[0].lines[0].content.len(), 8);
    }
}
