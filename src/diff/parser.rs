//! Parses raw git diff output into structured DiffLine and DiffHunk representations.
//!
//! This module converts the text output from `git diff` into typed Rust structures
//! used by the review engine's diff filtering and chunking pipeline.

use crate::models::*;

/// Maximum allowed diff text size in bytes (10 MiB) to prevent memory DoS.
const MAX_DIFF_SIZE: usize = 10 * 1024 * 1024;

/// Parse a unified diff string into structured `DiffFile` representations.
///
/// Returns an empty vec if `diff_text` exceeds `MAX_DIFF_SIZE`.
/// Validates file paths to reject path-traversal sequences.
pub fn parse_unified_diff(diff_text: &str) -> Vec<DiffFile> {
    if diff_text.len() > MAX_DIFF_SIZE {
        tracing::warn!("diff text exceeds {} bytes, returning empty parse", MAX_DIFF_SIZE);
        return Vec::new();
    }

    let mut files = Vec::new();
    let mut current_file: Option<DiffFile> = None;

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            if let Some(file) = current_file.take() {
                files.push(file);
            }
            let path = parse_path_from_diff_header(line);
            if is_safe_diff_path(&path) {
                current_file = Some(DiffFile {
                    old_path: String::new(),
                    new_path: String::new(),
                    path,
                    status: "modified".to_string(),
                    additions: 0,
                    deletions: 0,
                    hunks: Vec::new(),
                });
            } else {
                tracing::warn!("diff contains unsafe path: {}", path);
                // Skip this file by leaving current_file as None
            }
        } else if let Some(ref mut file) = current_file {
            if line.starts_with("--- ") && file.old_path.is_empty() {
                let raw = &line[4..];
                file.old_path = raw.trim_start_matches("a/").to_string();
            } else if line.starts_with("+++ ") && file.new_path.is_empty() {
                let raw = &line[4..];
                file.new_path = raw.trim_start_matches("b/").to_string();
            } else if line.starts_with("@@") {
                let hunk = parse_hunk_header(line);
                file.hunks.push(DiffHunk {
                    header: line.to_string(),
                    old_start: hunk.0,
                    old_lines: hunk.1,
                    new_start: hunk.2,
                    new_lines: hunk.3,
                    lines: Vec::new(),
                });
            } else if let Some(hunk) = file.hunks.last_mut() {
                let kind = if line.starts_with('+') {
                    file.additions += 1;
                    DiffLineKind::Add
                } else if line.starts_with('-') {
                    file.deletions += 1;
                    DiffLineKind::Delete
                } else {
                    DiffLineKind::Context
                };
                hunk.lines.push(DiffLine {
                    kind,
                    content: line.to_string(),
                    old_line_no: None,
                    new_line_no: None,
                });
            }
        }
    }

    if let Some(file) = current_file.take() {
        files.push(file);
    }

    files
}

/// Returns true if a path extracted from a diff header is safe.
fn is_safe_diff_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.starts_with('~') {
        return false;
    }
    if path.contains("..") || path.contains("\\") || path.contains(':') || path.contains('\0') {
        return false;
    }
    true
}

fn parse_path_from_diff_header(line: &str) -> String {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 4 {
        let a_path = parts[2].trim_start_matches("a/");
        let b_path = parts[3].trim_start_matches("b/");
        let path = if b_path == "/dev/null" { a_path } else { b_path };
        let path = path.to_string();
        // Defensive: reject paths that could be used for traversal
        if path.contains("..") || path.starts_with('/') {
            return String::new();
        }
        path
    } else {
        "unknown".to_string()
    }
}

