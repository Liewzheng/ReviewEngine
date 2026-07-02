use crate::models::{DiffFile, DiffHunk};

/// Render a single DiffFile as a diff text string.
pub fn render_file_diff(file: &DiffFile) -> String {
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

/// Render a single DiffHunk as a diff text string.
pub fn render_hunk(hunk: &DiffHunk) -> String {
    let mut out = hunk.header.clone();
    out.push('\n');
    for line in &hunk.lines {
        out.push_str(&line.content);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DiffFile, DiffHunk, DiffLine, DiffLineKind};

    fn make_line(kind: DiffLineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_string(),
            old_line_no: None,
            new_line_no: None,
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

    fn make_file(path: &str, hunks: Vec<DiffHunk>) -> DiffFile {
        DiffFile {
            old_path: path.to_string(),
            new_path: path.to_string(),
            path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks,
        }
    }

    #[test]
    fn test_render_file_diff_single_file() {
        let file = make_file(
            "test.rs",
            vec![make_hunk(vec![
                make_line(DiffLineKind::Context, " line1"),
                make_line(DiffLineKind::Add, "+line2"),
            ])],
        );
        let text = render_file_diff(&file);
        assert!(text.contains("diff --git a/test.rs b/test.rs"));
        assert!(text.contains("+line2"));
        assert!(text.contains(" line1"));
    }

    #[test]
    fn test_render_hunk() {
        let hunk = make_hunk(vec![make_line(DiffLineKind::Add, "+added")]);
        let text = render_hunk(&hunk);
        assert!(text.contains("@@ -1 +1 @@"));
        assert!(text.contains("+added"));
    }
}
