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

// ─── Risk level helpers ──────────────────────

/// Canonical mapping from a 0–100 score to a risk level label.
pub fn score_to_risk_level(score: u8) -> &'static str {
    match score {
        0..=40 => "critical",
        41..=60 => "high",
        61..=80 => "medium",
        81..=90 => "low",
        _ => "healthy",
    }
}

// ─── YAML parsing helper ─────────────────────

/// Parse a sequence of YAML values into `ScoreItem`s.
///
/// Each item may contain `severity`, `message` (or `description`),
/// `file`, `evidence`, `impact`, `recommendation`, and `effort`.
/// Missing fields default to `None` (or `"medium"` for severity).
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
        })
        .collect()
}

// ─── Weight helpers ──────────────────────────

/// Compute the weighted total score from a list of expert scores.
///
/// Each expert's score is multiplied by its weight, then divided by
/// the sum of all active weights. This normalises the result to a
/// 0–100 scale even when only a subset of experts is active.
///
/// Returns `(score, risk_label)` where `risk_label` is one of
/// `"critical"`, `"high"`, `"medium"`, `"low"`, or `"healthy"`.
pub fn weighted_total(scores: &[ExpertScore]) -> (u8, String) {
    let pairs: Vec<(u8, u8)> = scores.iter().map(|s| (s.score, s.weight)).collect();
    let score = crate::scoring::compute_weighted(&pairs);

    (score, score_to_risk_level(score).to_string())
}
