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
pub mod scoring;

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
}

impl RepoScanner {
    /// Create a new scanner for the given repo path.
    ///
    /// The scanner pre-populates a list of common ignore patterns
    /// (`.git`, `node_modules`, `target`, etc.) that can be extended
    /// via [`add_ignore_pattern`] (if added later).
    pub fn new(path: &str) -> Self {
        Self {
            root: PathBuf::from(path),
            ignore_patterns: vec![
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
            ],
        }
    }

    /// Walk the repository directory tree and collect [`FileEntry`] items.
    ///
    /// Skips directories and files matching the ignore patterns, hidden
    /// files (with some exceptions), and unreadable directories.
    pub fn scan(&self) -> Result<Vec<FileEntry>> {
        let mut entries = Vec::new();
        self.scan_dir(&self.root, &mut entries)?;
        Ok(entries)
    }

    /// Scan a directory recursively and collect file entries.
    fn scan_dir(&self, dir: &Path, entries: &mut Vec<FileEntry>) -> Result<()> {
        if !dir.exists() {
            return Ok(());
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

        // Skip hidden files, but keep common config files
        if name.starts_with('.')
            && name != ".gitignore"
            && name != ".editorconfig"
            && name != ".rustfmt.toml"
            && name != ".clippy.toml"
        {
            return None;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language = self.detect_language(ext);
        let is_binary = self.is_binary_file(ext);

        // Count lines
        let loc = if !is_binary {
            std::fs::read_to_string(path)
                .ok()
                .map(|c| c.lines().count())
                .unwrap_or(0)
        } else {
            0
        };

        let is_generated = name == "Cargo.lock"
            || name == "package-lock.json"
            || name == "yarn.lock"
            || name == "Gemfile.lock"
            || path_str.ends_with(".generated.rs")
            || path_str.contains("/generated/")
            || path_str.contains("/review_reports/");

        Some(FileEntry {
            path: path_str,
            language,
            loc,
            is_binary,
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
mod tests {
    use super::*;

    #[test]
    fn test_scanner_ignores_node_modules() {
        let scanner = RepoScanner::new(".");
        assert!(scanner.is_ignored(Path::new("node_modules")));
        assert!(scanner.is_ignored(Path::new(".git")));
        assert!(!scanner.is_ignored(Path::new("src")));
    }

    #[test]
    fn test_detect_language() {
        let scanner = RepoScanner::new(".");
        assert_eq!(scanner.detect_language("rs"), "Rust");
        assert_eq!(scanner.detect_language("py"), "Python");
        assert_eq!(scanner.detect_language("md"), "Documentation");
    }

    #[test]
    fn test_is_binary_file() {
        let scanner = RepoScanner::new(".");
        assert!(scanner.is_binary_file("png"));
        assert!(scanner.is_binary_file("pdf"));
        assert!(!scanner.is_binary_file("rs"));
    }

    #[test]
    fn test_compute_stats() {
        let entries = vec![
            FileEntry {
                path: "a.rs".to_string(),
                language: "Rust".to_string(),
                loc: 100,
                is_binary: false,
                is_generated: false,
            },
            FileEntry {
                path: "b.rs".to_string(),
                language: "Rust".to_string(),
                loc: 600,
                is_binary: false,
                is_generated: false,
            },
            FileEntry {
                path: "c.py".to_string(),
                language: "Python".to_string(),
                loc: 50,
                is_binary: false,
                is_generated: false,
            },
        ];
        let scanner = RepoScanner::new(".");
        let stats = scanner.compute_stats(&entries);
        assert_eq!(stats.total_files, 3);
        assert_eq!(stats.total_loc, 750);
        assert_eq!(stats.languages.get("Rust").unwrap().loc, 700);
        assert_eq!(stats.large_files.len(), 1);
    }
}
