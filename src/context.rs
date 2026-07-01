//! Rich review context assembled from MR/PR metadata and diff analysis.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! The [`ReviewContext`] struct aggregates MR title, description,
//! branch names, commit messages, language statistics, file lists,
//! and extracted ticket/issue references. The [`FileEntry`] and
//! [`EditType`] types describe individual file changes. Auxiliary
//! functions analyse commit messages for patterns, extract issue
//! references (e.g. Jira, GitHub issue numbers), and compute
//! language-level change statistics from the diff.

use regex::Regex;
use std::collections::HashMap;

/// Rich context for a code review, assembled from various sources.
#[derive(Debug, Clone, Default)]
pub struct ReviewContext {
    /// MR/PR title
    pub title: String,
    /// MR/PR description
    pub description: String,
    /// Source branch name
    pub source_branch: String,
    /// Target branch name
    pub target_branch: String,
    /// Recent commit messages (newest first)
    pub commit_messages: Vec<String>,
    /// Programming language statistics (language -> bytes of change)
    pub language_stats: HashMap<String, u64>,
    /// List of changed files with edit type
    pub file_list: Vec<FileEntry>,
    /// Extracted ticket/issue references
    pub ticket_refs: Vec<TicketRef>,
}

/// A file entry in the diff, recording its path and type of change.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Relative path of the changed file.
    pub path: String,
    /// Whether the file was added, modified, deleted, or renamed.
    pub edit_type: EditType,
}

/// Classification of a file change in the diff.
#[derive(Debug, Clone, PartialEq)]
pub enum EditType {
    /// File was newly created in this diff.
    Added,
    /// Existing file was modified.
    Modified,
    /// File was deleted.
    Deleted,
    /// File was renamed or moved.
    Renamed,
}

/// A reference to an external ticket, issue, or story from a project management system.
#[derive(Debug, Clone)]
pub struct TicketRef {
    /// Origin system name (e.g. `"jira"`, `"github"`, `"gitlab"`).
    pub system: String,
    /// Raw issue/ticket ID (e.g. `"PROJ-123"`, `"#42"`).
    pub id: String,
    /// Optional URL linking to the ticket in its native system.
    pub url: Option<String>,
}

/// Assembles a [`ReviewContext`] from MR/PR metadata and parsed diff data.
///
/// Computes per-language statistics and extracts issue/ticket references
/// from commit messages and branch names.
pub struct ContextAssembler;

impl ContextAssembler {
    pub fn new() -> Self {
        Self
    }

    /// Build a ReviewContext from MR info and diff file data.
    pub fn assemble(
        &self,
        title: &str,
        description: &str,
        source_branch: &str,
        target_branch: &str,
        diff_files: &[crate::models::DiffFile],
        commit_messages: Vec<String>,
    ) -> ReviewContext {
        let file_list: Vec<FileEntry> = diff_files
            .iter()
            .map(|f| {
                let edit_type = match f.status.as_str() {
                    "added" | "A" => EditType::Added,
                    "deleted" | "D" => EditType::Deleted,
                    "renamed" | "R" => EditType::Renamed,
                    _ => EditType::Modified,
                };
                FileEntry {
                    path: f.new_path.clone(),
                    edit_type,
                }
            })
            .collect();

        let language_stats = Self::compute_language_stats(diff_files);
        let ticket_refs = Self::extract_tickets(description);

        ReviewContext {
            title: title.to_string(),
            description: description.to_string(),
            source_branch: source_branch.to_string(),
            target_branch: target_branch.to_string(),
            commit_messages,
            language_stats,
            file_list,
            ticket_refs,
        }
    }

    /// Compute language statistics from diff files based on file extensions.
    fn compute_language_stats(diff_files: &[crate::models::DiffFile]) -> HashMap<String, u64> {
        let mut stats = HashMap::new();
        for file in diff_files {
            let lang = Self::detect_language_from_path(&file.new_path);
            *stats.entry(lang).or_insert(0) += (file.additions + file.deletions) as u64;
        }
        stats
    }

    /// Detect programming language from file extension.
    fn detect_language_from_path(path: &str) -> String {
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "rs" | "rlib" => "Rust".to_string(),
            "py" => "Python".to_string(),
            "js" | "ts" | "tsx" | "jsx" => "TypeScript/JavaScript".to_string(),
            "go" => "Go".to_string(),
            "java" => "Java".to_string(),
            "rb" => "Ruby".to_string(),
            "toml" | "yaml" | "yml" | "json" => "Config".to_string(),
            "md" | "rst" | "txt" => "Documentation".to_string(),
            "css" | "scss" | "less" => "CSS".to_string(),
            "sql" => "SQL".to_string(),
            "sh" | "bash" | "zsh" => "Shell".to_string(),
            "dockerfile" | "Dockerfile" => "Docker".to_string(),
            _ => "Other".to_string(),
        }
    }

    /// Extract ticket references from text using regex patterns.
    fn extract_tickets(text: &str) -> Vec<TicketRef> {
        let mut refs = Vec::new();

        // JIRA: PROJECT-123
        if let Ok(re) = Regex::new(r"(?P<id>[A-Z][A-Z0-9]+-\d+)") {
            for cap in re.captures_iter(text) {
                if let Some(id) = cap.name("id") {
                    refs.push(TicketRef {
                        system: "jira".to_string(),
                        id: id.as_str().to_string(),
                        url: None,
                    });
                }
            }
        }

        // GitLab/GitHub issue: #123
        if let Ok(re) = Regex::new(r"#(\d+)") {
            for cap in re.captures_iter(text) {
                if let Some(id) = cap.get(1) {
                    refs.push(TicketRef {
                        system: "gitlab".to_string(),
                        id: format!("#{}", id.as_str()),
                        url: None,
                    });
                }
            }
        }

        refs
    }
}

