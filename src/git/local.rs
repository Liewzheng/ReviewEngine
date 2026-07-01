use crate::models::RepoBrowser;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tokio::process::Command;

async fn run_git(cmd: &mut Command) -> anyhow::Result<String> {
    let output = cmd.output().await?;
    if !output.status.success() {
        anyhow::bail!("git command failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(crate) fn validate_ref(ref_name: &str) -> anyhow::Result<&str> {
    if ref_name.is_empty() {
        anyhow::bail!("git ref must not be empty");
    }
    if ref_name.starts_with('-') {
        anyhow::bail!("git ref must not start with '-' to prevent flag injection");
    }
    let forbidden = [';', '|', '&', '`', '$', '(', ')', '{', '}', '<', '>', '!', '\n', '\r'];
    if ref_name.contains(forbidden) {
        anyhow::bail!("git ref '{}' contains forbidden shell metacharacters", ref_name);
    }
    if ref_name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        anyhow::bail!("git ref '{}' contains whitespace or control characters", ref_name);
    }
    Ok(ref_name)
}

pub(crate) fn validate_path(path: &str) -> anyhow::Result<&str> {
    if path.is_empty() {
        anyhow::bail!("path must not be empty");
    }
    for component in Path::new(path).components() {
        match component {
            std::path::Component::ParentDir => {
                anyhow::bail!("path must not contain parent directory traversal ('..')");
            }
            std::path::Component::RootDir => {
                anyhow::bail!("path must not be absolute");
            }
            _ => {}
        }
    }
    Ok(path)
}

/// A Git repository browser that executes local `git` commands.
///
/// Wraps a local repository path and provides diff extraction, file
/// content retrieval, and commit log access by shelling out to `git`.
/// All ref and path inputs are validated for safety before execution.
pub struct LocalGitBrowser {
    /// Path to the local Git repository root.
    pub repo_path: PathBuf,
}

impl LocalGitBrowser {
    /// Create a new `LocalGitBrowser` for the repository at the given path.
    pub fn new(path: &str) -> Self {
        Self {
            repo_path: PathBuf::from(path),
        }
    }

    /// Get the diff between two refs (or a commit range).
    ///
    /// If `staged` is true, returns the staged diff (`--cached`).
    /// If `since` is provided, returns the diff from `since..HEAD`
    /// (or `since..until` if `until` is also given).
    /// Otherwise returns the diff between `base_ref` and `head_ref`.
    pub async fn get_diff(
        &self,
        base_ref: &str,
        head_ref: Option<&str>,
        staged: bool,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo_path).arg("diff");

        if staged {
            cmd.arg("--cached");
        }

        if let Some(ref since) = since {
            let since = validate_ref(since)?;
            let range = match until {
                Some(until) => {
                    let until = validate_ref(until)?;
                    format!("{}..{}", since, until)
                }
                None => format!("{}..HEAD", since),
            };
            cmd.arg(&range).arg("--");
        } else {
            let base_ref = validate_ref(base_ref)?;
            cmd.arg(base_ref);
            if let Some(ref head) = head_ref {
                let head = validate_ref(head)?;
                cmd.arg(head);
            }
            cmd.arg("--");
        }

        run_git(&mut cmd).await
    }

    /// Read the contents of a file at a specific Git ref.
    ///
    /// Uses `git show <ref>:<path>` to retrieve the file content.
    /// Both `git_ref` and `path` are validated for safety.
    pub async fn get_file_content(&self, path: &str, git_ref: &str) -> Result<String> {
        let git_ref = validate_ref(git_ref)?;
        let path = validate_path(path)?;

        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.repo_path)
            .arg("show")
            .arg("--")
            .arg(format!("{}:{}", git_ref, path));
        run_git(&mut cmd).await
    }

    /// Find test files related to a given source file.
    ///
    /// Strategies:
    /// 1. Same directory: `foo.rs` → `foo_test.rs`, `foo.test.rs`
    /// 2. Mirror directory: `src/foo.rs` → `tests/foo.rs`, `tests/foo_test.rs`
    /// 3. Checks if the file itself is a test file (ends with _test/test_)
    pub fn get_related_tests(&self, file: &str) -> Result<Vec<String>> {
        let path = Path::new(file);
        let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");

        let mut candidates = Vec::new();

        // Strategy 1: foo → foo_test.xxx (same dir)
        if let Some(parent) = path.parent() {
            for suffix in &["_test", ".test"] {
                let test_name = format!("{}{}", file_stem, suffix);
                if extension.is_empty() {
                    candidates.push(parent.join(test_name));
                } else {
                    candidates.push(parent.join(format!("{}.{}", test_name, extension)));
                }
            }
        }

        // Strategy 2: tests/ directory mirror (cross-platform)
        // src/foo.rs → tests/foo.rs, src/foo/bar.rs → tests/foo/bar.rs
        // lib/bar.rs → tests/bar.rs
        if let Some(first) = path.components().next().and_then(|c| c.as_os_str().to_str()) {
            if first == "src" || first == "lib" {
                let rest = path.strip_prefix(first).unwrap_or(path);
                let mirrored = Path::new("tests").join(rest);
                candidates.push(mirrored.to_path_buf());
                if let Some(parent) = mirrored.parent() {
                    let test_name = format!("{}_{}", file_stem, "test");
                    if extension.is_empty() {
                        candidates.push(parent.join(test_name));
                    } else {
                        candidates.push(parent.join(format!("{}.{}", test_name, extension)));
                    }
                }
            }
        }

        // Filter to existing files only
        let existing: Vec<String> = candidates
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|p| self.repo_path.join(p).exists())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        Ok(existing)
    }

    /// Get commit history for a file.
    /// Returns `Vec<(commit_sha, subject_line)>` ordered newest-first.
    pub fn get_file_history(&self, path: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let output = std::process::Command::new("git")
            .args(["-C", &self.repo_path.to_string_lossy()])
            .args(["log", "--oneline", &format!("-{}", limit), "--", path])
            .output()
            .map_err(|e| anyhow::anyhow!("failed to run git log: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git log failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut history = Vec::new();

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Format: "abc1234 Commit subject line"
            if let Some(space_pos) = line.find(' ') {
                let sha = line[..space_pos].to_string();
                let subject = line[space_pos + 1..].to_string();
                history.push((sha, subject));
            } else {
                history.push((line.to_string(), String::new()));
            }
        }

        Ok(history)
    }
}

