//! Publishing review results back to Git providers.
//!
//! Defines the [`Publisher`] trait, which abstracts posting review
//! discussions and inline notes to GitLab MRs or GitHub PRs. The
//! [`InlineNote`] struct represents a file/line-specific annotation.
//! The module-level [`publish_inline_notes`] function iterates over
//! findings and posts inline notes for critical/high-severity issues.
//! Concrete implementations live in the `github` and `gitlab` submodules.

use anyhow::Result;
use async_trait::async_trait;

pub mod github;
pub mod gitlab;

/// A note to be posted on a specific line of a file in a merge request.
#[derive(Debug, Clone)]
pub struct InlineNote {
    /// Relative file path where the note should appear.
    pub file: String,
    /// Line number in the new (head) version of the file.
    pub line: u32,
    /// Markdown body of the inline comment.
    pub body: String,
}

/// Unified interface for publishing review results back to a Git provider.
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Post a new top-level discussion on the MR/PR containing the review report.
    async fn post_mr_discussion(&self, body: &str) -> Result<String>;
    /// Post an inline comment on a specific file and line.
    async fn post_inline_note(&self, note: &InlineNote) -> Result<()>;
    /// Update the body of an existing discussion identified by its ID.
    async fn update_discussion(&self, discussion_id: &str, body: &str) -> Result<()>;

    /// Find an existing discussion by title prefix and update it, or create a new one.
    ///
    /// Default implementation falls back to `post_mr_discussion`.
    /// Override in platform-specific publishers to implement find-or-create.
    async fn find_or_update_discussion(&self, body: &str) -> Result<String> {
        self.post_mr_discussion(body).await
    }
}

/// Publish inline notes for critical and high-severity findings.
///
/// Only posts notes for findings that have a line number. Lower-severity
/// findings are included in the main discussion but not posted as inline
/// comments to avoid noise.
pub async fn publish_inline_notes(publisher: &dyn Publisher, findings: &[crate::models::Finding]) -> Result<()> {
    use crate::models::Severity;

    for finding in findings {
        if finding.severity == Severity::Critical || finding.severity == Severity::High {
            if let Some(line) = finding.line {
                let note = InlineNote {
                    file: finding.file.clone(),
                    line,
                    body: format!(
                        "**[{}]** {} (Confidence: {}/10)\n\n{}",
                        finding.expert_name, finding.title, finding.confidence, finding.recommendation,
                    ),
                };
                publisher.post_inline_note(&note).await?;
            }
        }
    }
    Ok(())
}

/// Format a finding's recommendation as a GitLab suggestion block.
///
/// The output uses ````suggestion` fence so that GitLab renders an
/// "Apply suggestion" button.  If `evidence` is non-empty it is used as
/// the "before" code; otherwise only the recommendation is shown
/// (no replace suggestion). Backticks in content are escaped to prevent
/// fence breakage.
pub fn format_suggestion_block(evidence: &str, recommendation: &str) -> String {
    /// Escape backticks in content to prevent fence breakage.
    fn escape_backticks(s: &str) -> String {
        s.replace('`', "\\`")
    }

    if evidence.is_empty() {
        recommendation.to_string()
    } else {
        format!(
            "```suggestion\n{code}\n```\n\n{note}",
            code = escape_backticks(evidence.trim_end()),
            note = escape_backticks(recommendation),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inline_note_struct() {
        let note = InlineNote {
            file: "src/main.rs".to_string(),
            line: 42,
            body: "test comment".to_string(),
        };
        assert_eq!(note.file, "src/main.rs");
        assert_eq!(note.line, 42);
        assert_eq!(note.body, "test comment");
    }

    #[tokio::test]
    async fn test_publish_inline_notes_skips_low_severity() {
        use crate::models::{Effort, Finding, Severity};

        // Create a mock publisher that records calls
        struct MockPublisher {
            calls: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl Publisher for MockPublisher {
            async fn post_mr_discussion(&self, _body: &str) -> Result<String> {
                Ok(String::new())
            }
            async fn post_inline_note(&self, note: &InlineNote) -> Result<()> {
                self.calls.lock().unwrap().push(note.file.clone());
                Ok(())
            }
            async fn update_discussion(&self, _id: &str, _body: &str) -> Result<()> {
                Ok(())
            }
        }

        let findings = vec![
            Finding {
                file: "critical.rs".to_string(),
                line: Some(1),
                line_end: None,
                severity: Severity::Critical,
                confidence: 9,
                category: String::new(),
                title: "Critical bug".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Fix it".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
            Finding {
                file: "low.rs".to_string(),
                line: Some(5),
                line_end: None,
                severity: Severity::Low,
                confidence: 3,
                category: String::new(),
                title: "Minor".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Consider".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
        ];

        let publisher = MockPublisher {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        publish_inline_notes(&publisher, &findings).await.unwrap();
        let called_files = publisher.calls.lock().unwrap().clone();
        assert_eq!(called_files.len(), 1);
        assert!(called_files.contains(&"critical.rs".to_string()));
    }
}
