//! Repository scanning, analysis, and file-level expert evaluation.
//!
//! The [`RepoScanner`] walks a local repository directory, collecting
//! metadata per file ([`FileEntry`]) and aggregate statistics
//! ([`RepoStats`], [`LanguageStats`]). Submodules provide deeper
//! analysis (`analysis`), file filtering (`filter`), scoring
//! (`scoring`), and expert evaluation of individual files (`experts`).
//! This module is used for repository-level assessments (as opposed
//! to MR-diff-based reviews).

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub mod analysis;
pub mod experts;
pub mod filter;

/// Metadata about a single file in the repository.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Absolute or relative path to the file.
    pub path: String,
    /// Detected programming language (e.g. "Rust", "Python").
    pub language: String,
    /// Number of lines of code (0 for binary files).
    pub loc: usize,
    /// Whether the file is a binary (non-text) file.
    pub is_binary: bool,
    /// Whether the file is auto-generated (e.g. lock files, generated code).
    pub is_generated: bool,
}

/// Aggregate statistics computed from a set of [`FileEntry`] values.
#[derive(Debug, Clone, Default)]
pub struct RepoStats {
    /// Total number of files scanned.
    pub total_files: usize,
    /// Total lines of code across all files.
    pub total_loc: usize,
    /// Per-language breakdown of files and LOC.
    pub languages: HashMap<String, LanguageStats>,
    /// Files exceeding the large-file threshold (>500 LOC).
    pub large_files: Vec<FileEntry>,
    /// Number of auto-generated files.
    pub generated_files: usize,
    /// Number of binary files.
    pub binary_files: usize,
}

/// Per-language statistics (file count and total LOC).
#[derive(Debug, Clone, Default)]
pub struct LanguageStats {
    /// Number of files in this language.
    pub files: usize,
    /// Lines of code in this language.
    pub loc: usize,
}

/// Scans a local repository directory for analysis.
pub struct RepoScanner {
    root: PathBuf,
    ignore_patterns: Vec<String>,
    submodules: Vec<String>,
}

/// Check whether `root` is a Git repository by looking for `.git`.
fn is_git_repo(root: &Path) -> bool {
    root.is_dir() && root.join(".git").exists()
}

/// Return the submodule paths inside a Git repository.
fn git_submodules(root: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["submodule", "status", "--recursive"])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            parts.get(1).map(|s| s.to_string())
        })
        .collect()
}

/// Return all tracked and untracked files under `root` that are not excluded by
/// standard ignore rules (`.gitignore`, `.git/info/exclude`, etc.).
fn git_tracked_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git ls-files: {}", e))?;

    if !output.status.success() {
        anyhow::bail!("git ls-files failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let mut paths = Vec::new();
    let mut start = 0;
    for (i, byte) in output.stdout.iter().enumerate() {
        if *byte == 0 {
            if i > start {
                let rel = std::str::from_utf8(&output.stdout[start..i])
                    .map_err(|e| anyhow::anyhow!("invalid utf-8 in git ls-files output: {}", e))?;
                paths.push(PathBuf::from(rel));
            }
            start = i + 1;
        }
    }
    if start < output.stdout.len() {
        let rel = std::str::from_utf8(&output.stdout[start..])
            .map_err(|e| anyhow::anyhow!("invalid utf-8 in git ls-files output: {}", e))?;
        paths.push(PathBuf::from(rel));
    }

    Ok(paths)
}

/// Return true if `path` equals `prefix` or is located inside it.
fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }
    let prefix_with_slash = format!("{}/", prefix);
    path.starts_with(&prefix_with_slash)
}

impl RepoScanner {
    /// Create a new scanner for the given repo path.
    ///
    /// The scanner pre-populates a list of common ignore patterns
    /// (`.git`, `node_modules`, `target`, etc.) that can be extended
    /// via [`add_ignore_pattern`] (if added later).
    pub fn new(path: &str) -> Self {
        let root = PathBuf::from(path);
        let mut ignore_patterns = vec![
            ".git".to_string(),
            "node_modules".to_string(),
            "target".to_string(),
            "__pycache__".to_string(),
            ".mypy_cache".to_string(),
            ".ruff_cache".to_string(),
            ".pytest_cache".to_string(),
            ".venv".to_string(),
            "venv".to_string(),
            "vendor".to_string(),
            ".generated".to_string(),
            "generated".to_string(),
            "dist".to_string(),
            "build".to_string(),
        ];

        let submodules = if is_git_repo(&root) {
            let subs = git_submodules(&root);
            ignore_patterns.extend(subs.clone());
            subs
        } else {
            Vec::new()
        };

        Self {
            root,
            ignore_patterns,
            submodules,
        }
    }

    /// Walk the repository directory tree and collect [`FileEntry`] items.
    ///
    /// In a Git repository this uses `git ls-files` so the result respects
    /// `.gitignore` and excludes submodule contents. Falls back to a manual
    /// directory walk for non-Git roots.
    pub fn scan(&self) -> Result<Vec<FileEntry>> {
        let mut entries = Vec::new();

        if is_git_repo(&self.root) {
            let tracked = git_tracked_files(&self.root)?;
            for rel in tracked {
                let rel_str = rel.to_string_lossy().replace('\\', "/");

                if self
                    .submodules
                    .iter()
                    .any(|prefix| path_matches_prefix(&rel_str, prefix))
                {
                    continue;
                }

                let abs = self.root.join(&rel);
                // The worktree is the source of truth: `git ls-files
                // --cached` reads the index, which still lists files deleted
                // from the worktree but not yet staged (`rm` without
                // `git rm`). Skip anything absent on disk.
                if !abs.exists() {
                    continue;
                }
                if abs.is_dir() {
                    continue;
                }
                if self.is_ignored(&abs) {
                    continue;
                }
                if let Some(entry) = self.classify_file(&abs) {
                    entries.push(entry);
                }
            }
        } else {
            self.scan_dir(&self.root, &mut entries)?;
        }

        Ok(entries)
    }