impl RepoBrowser for LocalGitBrowser {
    fn get_file(&self, path: &str, git_ref: &str) -> anyhow::Result<String> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.get_file_content(path, git_ref))
    }

    fn search_code(&self, query: &str) -> anyhow::Result<Vec<String>> {
        let output = std::process::Command::new("rg")
            .arg("-l")
            .arg("--")
            .arg(query)
            .arg(&self.repo_path)
            .output()?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.lines().map(|s| s.to_string()).collect())
        } else if output.status.code() == Some(1) {
            // ripgrep exits 1 when no matches are found.
            tracing::debug!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "rg returned exit code 1 (no matches)"
            );
            Ok(Vec::new())
        } else {
            tracing::debug!(
                stderr = %String::from_utf8_lossy(&output.stderr),
                "rg search failed"
            );
            anyhow::bail!("rg search failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_git_repo() -> (tempfile::TempDir, LocalGitBrowser) {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let browser = LocalGitBrowser::new(tmp.path().to_str().unwrap());

        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };

        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);

        std::fs::write(tmp.path().join("file.txt"), "first\n").expect("failed to write file");
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "first"]);

        std::fs::write(tmp.path().join("file.txt"), "second\n").expect("failed to write file");
        run(&["add", "file.txt"]);
        run(&["commit", "-m", "second"]);

        (tmp, browser)
    }

    fn init_git_repo(repo_path: &Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repo_path)
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
    }

    fn run_git_commit(repo_path: &Path, message: &str) {
        let status = std::process::Command::new("git")
            .args(["-C", &repo_path.to_string_lossy()])
            .args(["add", "-A"])
            .status()
            .expect("git add failed");
        assert!(status.success(), "git add failed");
        let status = std::process::Command::new("git")
            .args(["-C", &repo_path.to_string_lossy()])
            .args(["commit", "-m", message])
            .status()
            .expect("git commit failed");
        assert!(status.success(), "git commit failed");
    }

    #[test]
    fn test_validate_ref_allows_valid() {
        assert!(validate_ref("main").is_ok());
        assert!(validate_ref("feature/my-feature").is_ok());
        assert!(validate_ref("v1.2.3").is_ok());
        assert!(validate_ref("HEAD~1").is_ok());
        assert!(validate_ref("origin/main").is_ok());
    }

    #[test]
    fn test_validate_ref_rejects_empty() {
        assert!(validate_ref("").is_err());
    }

    #[test]
    fn test_validate_ref_rejects_shell_chars() {
        assert!(validate_ref("main; echo evil").is_err());
        assert!(validate_ref("main|cat /etc/passwd").is_err());
        assert!(validate_ref("$(whoami)").is_err());
        assert!(validate_ref("`id`").is_err());
    }

    #[test]
    fn test_validate_path_allows_normal() {
        assert!(validate_path("src/main.rs").is_ok());
        assert!(validate_path("src/lib.rs").is_ok());
    }

    #[test]
    fn test_validate_path_rejects_traversal() {
        assert!(validate_path("../etc/passwd").is_err());
        assert!(validate_path("src/../../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_path_rejects_absolute() {
        assert!(validate_path("/etc/passwd").is_err());
    }

    #[test]
    fn test_validate_path_rejects_empty() {
        assert!(validate_path("").is_err());
    }

    #[tokio::test]
    async fn test_get_diff_with_since_and_until() {
        let (_tmp, browser) = temp_git_repo();
        let result = browser
            .get_diff("HEAD", None, false, Some("HEAD~1"), Some("HEAD"))
            .await;
        assert!(result.is_ok());
        let diff = result.unwrap();
        assert!(!diff.is_empty());
    }

    #[tokio::test]
    async fn test_get_diff_with_base_and_head() {
        let (_tmp, browser) = temp_git_repo();
        let result = browser.get_diff("HEAD~1", Some("HEAD"), false, None, None).await;
        assert!(result.is_ok());
        let diff = result.unwrap();
        assert!(!diff.is_empty());
    }

    #[tokio::test]
    async fn test_get_file_content_invalid_path() {
        let (_tmp, browser) = temp_git_repo();
        // Path with parent traversal is rejected by validate_path
        let result = browser.get_file_content("../nonexistent_file_xyz.rs", "HEAD").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_diff_invalid_ref() {
        let (_tmp, browser) = temp_git_repo();
        let result = browser
            .get_diff("nonexistent-branch-that-does-not-exist-12345", None, false, None, None)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_diff_empty_range() {
        let (_tmp, browser) = temp_git_repo();
        let result = browser.get_diff("HEAD", None, false, Some("HEAD"), Some("HEAD")).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_search_code_no_matches() {
        if std::process::Command::new("rg").arg("--version").output().is_err() {
            eprintln!("ripgrep not installed; skipping test");
            return;
        }
        let (_tmp, browser) = temp_git_repo();
        let result = browser.search_code("definitely-not-in-repo-xyz-12345");
        assert!(result.is_ok(), "search_code failed: {:?}", result.err());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_validate_ref_rejects_whitespace() {
        assert!(validate_ref("main feature").is_err());
        assert!(validate_ref("HEAD~ 1").is_err());
    }

    #[test]
    fn test_get_related_tests_no_match() {
        let (_tmp, browser) = temp_git_repo();
        let tests = browser.get_related_tests("nonexistent.rs").unwrap();
        assert!(tests.is_empty());
    }

    #[test]
    fn test_get_file_history_no_commits() {
        let (_tmp, browser) = temp_git_repo();
        let history = browser.get_file_history("nonexistent.rs", 5).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn test_get_related_tests_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path();
        init_git_repo(repo_path);
        std::fs::create_dir_all(repo_path.join("src")).unwrap();
        std::fs::write(repo_path.join("src/lib.rs"), "").unwrap();
        std::fs::write(repo_path.join("src/lib_test.rs"), "").unwrap();
        let browser = LocalGitBrowser::new(repo_path.to_str().unwrap());
        let tests = browser.get_related_tests("src/lib.rs").unwrap();
        assert!(tests.contains(&"src/lib_test.rs".to_string()));
    }

    #[test]
    fn test_get_related_tests_mirror_dir() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path();
        init_git_repo(repo_path);
        std::fs::create_dir_all(repo_path.join("src")).unwrap();
        std::fs::create_dir_all(repo_path.join("tests")).unwrap();
        std::fs::write(repo_path.join("src/lib.rs"), "").unwrap();
        std::fs::write(repo_path.join("tests/lib.rs"), "").unwrap();
        let browser = LocalGitBrowser::new(repo_path.to_str().unwrap());
        let tests = browser.get_related_tests("src/lib.rs").unwrap();
        assert!(tests.contains(&"tests/lib.rs".to_string()));
    }

    #[test]
    fn test_get_file_history_with_commits() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path();
        init_git_repo(repo_path);
        let file_path = repo_path.join("test.txt");
        std::fs::write(&file_path, "v1").unwrap();
        run_git_commit(repo_path, "initial commit");
        std::fs::write(&file_path, "v2").unwrap();
        run_git_commit(repo_path, "second commit");

        let browser = LocalGitBrowser::new(repo_path.to_str().unwrap());
        let history = browser.get_file_history("test.txt", 10).unwrap();
        assert!(!history.is_empty());
        assert!(history.iter().any(|(_, s)| s == "second commit"));
    }
}
