//! Lightweight project context gathering for lead review prompts.
//!
//! Collects bounded metadata about a repository (file tree, README/manifest
//! excerpts, recent commits, branch commits) without reading the whole
//! codebase into the prompt. Works for both Git repositories and plain
//! directories.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tracing;

/// Project-level context gathered for the lead reviewer.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ProjectContext {
    /// Excerpt of the README, at most the first 2000 bytes.
    pub readme_excerpt: String,
    /// Excerpt of the first manifest found, at most the first 2000 bytes.
    pub manifest_excerpt: String,
    /// Tracked or discovered file paths (relative), capped at 200 entries.
    pub file_tree: Vec<String>,
    /// Recent `git log --oneline -n 30` entries.
    pub recent_commits: Vec<String>,
    /// `git log --oneline <base>..<head>` entries for the reviewed branch.
    pub branch_commits: Vec<String>,
}

/// Common manifest files used to identify project type and dependencies.
const MANIFEST_FILES: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "go.mod", "build.gradle"];

/// Extensions treated as binary when scanning the filesystem.
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "woff", "woff2", "ttf", "eot", "pdf", "doc", "docx", "xls", "xlsx", "zip",
    "tar", "gz", "bz2", "7z", "rar", "exe", "dll", "so", "dylib", "wasm", "mp3", "mp4", "avi", "mov", "mkv", "pyc",
    "class", "o",
];

/// Gather a bounded, lightweight project context from `repo_path`.
///
/// If `repo_path` is a Git repository, the context is built from Git commands
/// and `git show` of files at HEAD. For non-Git directories only the file
/// tree is collected via the filesystem. All fields are capped to keep prompt
/// token usage predictable.
pub fn gather_project_context(
    repo_path: &Path,
    base_ref: Option<&str>,
    head_ref: Option<&str>,
) -> Result<ProjectContext> {
    // Validate repo_path exists and is a directory
    if !repo_path.exists() {
        anyhow::bail!("Repository path does not exist: {}", repo_path.display());
    }
    if !repo_path.is_dir() {
        anyhow::bail!("Repository path is not a directory: {}", repo_path.display());
    }
    if repo_path.join(".git").is_dir() {
        gather_from_git(repo_path, base_ref, head_ref)
    } else {
        let file_tree = list_files_from_fs(repo_path, 200)?;
        Ok(ProjectContext {
            file_tree,
            ..ProjectContext::default()
        })
    }
}

fn gather_from_git(repo_path: &Path, base_ref: Option<&str>, head_ref: Option<&str>) -> Result<ProjectContext> {
    let file_tree = match git_ls_files(repo_path) {
        Ok(files) => files,
        Err(err) => {
            tracing::warn!("failed to list git files: {}", err);
            Vec::new()
        }
    };

    let recent_commits = match git_log_oneline(repo_path, &[]) {
        Ok(commits) => commits,
        Err(err) => {
            tracing::warn!("failed to read recent commits: {}", err);
            Vec::new()
        }
    };

    let branch_commits = match (base_ref, head_ref) {
        (Some(base), Some(head)) if !base.is_empty() && !head.is_empty() => {
            if is_valid_ref_name(base) && is_valid_ref_name(head) {
                match git_log_oneline(repo_path, &[&format!("{}..{}", base, head)]) {
                    Ok(commits) => commits,
                    Err(err) => {
                        tracing::warn!("failed to read branch commits: {}", err);
                        Vec::new()
                    }
                }
            } else {
                tracing::warn!("skipping branch commit collection: invalid base or head ref");
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    let readme_excerpt = match read_git_or_file(repo_path, "README.md", 2000) {
        Ok(content) => content,
        Err(err) => {
            tracing::warn!("failed to read README excerpt: {}", err);
            String::new()
        }
    };

    let manifest_excerpt = match first_manifest_excerpt(repo_path, 2000) {
        Ok(content) => content,
        Err(err) => {
            tracing::warn!("failed to read manifest excerpt: {}", err);
            String::new()
        }
    };

    Ok(ProjectContext {
        readme_excerpt,
        manifest_excerpt,
        file_tree,
        recent_commits,
        branch_commits,
    })
}

/// Returns true if `name` looks like a safe git ref name.
///
/// Allowed characters are ASCII letters, digits, `.`, `-`, `_`, and `/`.
/// Rejects empty names, leading `.` or `-`, trailing `.`, the sequences `..`,
/// `@{`, `.lock` suffixes, and any other Git revision metacharacters.
fn is_valid_ref_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') || name.starts_with('-') || name.ends_with('.') {
        return false;
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return false;
    }
    if name.contains("..") || name.contains("@{") || name.ends_with(".lock") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '/')
}

