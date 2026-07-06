//! Lightweight project context gathering for lead review prompts.
//!
//! Collects bounded metadata about a repository (file tree, README/manifest
//! excerpts, recent commits, branch commits) without reading the whole
//! codebase into the prompt. Works for both Git repositories and plain
//! directories.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
    "png", "jpg", "jpeg", "gif", "ico", "svg", "woff", "woff2", "ttf", "eot", "pdf", "doc", "docx", "xls", "xlsx",
    "zip", "tar", "gz", "bz2", "7z", "rar", "exe", "dll", "so", "dylib", "wasm", "mp3", "mp4", "avi", "mov", "mkv",
    "pyc", "class", "o",
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
    if repo_path.join(".git").exists() {
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
    let file_tree = git_ls_files(repo_path).unwrap_or_default();
    let recent_commits = git_log_oneline(repo_path, &[]).unwrap_or_default();
    let branch_commits = match (base_ref, head_ref) {
        (Some(base), Some(head)) if !base.is_empty() && !head.is_empty() => {
            git_log_oneline(repo_path, &[&format!("{}..{}", base, head)]).unwrap_or_default()
        }
        _ => Vec::new(),
    };

    let readme_excerpt = read_git_or_file(repo_path, "README.md", 2000).unwrap_or_default();
    let manifest_excerpt = first_manifest_excerpt(repo_path, 2000).unwrap_or_default();

    Ok(ProjectContext {
        readme_excerpt,
        manifest_excerpt,
        file_tree,
        recent_commits,
        branch_commits,
    })
}

/// Run `git -C <repo> ls-files` and keep at most the first `max_lines`.
fn git_ls_files(repo_path: &Path) -> Result<Vec<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
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

/// Run `git -C <repo> log --oneline -n 30` with optional extra revision args.
fn git_log_oneline(repo_path: &Path, extra_args: &[&str]) -> Result<Vec<String>> {
    let mut args = vec!["-C", repo_path.to_str().unwrap_or("."), "log", "--oneline", "-n", "30"];
    args.extend(extra_args);

    let output = std::process::Command::new("git")
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
/// `max_bytes`.
fn read_git_or_file(repo_path: &Path, path: &str, max_bytes: usize) -> Result<String> {
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

/// Run `git -C <repo> show HEAD:<path>` and return the content as a string.
fn git_show_head(repo_path: &Path, path: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
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
        s
    } else {
        let boundary = s.floor_char_boundary(max_bytes);
        s[..boundary].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_git_repo(path: &Path) {
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };
        run(&["init", "--initial-branch=main"]);
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
            .arg("-C")
            .arg(path)
            .args(["add", file])
            .status()
            .expect("git add failed");
        assert!(status.success());
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
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
            .arg("-C")
            .arg(dir.path())
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
}