fn parse_hunk_header(line: &str) -> (u32, u32, u32, u32) {
    let line = line.trim_start_matches("@@");
    let line = line.split("@@").next().unwrap_or("").trim();
    let parts: Vec<&str> = line.split_whitespace().collect();
    let old = parts.first().unwrap_or(&"-0,0");
    let new = parts.get(1).unwrap_or(&"+0,0");

    let old = old.trim_start_matches('-');
    let new = new.trim_start_matches('+');

    let old_parts: Vec<&str> = old.split(',').collect();
    let new_parts: Vec<&str> = new.split(',').collect();

    let old_start = old_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let old_lines = old_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let new_start = new_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let new_lines = new_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);

    (old_start, old_lines, new_start, new_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_diff() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
                    --- a/src/main.rs\n\
                    +++ b/src/main.rs\n\
                    @@ -1,3 +1,4 @@\n\
                     fn main() {\n\
                    -    println!(\"old\");\n\
                    +    println!(\"new\");\n\
                    +    println!(\"added\");\n\
                     }";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].old_path, "src/main.rs");
        assert_eq!(files[0].new_path, "src/main.rs");
        assert_eq!(files[0].additions, 2);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[0].hunks.len(), 1);
    }

    #[test]
    fn test_parse_with_old_new_paths() {
        let diff = "diff --git a/src/old.rs b/src/new.rs\n\
                    --- a/src/old.rs\n\
                    +++ b/src/new.rs\n\
                    @@ -1,3 +1,3 @@\n\
                     same\n\
                    -old\n\
                    +new\n\
                     same";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].old_path, "src/old.rs");
        assert_eq!(files[0].new_path, "src/new.rs");
    }

    #[test]
    fn test_parse_new_file_dev_null() {
        let diff = "diff --git a/src/new.rs b/src/new.rs\n\
                    --- /dev/null\n\
                    +++ b/src/new.rs\n\
                    @@ -0,0 +1,3 @@\n\
                    +fn main() {\n\
                    +    println!(\"new\");\n\
                    +}";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].old_path, "/dev/null");
        assert_eq!(files[0].new_path, "src/new.rs");
        assert_eq!(files[0].additions, 3);
    }

    #[test]
    fn test_parse_deleted_file_dev_null() {
        let diff = "diff --git a/src/old.rs b/src/old.rs\n\
                    --- a/src/old.rs\n\
                    +++ /dev/null\n\
                    @@ -1,3 +0,0 @@\n\
                    -fn main() {\n\
                    -    println!(\"old\");\n\
                    -}";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/old.rs");
        assert_eq!(files[0].new_path, "/dev/null");
    }

    #[test]
    fn test_parse_modified_file_with_multiple_hunks() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
                    --- a/src/lib.rs\n\
                    +++ b/src/lib.rs\n\
                    @@ -10,3 +10,4 @@\n\
                     a\n\
                    -b\n\
                    +c\n\
                     d\n\
                    @@ -30,5 +30,5 @@\n\
                     x\n\
                    -y\n\
                    +z\n\
                     w\n\
                     v";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].old_path, "src/lib.rs");
        assert_eq!(files[0].new_path, "src/lib.rs");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].old_start, 10);
        assert_eq!(files[0].hunks[0].old_lines, 3);
        assert_eq!(files[0].hunks[0].new_start, 10);
        assert_eq!(files[0].hunks[0].new_lines, 4);
        assert_eq!(files[0].hunks[1].old_start, 30);
        assert_eq!(files[0].hunks[1].old_lines, 5);
        assert_eq!(files[0].hunks[1].new_start, 30);
        assert_eq!(files[0].hunks[1].new_lines, 5);
        assert_eq!(files[0].additions, 2);
        assert_eq!(files[0].deletions, 2);
    }

    #[test]
    fn test_parse_new_file_line_range() {
        let diff = "diff --git a/src/new.rs b/src/new.rs\n\
                    --- /dev/null\n\
                    +++ b/src/new.rs\n\
                    @@ -0,0 +1,5 @@\n\
                    +fn main() {\n\
                    +    println!(\"hello\");\n\
                    +}";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0].old_start, 0);
        assert_eq!(files[0].hunks[0].old_lines, 0);
        assert_eq!(files[0].hunks[0].new_start, 1);
        assert_eq!(files[0].hunks[0].new_lines, 5);
    }

    #[test]
    fn test_parse_deleted_file_line_range() {
        let diff = "diff --git a/src/old.rs b/src/old.rs\n\
                    --- a/src/old.rs\n\
                    +++ /dev/null\n\
                    @@ -1,4 +0,0 @@\n\
                    -fn main() {\n\
                    -    println!(\"old\");\n\
                    -}";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0].old_start, 1);
        assert_eq!(files[0].hunks[0].old_lines, 4);
        assert_eq!(files[0].hunks[0].new_start, 0);
        assert_eq!(files[0].hunks[0].new_lines, 0);
    }

    #[test]
    fn test_is_safe_diff_path_rejects_traversal() {
        assert!(!is_safe_diff_path("../etc/passwd"));
        assert!(!is_safe_diff_path("/etc/passwd"));
        assert!(!is_safe_diff_path("foo/../bar"));
        assert!(!is_safe_diff_path("foo\\bar"));
        assert!(!is_safe_diff_path("foo:bar"));
        assert!(!is_safe_diff_path("~/.ssh/id_rsa"));
        assert!(!is_safe_diff_path("foo\0bar"));
    }

    #[test]
    fn test_is_safe_diff_path_accepts_valid() {
        assert!(is_safe_diff_path("src/main.rs"));
        assert!(is_safe_diff_path("a/b/c.txt"));
        assert!(is_safe_diff_path(".gitignore"));
    }

    #[test]
    fn test_parse_unified_diff_skips_unsafe_paths() {
        let diff = "diff --git a/../etc/passwd b/../etc/passwd\n\
                    --- a/../etc/passwd\n\
                    +++ b/../etc/passwd\n\
                    @@ -1,3 +1,4 @@\n\
                     root:x:0:0\n\
                    +injected\n\
                     same";
        let files = parse_unified_diff(diff);
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_max_size() {
        let huge_diff = "a".repeat(MAX_DIFF_SIZE + 1);
        let files = parse_unified_diff(&huge_diff);
        assert!(files.is_empty());
    }
}
