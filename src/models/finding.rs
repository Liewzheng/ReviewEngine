use serde::{Deserialize, Serialize};

// ─── 审核结果 ───────────────────────────────

/// A report produced by a single expert after reviewing a diff.
///
/// Contains the expert's name, the parsed findings, pre-rendered
/// Markdown, and the raw LLM response text for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertReport {
    /// Name of the expert that produced this report.
    pub expert_name: String,
    /// Individual findings (issues, suggestions) identified by the expert.
    pub findings: Vec<Finding>,
    /// Pre-rendered Markdown summary of the report.
    pub markdown: String,
    /// Raw LLM response text (preserved for debugging / transparency).
    pub raw_llm_response: String,
    /// Set when the LLM response could not be parsed into findings; carries
    /// the parse error so the report can surface it (⚠️) instead of silently
    /// showing "No issues found".
    #[serde(default)]
    pub parse_error: Option<String>,
    /// Path of the dumped raw LLM prompt + response when the run was launched
    /// with `--verbose` (see the orchestrator's verbose dump). `None` when the
    /// raw exchange was not persisted to disk.
    #[serde(default)]
    pub raw_dump_path: Option<String>,
}

/// A single finding / issue identified during a code review.
///
/// Each finding pinpoints a specific location in the code (file + line),
/// describes the problem with severity and confidence ratings, and
/// optionally provides evidence, impact analysis, and a recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Relative file path where the issue was found.
    pub file: String,
    /// Starting line number of the issue (optional).
    pub line: Option<u32>,
    /// Ending line number for multi-line issues (optional).
    pub line_end: Option<u32>,
    /// Severity level of the finding.
    pub severity: Severity,
    /// Confidence score (0–10) indicating how sure the expert is.
    pub confidence: u8,
    /// Category tag for grouping related findings (e.g. "security", "style").
    pub category: String,
    /// Short, descriptive title of the issue.
    pub title: String,
    /// Detailed explanation of the issue and its context.
    pub summary: String,
    /// Code snippet or log excerpt demonstrating the problem.
    pub evidence: String,
    /// Description of the potential business or technical impact.
    pub impact: String,
    /// Suggested fix or remediation advice.
    pub recommendation: String,
    /// Estimated effort to fix the issue.
    pub effort: Effort,
    /// Name of the expert that reported this finding.
    pub expert_name: String,
    /// Human-readable role of the expert that reported this finding.
    pub expert_role: String,
    /// Names of other experts that agree with this finding.
    pub agrees_with: Vec<String>,
    /// Reference links (e.g. to documentation, standards, or related code).
    pub references: Vec<String>,
}

impl Finding {
    /// Stable fingerprint identifying this finding across repeated reviews.
    ///
    /// Delegates to [`crate::feedback::fingerprint`] — the exact algorithm the
    /// feedback API uses when recording user verdicts — so feedback submitted
    /// through `POST /api/v1/feedback` matches the fingerprint computed here
    /// when later reviews filter false positives.
    pub fn fingerprint(&self) -> String {
        crate::feedback::fingerprint(&self.file, self.line, &self.title, &self.category)
    }
}

/// Severity level of a review finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum Severity {
    /// Must-fix issue with significant security, correctness, or stability impact.
    Critical,
    /// Should-fix issue that may cause problems in production.
    High,
    /// Moderate issue worth addressing but not blocking.
    #[default]
    Medium,
    /// Minor suggestion or cosmetic issue.
    Low,
    /// Informational observation (not an issue).
    Note,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Critical => write!(f, "critical"),
            Severity::High => write!(f, "high"),
            Severity::Medium => write!(f, "medium"),
            Severity::Low => write!(f, "low"),
            Severity::Note => write!(f, "note"),
        }
    }
}

/// Estimated effort required to address a review finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum Effort {
    /// Can be fixed in minutes (e.g. typo, rename).
    Trivial,
    /// Small, localised change (e.g. add error handling).
    #[default]
    Small,
    /// Moderate refactoring across a few files.
    Medium,
    /// Significant architectural change spanning many files.
    Large,
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effort::Trivial => write!(f, "trivial"),
            Effort::Small => write!(f, "small"),
            Effort::Medium => write!(f, "medium"),
            Effort::Large => write!(f, "large"),
        }
    }
}

/// The top-level output of a complete review pipeline.
///
/// Contains individual per-expert reports, an optional aggregated
/// report produced by the aggregator expert, and an optional lead
/// consolidation summary computed from all expert findings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewOutput {
    /// Per-expert review reports.
    pub reports: Vec<ExpertReport>,
    /// Optional consolidated report from the aggregator expert.
    pub aggregated: Option<AggregatedReport>,
    /// Findings dropped by the optional verification pass, with reasons.
    #[serde(default)]
    pub dropped_findings: Vec<crate::team::verifier::DroppedFinding>,
    /// Lead consolidation summary (confidence filtering, deduplication,
    /// conflicts, overall score). Always computed for expert-team reviews;
    /// `None` for non-review commands (describe/ask/improve/changelog).
    #[serde(default)]
    pub consolidated: Option<crate::team::lead_consolidator::ConsolidatedReport>,
}