    /// Scan a directory recursively and collect file entries.
    fn scan_dir(&self, dir: &Path, entries: &mut Vec<FileEntry>) -> Result<()> {
        // RR-03: a missing root previously failed open (empty entries → a
        // 53/100 report for a path that does not exist). Fail closed with a
        // clear error instead.
        if !dir.exists() {
            anyhow::bail!("Repository path does not exist: {}", dir.display());
        }

        let read_dir = match dir.read_dir() {
            Ok(rd) => rd,
            Err(_) => return Ok(()), // skip unreadable dirs
        };

        for entry in read_dir.flatten() {
            let path = entry.path();

            // Check if path should be ignored
            if self.is_ignored(&path) {
                continue;
            }

            if path.is_dir() {
                self.scan_dir(&path, entries)?;
            } else if path.is_file() {
                if let Some(entry) = self.classify_file(&path) {
                    entries.push(entry);
                }
            }
        }

        Ok(())
    }

    /// Check if a path should be ignored based on patterns.
    fn is_ignored(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.ignore_patterns.iter().any(|p| name == p)
    }

    /// Classify a file and produce a FileEntry if it should be included.
    fn classify_file(&self, path: &Path) -> Option<FileEntry> {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let path_str = path.to_string_lossy().to_string();

        // Skip hidden files, but keep common config files — including CI
        // configs (`.gitlab-ci.yml`, `.travis.yml`), which the test-coverage
        // expert inspects for CI test steps.
        if name.starts_with('.')
            && name != ".gitignore"
            && name != ".editorconfig"
            && name != ".rustfmt.toml"
            && name != ".clippy.toml"
            && name != ".gitlab-ci.yml"
            && name != ".travis.yml"
        {
            return None;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = self.detect_language(ext);

        // Binary files are skipped entirely.
        if self.is_binary_file(ext) {
            return None;
        }

        let mut is_generated = name == "Cargo.lock"
            || name == "package-lock.json"
            || name == "yarn.lock"
            || name == "Gemfile.lock"
            || path_str.ends_with(".generated.rs")
            || path_str.contains("/generated/")
            || path_str.contains("/review_reports/");

        // Count lines and detect generated markers in content. A file that
        // cannot be read (e.g. deleted from the worktree after enumeration,
        // or an unreadable permission) is skipped entirely instead of being
        // reported as a phantom 0-LOC entry.
        let content = std::fs::read_to_string(path).ok()?;
        is_generated = is_generated || self.is_generated_content(&content);
        let loc = content.lines().count();

        Some(FileEntry {
            path: path_str,
            language,
            loc,
            is_binary: false,
            is_generated,
        })
    }

    fn detect_language(&self, ext: &str) -> String {
        match ext {
            "rs" => "Rust",
            "py" => "Python",
            "js" => "JavaScript",
            "ts" | "tsx" => "TypeScript",
            "jsx" => "React JSX",
            "go" => "Go",
            "java" => "Java",
            "rb" => "Ruby",
            "c" | "h" => "C",
            "cpp" | "hpp" | "cc" | "cxx" => "C++",
            "swift" => "Swift",
            "kt" | "kts" => "Kotlin",
            "toml" | "yaml" | "yml" | "json" => "Config",
            "md" | "rst" | "txt" => "Documentation",
            "html" | "css" | "scss" | "less" => "Web",
            "sh" | "bash" | "zsh" => "Shell",
            "sql" => "SQL",
            "dockerfile" => "Docker",
            _ => "Other",
        }
        .to_string()
    }

    fn is_binary_file(&self, ext: &str) -> bool {
        matches!(
            ext,
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "ico"
                | "svg"
                | "woff"
                | "woff2"
                | "ttf"
                | "eot"
                | "pdf"
                | "doc"
                | "docx"
                | "xls"
                | "xlsx"
                | "zip"
                | "tar"
                | "gz"
                | "bz2"
                | "7z"
                | "rar"
                | "exe"
                | "dll"
                | "so"
                | "dylib"
                | "wasm"
                | "mp3"
                | "mp4"
                | "avi"
                | "mov"
                | "mkv"
                | "pyc"
                | "class"
                | "o"
        )
    }

    /// Detect generated-file markers inside file content.
    fn is_generated_content(&self, content: &str) -> bool {
        let lower = content.to_lowercase();
        lower.contains("code generated")
            || lower.contains("autogenerated")
            || lower.contains("auto-generated")
            || lower.contains("generated by")
            || lower.contains("do not edit")
    }

    /// Compute aggregate statistics from file entries.
    pub fn compute_stats(&self, entries: &[FileEntry]) -> RepoStats {
        let mut stats = RepoStats::default();
        let mut large_files = Vec::new();

        for entry in entries {
            stats.total_files += 1;
            stats.total_loc += entry.loc;

            if entry.is_generated {
                stats.generated_files += 1;
            }
            if entry.is_binary {
                stats.binary_files += 1;
            }

            let lang_stats = stats.languages.entry(entry.language.clone()).or_default();
            lang_stats.files += 1;
            lang_stats.loc += entry.loc;

            if entry.loc > 500 {
                large_files.push(entry.clone());
            }
        }

        stats.large_files = large_files;
        stats
    }
}

#[cfg(test)]
mod tests;
