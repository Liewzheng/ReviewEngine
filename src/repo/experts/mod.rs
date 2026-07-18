//! Expert evaluation framework for repository-level code analysis.
//!
//! Defines the [`RepoExpert`] trait, which can be implemented by both
//! static (rule-based) and LLM-driven experts. The [`RepoContext`]
//! provides each expert with file entries, repository statistics, and
//! LLM configurations. Submodules supply concrete implementations:
//! `static_experts` for synchronous rule checks, `llm_experts` for
//! AI-powered analysis, `aggregator` for merging results, `chunk` for
//! splitting large repos, and `context` for building expert context.

use anyhow::Result;
use async_trait::async_trait;

pub mod aggregator;
pub mod chunk;
pub mod context;
pub mod llm_experts;
pub mod static_experts;
pub mod test_coverage;

use crate::llm::client::LLMClient;
use crate::repo::FileEntry;

// ─── RepoContext ─────────────────────────────

/// Context provided to every repo-level expert for evaluation.
///
/// Contains scanned file entries, aggregate statistics, and LLM
/// configurations for experts that require AI-powered analysis.
pub struct RepoContext {
    /// All scanned file entries in the repository.
    pub entries: Vec<FileEntry>,
    /// Aggregate repository statistics (total files, LOC, languages, etc.).
    pub stats: crate::repo::RepoStats,
    /// LLM configurations for experts that require AI-powered analysis.
    pub llm_configs: Vec<crate::models::LLMConfig>,
    /// Resolved application configuration (for language profiles).
    pub config: Option<std::sync::Arc<crate::models::AppConfig>>,
}

// ─── ExpertScore ─────────────────────────────

/// Score produced by a single expert evaluation.
#[derive(Clone)]
pub struct ExpertScore {
    /// Name of the expert that produced this score.
    pub expert_name: String,
    /// Weight of this expert in the overall score (0–100).
    pub weight: u8,
    /// Normalised score (0–100).
    pub score: u8,
    /// One-line summary of the expert's assessment.
    pub summary: String,
    /// Detailed findings and observations.
    pub details: Vec<ScoreItem>,
}

/// A single finding or observation within an expert score.
///
/// Fields beyond [`severity`], [`message`] and [`file`] are populated
/// by LLM experts and provide actionable context for developers.
#[derive(Clone, Default)]
pub struct ScoreItem {
    /// Severity level (e.g. "high", "medium", "low", "note", "info").
    pub severity: String,
    /// Human-readable description of the issue or observation.
    pub message: String,
    /// Optional file path that the finding relates to.
    pub file: Option<String>,
    /// Code snippet or evidence demonstrating the issue (optional).
    pub evidence: Option<String>,
    /// Impact of not fixing the issue (optional).
    pub impact: Option<String>,
    /// Specific recommendation for fixing the issue (optional).
    pub recommendation: Option<String>,
    /// Estimated effort: trivial / small / medium / large (optional).
    pub effort: Option<String>,
    /// Confidence score (0–10) from the LLM, if provided (optional).
    pub confidence: Option<u8>,
}

// ─── RepoExpert trait ────────────────────────

/// An expert capable of evaluating a repository dimension.
///
/// - Static experts (requires_llm = false) run synchronously, no LLM needed.
/// - LLM experts (requires_llm = true) receive an `LLMClient` for API calls.
#[async_trait]
pub trait RepoExpert: Send + Sync {
    fn name(&self) -> &str;
    fn weight(&self) -> u8;
    fn requires_llm(&self) -> bool;
    async fn evaluate(&self, ctx: &RepoContext, llm: Option<&LLMClient>) -> Result<ExpertScore>;
}

// ─── Default weights ─────────────────────────

pub const DEFAULT_WEIGHTS: &[(&str, u8)] = &[
    ("code_organization", 15),
    ("test_coverage", 20),
    ("security", 15),
    ("documentation", 10),
    ("dependency", 10),
    ("code_style", 5),
    ("architecture", 15),
    ("code_quality", 10),
];

/// Sum of static-only weights (used when no LLM experts are active).
pub const STATIC_WEIGHT_SUM: u8 = 75;

/// Total weight when all experts (including LLM) are active.
pub const FULL_WEIGHT_SUM: u8 = 100;

// ─── YAML parsing helper ─────────────────────

