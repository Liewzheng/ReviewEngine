//! Publishing helper functions for review results on Git providers.
//!
//! This module provides the [`InlineNote`] struct and helper functions that
//! operate on a [`GitProvider`][crate::git_provider::GitProvider] to format and
//! publish review output. Platform-specific logic lives in the
//! `git_provider` implementations; this module only contains generic helpers
//! such as inline-note publishing and suggestion formatting.

use anyhow::Result;

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

/// Publish inline notes for critical and high-severity findings.
///
/// Only posts notes for findings that have a line number. Lower-severity
/// findings are included in the main discussion but not posted as inline
/// comments to avoid noise.
pub async fn publish_inline_notes(
    provider: &dyn crate::git_provider::GitProvider,
    findings: &[crate::models::Finding],
) -> Result<()> {
    use crate::models::Severity;

    for finding in findings {
        if finding.severity == Severity::Critical || finding.severity == Severity::High {
            if let Some(line) = finding.line {
                let body = format!(
                    "**[{}]** {} (Confidence: {}/10)\n\n{}",
                    finding.expert_name, finding.title, finding.confidence, finding.recommendation,
                );
                provider.post_inline_comment(&finding.file, line, &body).await?;
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
        use crate::git_provider::GitProvider;
        use crate::models::{Effort, Finding, MRInfo, Severity};
        use async_trait::async_trait;

        // Create a mock provider that records inline-comment calls.
        struct MockGitProvider {
            calls: std::sync::Mutex<Vec<String>>,
        }

        #[async_trait]
        impl GitProvider for MockGitProvider {
            async fn fetch_mr_info(&self) -> anyhow::Result<MRInfo> {
                unimplemented!()
            }
            async fn fetch_diff(&self) -> anyhow::Result<String> {
                unimplemented!()
            }
            async fn post_review_comment(&self, _body: &str) -> anyhow::Result<i64> {
                unimplemented!()
            }
            async fn post_inline_comment(&self, file: &str, _line: u32, _body: &str) -> anyhow::Result<()> {
                self.calls.lock().unwrap().push(file.to_string());
                Ok(())
            }
            async fn fetch_code_audit_toml(&self) -> anyhow::Result<Option<String>> {
                unimplemented!()
            }
            async fn add_reaction(&self, _comment_id: i64, _reaction: &str) -> anyhow::Result<()> {
                unimplemented!()
            }
            async fn update_discussion(&self, _discussion_id: &str, _body: &str) -> anyhow::Result<()> {
                unimplemented!()
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

        let provider = MockGitProvider {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        publish_inline_notes(&provider, &findings).await.unwrap();
        let called_files = provider.calls.lock().unwrap().clone();
        assert_eq!(called_files.len(), 1);
        assert!(called_files.contains(&"critical.rs".to_string()));
    }
}
