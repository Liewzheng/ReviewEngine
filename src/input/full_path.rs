//! Full-content directory review input (P0: `review --path <dir>`).
//!
//! A directory-level review treats every controlled file under `<dir>` as
//! *newly added code*: each file is rendered as a brand-new file in a
//! synthetic unified diff (`--- /dev/null`, entire content as `+` lines).
//! Feeding that diff through the existing review pipeline means every line
//! of every file is in-scope for chunking (with the large-PR coverage
//! guarantee), full-file-content injection, and finding validation — so no
//! file is silently collapsed out of the review. This is intentionally
//! distinct from `audit`/`repo-review`, which runs the whole-repository
//! static+LLM pipeline on the repository root.

use crate::models::DiffFile;
use anyhow::Result;
use std::path::{Component, Path};

/// Directories never descended into when walking a non-Git tree. Git
/// repositories are enumerated via `git ls-files --exclude-standard`, which
/// already applies `.gitignore`.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".venv",
    "venv",
    "vendor",
    "dist",
    "build",
    ".generated",
    "generated",
];

/// Result of building a full-content review input.
pub struct FullPathReview {
    /// Synthetic unified diff text ("empty tree → current" for every file).
    pub diff: String,
    /// Repository-relative paths of the files actually included in the diff.
    pub files: Vec<String>,
}

/// Build the synthetic full-content diff for every controlled file under
/// `dir` inside `repo_path`.
///
/// Fail-closed on every boundary: a missing/absent repo or target directory,
/// an empty or unreviewable directory, and unsafe paths all produce a clear
/// error instead of a silently empty review.
pub fn build_path_review_diff(repo_path: &str, dir: &str) -> Result<FullPathReview> {
    let repo = Path::new(repo_path);
    if !repo.exists() {
        anyhow::bail!("Repository path does not exist: {}", repo.display());
    }
    if !repo.is_dir() {
        anyhow::bail!("Repository path is not a directory: {}", repo.display());
    }

    if dir.trim().is_empty() {
        anyhow::bail!("review --path must be a non-empty relative directory inside --local-path");
    }
    if Path::new(dir).is_absolute() {
        anyhow::bail!(
            "review --path must be relative to --local-path, got absolute path '{}'",
            dir
        );
    }
    if Path::new(dir).components().any(|c| matches!(c, Component::ParentDir)) {
        anyhow::bail!(
            "review --path must not contain parent directory traversal ('..'): '{}'",
            dir
        );
    }

    let target = repo.join(dir);
    if !target.exists() {
        anyhow::bail!("Directory to review does not exist: {}", target.display());
    }
    if !target.is_dir() {
        anyhow::bail!("review --path must name a directory, not a file: {}", target.display());
    }

    let raw_files = list_controlled_files(repo, dir)?;

    // Keep only files the review pipeline will actually review: paths the
    // diff parser can represent and kinds `diff::filter` does not drop.
    let mut reviewed: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for rel in raw_files {
        let full = repo.join(&rel);
        // Symlinks can escape the reviewed tree; skip them (the non-Git walk
        // in `walk_files` already does this per-entry, but `git ls-files` lists
        // committed symlinks too, so re-check before every read — otherwise a
        // malicious repo could make us read a file outside `repo`).
        if let Ok(meta) = std::fs::symlink_metadata(&full) {
            if meta.file_type().is_symlink() {
                skipped.push(format!("{} (symlink)", rel));
                continue;
            }
        }
        if !is_parser_safe_path(&rel) {
            skipped.push(format!("{} (unsafe path)", rel));
            continue;
        }
        if filter_would_ignore(&rel) {
            skipped.push(format!("{} (ignored file kind)", rel));
            continue;
        }
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(e) => {
                anyhow::bail!("failed to read {}: {}", full.display(), e);
            }
        };
        match String::from_utf8(bytes) {
            Ok(text) => reviewed.push((rel, text)),
            Err(_) => skipped.push(format!("{} (non-UTF-8 content)", rel)),
        }
    }

    if reviewed.is_empty() {
        anyhow::bail!("no reviewable files found under '{}' in '{}'", dir, repo.display());
    }

    // Deterministic order: repository-relative path.
    reviewed.sort_by(|a, b| a.0.cmp(&b.0));

    if !skipped.is_empty() {
        let examples: Vec<&str> = skipped.iter().take(5).map(|s| s.as_str()).collect();
        tracing::warn!(
            "full-path review excluded {} file(s) under '{}' (examples: {})",
            skipped.len(),
            dir,
            examples.join(", ")
        );
    }

    let diff = reviewed
        .iter()
        .map(|(rel, content)| render_new_file_diff(rel, content))
        .collect::<String>();
    let files = reviewed.into_iter().map(|(rel, _)| rel).collect();

    Ok(FullPathReview { diff, files })
}