/// Parse a sequence of YAML values into `ScoreItem`s.
///
/// Each item may contain `severity`, `message` (or `description`),
/// `file`, `evidence`, `impact`, `recommendation`, `effort`, and
/// `confidence`. Missing fields default to `None` (or `"medium"` for
/// severity).
pub(crate) fn parse_yaml_findings(items: &[serde_yaml_ng::Value]) -> Vec<ScoreItem> {
    items
        .iter()
        .map(|f| ScoreItem {
            severity: f["severity"]
                .as_str()
                .or_else(|| f["severity"].as_str())
                .unwrap_or("medium")
                .to_string(),
            message: f["message"]
                .as_str()
                .or_else(|| f["description"].as_str())
                .unwrap_or("")
                .to_string(),
            file: f["file"].as_str().map(String::from),
            evidence: f["evidence"].as_str().map(String::from),
            impact: f["impact"].as_str().map(String::from),
            recommendation: f["recommendation"].as_str().map(String::from),
            effort: f["effort"].as_str().map(String::from),
            confidence: f["confidence"].as_u64().map(|c| c.min(10) as u8),
        })
        .collect()
}

// ─── Finding conversion ──────────────────────

/// Map a repo-expert [`ScoreItem`] to a standard [`crate::models::Finding`],
/// so repo-review findings can flow through the shared quality mechanisms
/// (lead consolidator, verification pass).
///
/// `file` becomes the empty string when the item has no path; `line` is
/// always `None` (repo findings are not line-anchored); `confidence`
/// defaults to 5 when the LLM did not provide one.
pub(crate) fn score_item_to_finding(item: &ScoreItem) -> crate::models::Finding {
    use crate::models::{Effort, Severity};
    crate::models::Finding {
        file: item.file.clone().unwrap_or_default(),
        line: None,
        line_end: None,
        severity: match item.severity.as_str() {
            "critical" => Severity::Critical,
            "high" => Severity::High,
            "medium" => Severity::Medium,
            "low" => Severity::Low,
            "note" | "info" => Severity::Note,
            _ => Severity::Medium,
        },
        confidence: item.confidence.unwrap_or(5),
        category: "quality".to_string(),
        title: item.message.clone(),
        summary: String::new(),
        evidence: item.evidence.clone().unwrap_or_default(),
        impact: item.impact.clone().unwrap_or_default(),
        recommendation: item.recommendation.clone().unwrap_or_default(),
        effort: match item.effort.as_deref() {
            Some("trivial") => Effort::Trivial,
            Some("medium") => Effort::Medium,
            Some("large") => Effort::Large,
            _ => Effort::Small,
        },
        expert_name: "code_quality".to_string(),
        expert_role: "Code Quality".to_string(),
        agrees_with: vec![],
        references: vec![],
    }
}

/// Map a standard [`crate::models::Finding`] back to a [`ScoreItem`] for
/// repo-report rendering. Empty strings map back to `None`, so a round trip
/// through [`score_item_to_finding`] preserves the original shape.
pub(crate) fn finding_to_score_item(f: &crate::models::Finding) -> ScoreItem {
    fn non_empty(s: &str) -> Option<String> {
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }
    ScoreItem {
        severity: f.severity.to_string(),
        message: f.title.clone(),
        file: non_empty(&f.file),
        evidence: non_empty(&f.evidence),
        impact: non_empty(&f.impact),
        recommendation: non_empty(&f.recommendation),
        effort: Some(f.effort.to_string()),
        confidence: Some(f.confidence),
    }
}

// ─── Weight helpers ──────────────────────────

/// Compute the weighted total score from a list of expert scores.
///
/// Each expert's score is multiplied by its weight, then divided by
/// the sum of all active weights. This normalises the result to a
/// 0–100 scale even when only a subset of experts is active.
///
/// Returns `(score, risk_level)` where `risk_level` comes from the unified
/// [`crate::scoring::review`] mapping with the default thresholds — the
/// same bands the retired repo-local mapping used (≤40 Critical, 41–60
/// High, 61–80 Medium, 81–90 LowMedium, 91+ Healthy).
pub fn weighted_total(scores: &[ExpertScore]) -> (u8, crate::models::RiskLevel) {
    let pairs: Vec<(u8, u8)> = scores.iter().map(|s| (s.score, s.weight)).collect();
    let score = crate::scoring::review::compute_weighted(&pairs);
    let risk =
        crate::scoring::review::score_to_risk_level_with_config(score, &crate::models::RiskThresholdConfig::default());

    (score, risk)
}
