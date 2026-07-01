//! Parses raw git diff output into structured DiffLine and DiffHunk representations.
//!
//! This module converts the text output from `git diff` into typed Rust structures
//! used by the review engine's diff filtering and chunking pipeline.

use crate::models::*;

pub fn parse_unified_diff(diff_text: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current_file: Option<DiffFile> = None;

    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            if let Some(file) = current_file.take() {
                files.push(file);
            }
            let path = parse_path_from_diff_header(line);
            current_file = Some(DiffFile {
                old_path: String::new(),
                new_path: String::new(),
                path,
                status: "modified".to_string(),
                additions: 0,
                deletions: 0,
                hunks: Vec::new(),
            });
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

fn parse_path_from_diff_header(line: &str) -> String {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 4 {
        let a_path = parts[2].trim_start_matches("a/");
        let b_path = parts[3].trim_start_matches("b/");
        let path = if b_path == "/dev/null" { a_path } else { b_path };
        path.to_string()
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
}
