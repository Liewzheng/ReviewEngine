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
/// Contains individual per-expert reports and an optional aggregated
/// report produced by the aggregator expert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewOutput {
    /// Per-expert review reports.
    pub reports: Vec<ExpertReport>,
    /// Optional consolidated report from the aggregator expert.
    pub aggregated: Option<AggregatedReport>,
    /// Findings dropped by the optional verification pass, with reasons.
    #[serde(default)]
    pub dropped_findings: Vec<crate::team::verifier::DroppedFinding>,
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
}

impl ReviewOutput {
    /// Create a `ReviewOutput` with per-expert reports (no aggregation).
    pub fn new(reports: Vec<ExpertReport>) -> Self {
        Self {
            reports,
            aggregated: None,
            dropped_findings: Vec::new(),
        }
    }

    /// Create a `ReviewOutput` with both per-expert reports and an aggregated report.
    pub fn with_aggregated(reports: Vec<ExpertReport>, aggregated: AggregatedReport) -> Self {
        Self {
            reports,
            aggregated: Some(aggregated),
            dropped_findings: Vec::new(),
        }
    }

    /// Attach findings dropped by the verification pass.
    pub fn with_dropped_findings(mut self, dropped_findings: Vec<crate::team::verifier::DroppedFinding>) -> Self {
        self.dropped_findings = dropped_findings;
        self
    }
}