/// Enumerate controlled files under `dir` inside `repo`, repository-relative.
///
/// Git repositories use `git ls-files --cached --others --exclude-standard`
/// (tracked + untracked-not-ignored, `.gitignore`-aware). Non-Git trees are
/// walked with a small ignore set; symlinks are skipped to prevent escape.
fn list_controlled_files(repo: &Path, dir: &str) -> Result<Vec<String>> {
    if repo.join(".git").is_dir() {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
            .arg("--")
            .arg(dir)
            .output()
            .map_err(|e| anyhow::anyhow!("failed to run git ls-files: {}", e))?;
        if !output.status.success() {
            anyhow::bail!("git ls-files failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        let mut files = Vec::new();
        let mut start = 0;
        for (i, byte) in output.stdout.iter().enumerate() {
            if *byte == 0 {
                if i > start {
                    let rel = std::str::from_utf8(&output.stdout[start..i])
                        .map_err(|e| anyhow::anyhow!("invalid utf-8 in git ls-files output: {}", e))?;
                    files.push(rel.to_string());
                }
                start = i + 1;
            }
        }
        if start < output.stdout.len() {
            let rel = std::str::from_utf8(&output.stdout[start..])
                .map_err(|e| anyhow::anyhow!("invalid utf-8 in git ls-files output: {}", e))?;
            files.push(rel.to_string());
        }
        Ok(files)
    } else {
        walk_files(repo, dir)
    }
}

/// Recursively collect files under `dir` (relative to `repo`), skipping
/// ignored directories and symlinks.
fn walk_files(repo: &Path, dir: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![repo.join(dir)];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(rd) => rd,
            Err(e) => {
                tracing::warn!("cannot read directory {}: {}", current.display(), e);
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Symlinks can escape the reviewed tree; skip them.
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.file_type().is_symlink() {
                    continue;
                }
            }
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if IGNORED_DIRS.contains(&name) {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() {
                if let Some(rel) = path.strip_prefix(repo).ok().and_then(|p| p.to_str()) {
                    out.push(rel.replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// True when the diff parser can faithfully represent `rel` in a header
/// (`diff --git a/.. b/..` is split on whitespace; unsafe chars are rejected).
fn is_parser_safe_path(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('~') {
        return false;
    }
    // Reject parent-directory traversal by path *segment*, not by substring:
    // `foo..bar.rs` is a legal filename and must not be dropped from review.
    if Path::new(rel).components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    if rel.contains('\\') || rel.contains(':') || rel.contains('\0') {
        return false;
    }
    // Whitespace would split the `diff --git` header into more tokens.
    rel.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// True when `diff::filter::should_ignore` would drop the file anyway, so it
/// must not count towards "N files fully reviewed".
fn filter_would_ignore(rel: &str) -> bool {
    let dummy = DiffFile {
        old_path: rel.to_string(),
        new_path: rel.to_string(),
        path: rel.to_string(),
        status: "modified".to_string(),
        additions: 0,
        deletions: 0,
        hunks: Vec::new(),
    };
    crate::diff::filter::should_ignore(&dummy)
}

/// Render a single file as a brand-new file in a unified diff: full content
/// as additions, one hunk covering the whole file.
fn render_new_file_diff(rel: &str, content: &str) -> String {
    let body = content.strip_suffix('\n').unwrap_or(content);
    let lines: Vec<&str> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    let n = lines.len();
    let new_start = if n == 0 { 0 } else { 1 };

    let mut out = format!("diff --git a/{rel} b/{rel}\n");
    out.push_str("--- /dev/null\n");
    out.push_str(&format!("+++ b/{rel}\n"));
    out.push_str(&format!("@@ -0,0 +{new_start},{n} @@\n"));
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_git_repo(path: &Path) {
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .current_dir(path)
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
    }

    fn commit(path: &Path, files: &[(&str, &str)]) {
        for (rel, content) in files {
            let full = path.join(rel);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, content).unwrap();
        }
        let status = Command::new("git")
            .current_dir(path)
            .args(["add", "-A"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(path)
            .args(["commit", "-m", "test commit"])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn missing_repo_errors() {
        let err = build_path_review_diff("/definitely/not/a/repo-xyz", "src")
            .err()
            .expect("missing repo must fail");
        assert!(err.to_string().contains("Repository path does not exist"), "{err}");
    }

    #[test]
    fn missing_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = build_path_review_diff(dir.path().to_str().unwrap(), "nope")
            .err()
            .expect("missing dir must fail");
        assert!(err.to_string().contains("Directory to review does not exist"), "{err}");
    }

    #[test]
    fn traversal_and_absolute_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        for bad in ["../etc", "/etc", "sub/../../etc"] {
            let err = build_path_review_diff(dir.path().to_str().unwrap(), bad)
                .err()
                .expect("unsafe path must fail");
            assert!(
                err.to_string().contains("--path"),
                "unexpected message for {bad}: {err}"
            );
        }
    }

    #[test]
    fn empty_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        let err = build_path_review_diff(dir.path().to_str().unwrap(), "empty")
            .err()
            .expect("empty dir must fail");
        assert!(err.to_string().contains("no reviewable files"), "{err}");
    }

    #[test]
    fn git_repo_builds_full_content_diff() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit(
            dir.path(),
            &[
                ("src/a.rs", "fn a() {}\nfn second() {}\n"),
                ("src/b.rs", "fn b() {}\n"),
                ("src/logo.png", "not really a png\n"),
            ],
        );

        let review = build_path_review_diff(dir.path().to_str().unwrap(), "src").unwrap();

        assert_eq!(review.files, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
        assert!(review.diff.contains("diff --git a/src/a.rs b/src/a.rs"));
        assert!(review.diff.contains("--- /dev/null"));
        assert!(review.diff.contains("+++ b/src/a.rs"));
        assert!(review.diff.contains("@@ -0,0 +1,2 @@"));
        assert!(review.diff.contains("+fn a() {}"));
        assert!(review.diff.contains("+fn second() {}"));
        assert!(review.diff.contains("+fn b() {}"));
        assert!(!review.diff.contains("logo.png"));
    }

    /// Security regression: `git ls-files` lists committed symlinks, and the
    /// Git branch used to read them via `std::fs::read` — which follows the
    /// link, so a symlink pointing outside the repo would leak external file
    /// content into the review. The read path must skip symlinks (mirroring
    /// the non-Git walk).
    #[cfg(unix)]
    #[test]
    fn git_repo_skips_committed_symlink() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        // A secret *outside* the reviewed tree; the committed symlink must not
        // leak it.
        std::fs::write(dir.path().join("outside.txt"), "TOP-SECRET-OUTSIDE\n").unwrap();
        std::os::unix::fs::symlink("../outside.txt", dir.path().join("src/link.rs")).unwrap();
        std::fs::write(dir.path().join("src/real.rs"), "fn real() {}\n").unwrap();
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["add", "-A"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["commit", "-m", "add symlink"])
            .status()
            .unwrap();
        assert!(status.success());

        let review = build_path_review_diff(dir.path().to_str().unwrap(), "src").unwrap();
        assert_eq!(review.files, vec!["src/real.rs".to_string()]);
        assert!(
            !review.diff.contains("TOP-SECRET-OUTSIDE"),
            "symlink target content leaked into the review: {}",
            review.diff
        );
        assert!(
            !review.diff.contains("link.rs"),
            "symlink must be excluded: {}",
            review.diff
        );
    }

    /// Coverage regression: a legal filename containing `..` inside a single
    /// segment (`foo..bar.rs`) must not be dropped by the substring check.
    #[test]
    fn git_repo_accepts_double_dot_filename() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit(dir.path(), &[("src/foo..bar.rs", "fn foo() {}\n")]);

        let review = build_path_review_diff(dir.path().to_str().unwrap(), "src").unwrap();
        assert_eq!(review.files, vec!["src/foo..bar.rs".to_string()]);
        assert!(
            review.diff.contains("diff --git a/src/foo..bar.rs b/src/foo..bar.rs"),
            "double-dot filename must be reviewed: {}",
            review.diff
        );
    }

    #[test]
    fn gitignore_respected_and_no_trailing_newline_handled() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join(".gitignore"), "lib/*.ignored\n").unwrap();
        commit(
            dir.path(),
            &[
                ("lib/keep.c", "int main() { return 0; }"), // no trailing newline
                ("lib/skip.ignored", "nope\n"),
                ("lib/empty.c", ""),
            ],
        );

        let review = build_path_review_diff(dir.path().to_str().unwrap(), "lib").unwrap();

        assert_eq!(review.files, vec!["lib/empty.c".to_string(), "lib/keep.c".to_string()]);
        // No trailing newline → one line.
        assert!(review.diff.contains("@@ -0,0 +1,1 @@"), "{}", review.diff);
        assert!(review.diff.contains("+int main() { return 0; }"));
        // Empty file → zero-line hunk, present in the file list.
        assert!(review.diff.contains("@@ -0,0 +0,0 @@"), "{}", review.diff);
        assert!(!review.diff.contains("skip.ignored"));
    }

    #[test]
    fn non_git_dir_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/deep/helper.rs"), "fn helper() {}\n").unwrap();
        std::fs::write(dir.path().join("src/logo.png"), "binary").unwrap();
        std::fs::create_dir_all(dir.path().join("src/target")).unwrap();
        std::fs::write(dir.path().join("src/target/out.rs"), "fn out() {}\n").unwrap();

        let review = build_path_review_diff(dir.path().to_str().unwrap(), "src").unwrap();

        assert_eq!(
            review.files,
            vec!["src/deep/helper.rs".to_string(), "src/main.rs".to_string(),]
        );
        assert!(!review.diff.contains("logo.png"));
        assert!(!review.diff.contains("target/out.rs"));
    }

    #[test]
    fn crlf_lines_are_stripped() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit(dir.path(), &[("win/foo.c", "int a;\r\nint b;\r\n")]);

        let review = build_path_review_diff(dir.path().to_str().unwrap(), "win").unwrap();
        assert_eq!(review.files, vec!["win/foo.c".to_string()]);
        assert!(review.diff.contains("+int a;"), "{}", review.diff);
        assert!(review.diff.contains("+int b;"), "{}", review.diff);
        assert!(!review.diff.contains("\r"), "CR must be stripped: {}", review.diff);
    }

    #[test]
    fn file_is_rejected_as_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.c"), "int x;\n").unwrap();
        let err = build_path_review_diff(dir.path().to_str().unwrap(), "a.c")
            .err()
            .expect("a file path must fail");
        assert!(err.to_string().contains("must name a directory"), "{err}");
    }
}