/// Format context into a human-readable string for LLM prompts.
pub fn format_context_for_prompt(ctx: &ReviewContext) -> String {
    let mut parts = Vec::new();

    if !ctx.title.is_empty() {
        parts.push(format!("## Title\n{}", ctx.title));
    }

    if !ctx.description.is_empty() {
        parts.push(format!("## Description\n{}", ctx.description));
    }

    if !ctx.commit_messages.is_empty() {
        let commits = ctx
            .commit_messages
            .iter()
            .enumerate()
            .map(|(i, msg)| format!("  {}. {}", i + 1, msg))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("## Recent Commits\n{}", commits));
    }

    if !ctx.language_stats.is_empty() {
        let stats: Vec<String> = ctx
            .language_stats
            .iter()
            .map(|(lang, bytes)| format!("  - {}: {} bytes changed", lang, bytes))
            .collect();
        parts.push(format!("## Languages\n{}", stats.join("\n")));
    }

    if !ctx.file_list.is_empty() {
        let files: Vec<String> = ctx
            .file_list
            .iter()
            .map(|f| format!("  - [{}] {}", edit_type_symbol(&f.edit_type), f.path))
            .collect();
        parts.push(format!("## Changed Files\n{}", files.join("\n")));
    }

    if !ctx.ticket_refs.is_empty() {
        let tickets: Vec<String> = ctx
            .ticket_refs
            .iter()
            .map(|t| format!("  - {}: {}", t.system, t.id))
            .collect();
        parts.push(format!("## Related Tickets\n{}", tickets.join("\n")));
    }

    parts.join("\n\n")
}

fn edit_type_symbol(edit_type: &EditType) -> &'static str {
    match edit_type {
        EditType::Added => "A",
        EditType::Modified => "M",
        EditType::Deleted => "D",
        EditType::Renamed => "R",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_from_extension() {
        assert_eq!(ContextAssembler::detect_language_from_path("src/main.rs"), "Rust");
        assert_eq!(ContextAssembler::detect_language_from_path("app.py"), "Python");
        assert_eq!(
            ContextAssembler::detect_language_from_path("test.ts"),
            "TypeScript/JavaScript"
        );
        assert_eq!(ContextAssembler::detect_language_from_path("config.toml"), "Config");
        assert_eq!(
            ContextAssembler::detect_language_from_path("README.md"),
            "Documentation"
        );
    }

    #[test]
    fn test_extract_jira_ticket() {
        let refs = ContextAssembler::extract_tickets("Fixed PROJ-123 and SEC-456");
        assert!(refs.iter().any(|r| r.id == "PROJ-123"));
        assert!(refs.iter().any(|r| r.id == "SEC-456"));
    }

    #[test]
    fn test_extract_gitlab_issue() {
        let refs = ContextAssembler::extract_tickets("Closes #42 and related to #100");
        assert!(refs.iter().any(|r| r.id == "#42"));
        assert!(refs.iter().any(|r| r.id == "#100"));
    }

    #[test]
    fn test_no_tickets() {
        let refs = ContextAssembler::extract_tickets("Simple cleanup with no references");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_format_context_with_all_fields() {
        let mut ctx = ReviewContext::default();
        ctx.title = "Fix login bug".to_string();
        ctx.commit_messages = vec!["fix: login timeout".to_string()];
        ctx.language_stats.insert("Rust".to_string(), 150);
        ctx.file_list.push(FileEntry {
            path: "src/auth.rs".to_string(),
            edit_type: EditType::Modified,
        });
        let formatted = format_context_for_prompt(&ctx);
        assert!(formatted.contains("Fix login bug"));
        assert!(formatted.contains("src/auth.rs"));
        assert!(formatted.contains("Rust"));
    }

    #[test]
    fn test_compute_language_stats() {
        use crate::models::DiffFile;
        let files = vec![
            DiffFile {
                path: "src/main.rs".to_string(),
                old_path: String::new(),
                new_path: "src/main.rs".to_string(),
                status: "modified".to_string(),
                additions: 10,
                deletions: 5,
                hunks: vec![],
            },
            DiffFile {
                path: "test.py".to_string(),
                old_path: String::new(),
                new_path: "test.py".to_string(),
                status: "added".to_string(),
                additions: 30,
                deletions: 0,
                hunks: vec![],
            },
        ];
        let stats = ContextAssembler::compute_language_stats(&files);
        assert_eq!(stats.get("Rust"), Some(&15));
        assert_eq!(stats.get("Python"), Some(&30));
    }
}