/// A consolidated report produced by the aggregator expert.
///
/// Merges, deduplicates, and sorts findings from all individual experts
/// into a single comprehensive report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedReport {
    /// Merged and deduplicated findings from all experts.
    pub findings: Vec<Finding>,
    /// Pre-rendered Markdown of the consolidated report.
    pub markdown: String,
    /// Raw LLM response text from the aggregator call.
    pub raw_llm_response: String,
    /// Set when the aggregator response could not be parsed; surfaced in the
    /// report so a silent empty aggregation is not mistaken for "no issues".
    #[serde(default)]
    pub parse_error: Option<String>,
    /// Path of the dumped raw LLM prompt + response with `--verbose`.
    #[serde(default)]
    pub raw_dump_path: Option<String>,
}

impl ReviewOutput {
    /// Create a `ReviewOutput` with per-expert reports (no aggregation).
    pub fn new(reports: Vec<ExpertReport>) -> Self {
        Self {
            reports,
            aggregated: None,
            dropped_findings: Vec::new(),
            consolidated: None,
        }
    }

    /// Create a `ReviewOutput` with both per-expert reports and an aggregated report.
    pub fn with_aggregated(reports: Vec<ExpertReport>, aggregated: AggregatedReport) -> Self {
        Self {
            reports,
            aggregated: Some(aggregated),
            dropped_findings: Vec::new(),
            consolidated: None,
        }
    }

    /// Attach findings dropped by the verification pass.
    pub fn with_dropped_findings(mut self, dropped_findings: Vec<crate::team::verifier::DroppedFinding>) -> Self {
        self.dropped_findings = dropped_findings;
        self
    }

    /// Attach the lead consolidation summary.
    pub fn with_consolidated(mut self, consolidated: crate::team::lead_consolidator::ConsolidatedReport) -> Self {
        self.consolidated = Some(consolidated);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Finding` method must produce the exact fingerprint the feedback
    /// API computes, so verdicts recorded server-side match findings seen by
    /// the review pipeline.
    #[test]
    fn test_finding_fingerprint_matches_feedback_algorithm() {
        let finding = Finding {
            file: "src/main.rs".to_string(),
            line: Some(42),
            line_end: None,
            severity: Severity::High,
            confidence: 9,
            category: "security".to_string(),
            title: "SQL injection".to_string(),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: String::new(),
            effort: Effort::Small,
            expert_name: "security".to_string(),
            expert_role: String::new(),
            agrees_with: vec![],
            references: vec![],
        };
        assert_eq!(
            finding.fingerprint(),
            crate::feedback::fingerprint("src/main.rs", Some(42), "SQL injection", "security")
        );

        // A missing line hashes identically on both paths too.
        let no_line = Finding { line: None, ..finding };
        assert_eq!(
            no_line.fingerprint(),
            crate::feedback::fingerprint("src/main.rs", None, "SQL injection", "security")
        );
    }

    fn finding(file: &str, line: Option<u32>, title: &str, category: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line,
            line_end: None,
            severity: Severity::High,
            confidence: 9,
            category: category.to_string(),
            title: title.to_string(),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: String::new(),
            effort: Effort::Small,
            expert_name: "security".to_string(),
            expert_role: String::new(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    #[test]
    fn fingerprint_is_deterministic_and_sensitive_to_each_field() {
        let a = finding("src/main.rs", Some(42), "SQL injection", "security");
        let b = finding("src/main.rs", Some(42), "SQL injection", "security");
        assert_eq!(a.fingerprint(), b.fingerprint(), "same finding → same fingerprint");

        // Each distinguishing field changes the fingerprint.
        assert_ne!(
            a.fingerprint(),
            finding("src/other.rs", Some(42), "SQL injection", "security").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            finding("src/main.rs", Some(43), "SQL injection", "security").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            finding("src/main.rs", None, "SQL injection", "security").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            finding("src/main.rs", Some(42), "XSS", "security").fingerprint()
        );
        assert_ne!(
            a.fingerprint(),
            finding("src/main.rs", Some(42), "SQL injection", "quality").fingerprint()
        );
    }

    #[test]
    fn fingerprint_is_sixteen_hex_chars() {
        let fp = finding("src/a.rs", Some(1), "t", "c").fingerprint();
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn severity_display_is_lowercase_label() {
        assert_eq!(Severity::Critical.to_string(), "critical");
        assert_eq!(Severity::High.to_string(), "high");
        assert_eq!(Severity::Medium.to_string(), "medium");
        assert_eq!(Severity::Low.to_string(), "low");
        assert_eq!(Severity::Note.to_string(), "note");
    }

    #[test]
    fn severity_default_is_medium() {
        assert_eq!(Severity::default(), Severity::Medium);
    }

    #[test]
    fn effort_display_is_lowercase_label() {
        assert_eq!(Effort::Trivial.to_string(), "trivial");
        assert_eq!(Effort::Small.to_string(), "small");
        assert_eq!(Effort::Medium.to_string(), "medium");
        assert_eq!(Effort::Large.to_string(), "large");
    }

    #[test]
    fn effort_default_is_small() {
        assert_eq!(Effort::default(), Effort::Small);
    }

    #[test]
    fn finding_round_trips_through_json() {
        let f = finding("src/main.rs", Some(7), "leak", "security");
        let json = serde_json::to_string(&f).unwrap();
        let back: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file, f.file);
        assert_eq!(back.line, f.line);
        assert_eq!(back.title, f.title);
        assert_eq!(back.category, f.category);
        assert_eq!(back.fingerprint(), f.fingerprint());
    }
}