/// Returns true if `path` is a safe repository-relative path for `git show`.
///
/// The path must be non-empty, must not start with `/` or `~`, must not contain
/// `..` or backslashes, and must not contain characters such as `:`, `\n`, `\r`,
/// or `\0` that could alter command semantics or path interpretation.
fn is_valid_repo_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') || path.starts_with('~') || path.starts_with("\\\\") {
        return false;
    }
    if path.contains("..") || path.contains("\\") {
        return false;
    }
    let forbidden = |c: char| {
        c == ':' || c == '\n' || c == '\r' || c == '\0' || c == ';' || c == '|' || c == '&' || c == '$' || c == '`'
    };
    !path.chars().any(forbidden)
}

/// Sanitize a user-controlled argument to prevent command injection.
///
/// Rejects strings that start with `-` (flag injection), contain `;` or `|`
/// (shell metacharacters), or contain backticks.
pub fn sanitize_user_arg(arg: &str) -> Option<&str> {
    if arg.starts_with('-') {
        return None;
    }
    if arg.contains(';')
        || arg.contains('|')
        || arg.contains('&')
        || arg.contains('$')
        || arg.contains('`')
        || arg.contains('\0')
    {
        return None;
    }
    Some(arg)
}

/// Run `git ls-files` in `repo_path` and keep at most the first `max_lines`.
fn git_ls_files(repo_path: &Path) -> Result<Vec<String>> {
    let output = std::process::Command::new("git")
        .current_dir(repo_path)
        .args(["ls-files"])
        .output()
        .context("failed to run git ls-files")?;

    if !output.status.success() {
        anyhow::bail!("git ls-files failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with(".git"))
        .take(200)
        .map(String::from)
        .collect();
    lines.sort();
    Ok(lines)
}

/// Run `git log --oneline -n 30` in `repo_path` with optional extra revision args.
///
/// User-controlled arguments starting with `-` and arguments that do not pass
/// `is_valid_ref_name` are skipped and logged as warnings.
fn git_log_oneline(repo_path: &Path, extra_args: &[&str]) -> Result<Vec<String>> {
    let mut args = vec!["log", "--oneline", "-n", "30"];
    for arg in extra_args {
        if arg.starts_with('-') {
            tracing::warn!("skipping user-controlled git log argument: {}", arg);
            continue;
        }
        if let Some(sanitized) = sanitize_user_arg(arg) {
            if !is_valid_ref_name(sanitized) {
                tracing::warn!("skipping user-controlled git log argument: invalid ref '{}'", arg);
                continue;
            }
            args.push(sanitized);
        } else {
            tracing::warn!("skipping user-controlled git log argument: unsafe characters '{}'", arg);
            continue;
        }
    }

    let output = std::process::Command::new("git")
        .current_dir(repo_path)
        .args(&args)
        .output()
        .context("failed to run git log")?;

    if !output.status.success() {
        anyhow::bail!("git log failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(String::from)
        .collect())
}

/// Read a file from `git show HEAD:<path>` or the filesystem, keeping at most
/// `max_bytes`. Invalid `path` values are rejected with a warning and return an
/// empty string.
fn read_git_or_file(repo_path: &Path, path: &str, max_bytes: usize) -> Result<String> {
    if !is_valid_repo_path(path) {
        tracing::warn!("skipping read of invalid repo path: {}", path);
        return Ok(String::new());
    }
    if let Ok(content) = git_show_head(repo_path, path) {
        Ok(truncate_string(content, max_bytes))
    } else {
        let abs = repo_path.join(path);
        if abs.is_file() {
            let bytes = std::fs::read(&abs).with_context(|| format!("failed to read {}", path))?;
            if bytes.len() > max_bytes {
                Ok(String::from_utf8_lossy(&bytes[..max_bytes]).to_string())
            } else {
                Ok(String::from_utf8_lossy(&bytes).to_string())
            }
        } else {
            Ok(String::new())
        }
    }
}

/// Run `git show HEAD:<path>` in `repo_path` and return the content as a string.
///
/// `path` is validated defensively with `is_valid_repo_path`; invalid paths
/// return an error before invoking `git`.
fn git_show_head(repo_path: &Path, path: &str) -> Result<String> {
    if !is_valid_repo_path(path) {
        anyhow::bail!("invalid repo path for git show: {}", path);
    }
    let output = std::process::Command::new("git")
        .current_dir(repo_path)
        .arg("show")
        .arg(format!("HEAD:{}", path))
        .output()
        .context("failed to run git show")?;

    if !output.status.success() {
        anyhow::bail!("git show failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Find the first manifest among the known list and return its excerpt.
fn first_manifest_excerpt(repo_path: &Path, max_bytes: usize) -> Result<String> {
    for path in MANIFEST_FILES {
        match read_git_or_file(repo_path, path, max_bytes) {
            Ok(content) if !content.is_empty() => return Ok(content),
            _ => continue,
        }
    }
    Ok(String::new())
}

/// Recursively list files under `dir` up to `max_entries`, skipping binary and
/// hidden directories.
fn list_files_from_fs(dir: &Path, max_entries: usize) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries = match current.read_dir() {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            // Skip symlinks to prevent directory traversal via symlink escape
            if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                if metadata.file_type().is_symlink() {
                    continue;
                }
            }
            if is_ignored_path(&path) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                if let Some(rel) = path.strip_prefix(dir).ok().and_then(|p| p.to_str()) {
                    let rel_unix = rel.replace('\\', "/");
                    if !is_binary_extension(&rel_unix) {
                        result.push(rel_unix);
                    }
                }
                if result.len() >= max_entries {
                    break;
                }
            }
        }

        if result.len() >= max_entries {
            break;
        }
    }

    result.sort();
    if result.len() > max_entries {
        result.truncate(max_entries);
    }
    Ok(result)
}

fn is_ignored_path(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(
        name,
        ".git"
            | "node_modules"
            | "target"
            | "__pycache__"
            | ".venv"
            | "venv"
            | "vendor"
            | "dist"
            | "build"
            | ".generated"
            | "generated"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".pytest_cache"
    )
}

fn is_binary_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| BINARY_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn truncate_string(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the largest byte boundary that does not split a multi-byte character.
    let mut boundary = max_bytes;
    while boundary > 0 && s.get(..boundary).is_none() {
        boundary -= 1;
    }
    s.get(..boundary).map(|s| s.to_string()).unwrap_or(s)
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
        run(&["init"]);
        run(&["checkout", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
    }

    fn commit_file(path: &Path, file: &str, content: &str, message: &str) {
        let full = path.join(file);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
        let status = Command::new("git")
            .current_dir(path)
            .args(["add", file])
            .status()
            .expect("git add failed");
        assert!(status.success());
        let status = Command::new("git")
            .current_dir(path)
            .args(["commit", "-m", message])
            .status()
            .expect("git commit failed");
        assert!(status.success());
    }

    #[test]
    fn test_gather_project_context_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        let readme = "# Test Project\n\nThis is a test project.\n";
        let cargo = "[package]\nname = \"test\"\nversion = \"0.1.0\"\n";
        commit_file(dir.path(), "README.md", readme, "add readme");
        commit_file(dir.path(), "Cargo.toml", cargo, "add manifest");
        commit_file(dir.path(), "src/main.rs", "fn main() {}\n", "add main");

        // Create a feature branch with an additional commit so branch_commits is non-empty.
        let status = Command::new("git")
            .current_dir(dir.path())
            .args(["checkout", "-b", "feat/test"])
            .status()
            .expect("git checkout failed");
        assert!(status.success());
        commit_file(dir.path(), "src/feature.rs", "pub fn feature() {}\n", "add feature");

        let ctx = gather_project_context(dir.path(), Some("main"), Some("feat/test")).unwrap();
        assert!(ctx.readme_excerpt.contains("# Test Project"));
        assert!(ctx.manifest_excerpt.contains("[package]"));
        assert!(ctx.file_tree.iter().any(|f| f == "src/main.rs"));
        assert!(ctx.file_tree.iter().any(|f| f == "src/feature.rs"));
        assert!(ctx.recent_commits.iter().any(|c| c.contains("add feature")));
        assert!(ctx.branch_commits.iter().any(|c| c.contains("add feature")));
    }

    #[test]
    fn test_gather_project_context_non_git() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("logo.png"), "not binary").unwrap();

        let ctx = gather_project_context(dir.path(), None, None).unwrap();
        assert!(ctx.file_tree.iter().any(|f| f == "src/main.rs"));
        assert!(!ctx.file_tree.iter().any(|f| f == "logo.png"));
        assert!(ctx.readme_excerpt.is_empty());
        assert!(ctx.manifest_excerpt.is_empty());
    }

    #[test]
    fn test_default_project_context() {
        let ctx = ProjectContext::default();
        assert!(ctx.readme_excerpt.is_empty());
        assert!(ctx.manifest_excerpt.is_empty());
        assert!(ctx.file_tree.is_empty());
        assert!(ctx.recent_commits.is_empty());
        assert!(ctx.branch_commits.is_empty());
    }

    #[test]
    fn test_is_binary_extension() {
        assert!(is_binary_extension("assets/logo.png"));
        assert!(is_binary_extension("bin/lib.so"));
        assert!(!is_binary_extension("src/main.rs"));
    }

    #[test]
    fn test_truncate_string_respects_char_boundary() {
        let s = "Hello 世界".to_string();
        let truncated = truncate_string(s, 8);
        assert!(truncated.len() <= 8);
    }

    #[test]
    fn test_invalid_branch_refs_skipped() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        commit_file(dir.path(), "README.md", "# Test\n", "add readme");

        let ctx = gather_project_context(dir.path(), Some("--help"), Some("main")).unwrap();
        assert!(ctx.branch_commits.is_empty());
        assert!(ctx.readme_excerpt.contains("# Test"));
    }

    #[test]
    fn test_truncate_string_multibyte() {
        let s = "Hello 世界".to_string();
        let truncated = truncate_string(s, 8);
        assert!(truncated.len() <= 8);
    }

    #[test]
    fn test_svg_not_binary() {
        assert!(!is_binary_extension("assets/icon.svg"));
    }

    #[test]
    fn test_valid_ref_names() {
        assert!(is_valid_ref_name("main"));
        assert!(is_valid_ref_name("feature/foo"));
        assert!(is_valid_ref_name("v1.0.0"));
        assert!(is_valid_ref_name("fix_issue-123"));
    }

    #[test]
    fn test_invalid_ref_names() {
        assert!(!is_valid_ref_name(""));
        assert!(!is_valid_ref_name("@{upstream}"));
        assert!(!is_valid_ref_name(".."));
        assert!(!is_valid_ref_name("foo:bar"));
        assert!(!is_valid_ref_name("foo bar"));
        assert!(!is_valid_ref_name("-foo"));
        assert!(!is_valid_ref_name(".lock"));
        assert!(!is_valid_ref_name("foo~bar"));
        assert!(!is_valid_ref_name("foo^bar"));
        assert!(!is_valid_ref_name("foo{bar}"));
        assert!(!is_valid_ref_name("foo\\bar"));
        assert!(!is_valid_ref_name("foo[bar]"));
        assert!(!is_valid_ref_name("foo*bar"));
        assert!(!is_valid_ref_name("foo?bar"));
        assert!(!is_valid_ref_name("/feature"));
        assert!(!is_valid_ref_name("feature/"));
        assert!(!is_valid_ref_name("foo//bar"));
    }

    #[test]
    fn test_valid_repo_paths() {
        assert!(is_valid_repo_path("README.md"));
        assert!(is_valid_repo_path("src/main.rs"));
        assert!(is_valid_repo_path("Cargo.toml"));
    }

    #[test]
    fn test_invalid_repo_paths() {
        assert!(!is_valid_repo_path(""));
        assert!(!is_valid_repo_path("../etc/passwd"));
        assert!(!is_valid_repo_path("/etc/passwd"));
        assert!(!is_valid_repo_path("foo:bar"));
        assert!(!is_valid_repo_path("foo\nbar"));
        assert!(!is_valid_repo_path("foo\rbar"));
        assert!(!is_valid_repo_path("foo\0bar"));
    }

    #[test]
    fn test_is_valid_repo_path_symlink_and_traversal() {
        // Symlinks and path-traversal attempts should be rejected
        assert!(!is_valid_repo_path("link/to/../etc/passwd"));
        assert!(!is_valid_repo_path("foo/../../bar"));
        assert!(!is_valid_repo_path(".hidden/../secret"));
    }

    #[test]
    fn test_is_valid_repo_path_special_chars() {
        // Shell metacharacters and injection vectors
        assert!(!is_valid_repo_path("foo;bar"));
        assert!(!is_valid_repo_path("foo|bar"));
        assert!(!is_valid_repo_path("foo&bar"));
        assert!(!is_valid_repo_path("foo$bar"));
        assert!(!is_valid_repo_path("foo`bar"));
        assert!(!is_valid_repo_path("foo\x00bar"));
    }

    #[test]
    fn test_is_valid_repo_path_valid_edge_cases() {
        assert!(is_valid_repo_path(".gitignore"));
        assert!(is_valid_repo_path("a"));
        assert!(is_valid_repo_path("src/deep/nested/file.rs"));
        assert!(is_valid_repo_path("file_with-dashes.txt"));
        assert!(is_valid_repo_path("file.with.dots.rs"));
    }

    #[test]
    fn test_is_valid_ref_name_command_injection() {
        // Refs starting with dash are dangerous (git treats them as flags)
        assert!(!is_valid_ref_name("--help"));
        assert!(!is_valid_ref_name("-option"));
        assert!(!is_valid_ref_name("--output=/etc/passwd"));
    }

    #[test]
    fn test_is_valid_ref_name_more_invalid() {
        assert!(!is_valid_ref_name("foo;bar"));
        assert!(!is_valid_ref_name("foo|bar"));
        assert!(!is_valid_ref_name("foo bar"));
        assert!(!is_valid_ref_name("foo\tbar"));
        assert!(!is_valid_ref_name("foo\nbar"));
        assert!(!is_valid_ref_name("foo\0bar"));
    }

    #[test]
    fn test_list_files_from_fs_respects_max_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        std::fs::write(dir.path().join("c.txt"), "c").unwrap();

        let files = list_files_from_fs(dir.path(), 2).unwrap();
        assert!(files.len() <= 2);
    }

    #[test]
    fn test_sanitize_user_arg_rejects_flags() {
        assert!(sanitize_user_arg("--help").is_none());
        assert!(sanitize_user_arg("-option").is_none());
        assert!(sanitize_user_arg("-f").is_none());
    }

    #[test]
    fn test_sanitize_user_arg_rejects_shell_metacharacters() {
        assert!(sanitize_user_arg("foo;bar").is_none());
        assert!(sanitize_user_arg("foo|bar").is_none());
        assert!(sanitize_user_arg("foo&bar").is_none());
        assert!(sanitize_user_arg("foo$bar").is_none());
        assert!(sanitize_user_arg("foo`bar").is_none());
        assert!(sanitize_user_arg("foo\0bar").is_none());
    }

    #[test]
    fn test_sanitize_user_arg_accepts_valid() {
        assert_eq!(sanitize_user_arg("main"), Some("main"));
        assert_eq!(sanitize_user_arg("feature/foo"), Some("feature/foo"));
        assert_eq!(sanitize_user_arg("v1.0.0"), Some("v1.0.0"));
    }

    #[test]
    fn test_is_valid_repo_path_rejects_tilde_and_backslash() {
        assert!(!is_valid_repo_path("~/.ssh/id_rsa"));
        assert!(!is_valid_repo_path("foo\\bar"));
        assert!(!is_valid_repo_path("\\\\server\\share"));
    }

    #[test]
    fn test_is_valid_repo_path_rejects_absolute_and_dotdot() {
        assert!(!is_valid_repo_path("/etc/passwd"));
        assert!(!is_valid_repo_path("../secret"));
        assert!(!is_valid_repo_path("foo/../bar"));
    }

    #[test]
    fn test_gather_project_context_plain_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test").unwrap();

        let ctx = gather_project_context(dir.path(), None, None).unwrap();
        assert!(ctx.file_tree.iter().any(|f| f == "src/main.rs"));
        assert!(ctx.file_tree.iter().any(|f| f == "README.md"));
        assert!(ctx.readme_excerpt.is_empty()); // plain dir, no git show
        assert!(ctx.manifest_excerpt.is_empty());
    }
}
