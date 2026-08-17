//! Expert evaluation framework for repository-level code analysis.
//!
//! Defines the [`RepoExpert`] trait, which can be implemented by both
//! static (rule-based) and LLM-driven experts. The [`RepoContext`]
//! provides each expert with file entries, repository statistics, and
//! LLM configurations. Submodules supply concrete implementations:
//! `static_experts` for synchronous rule checks, `llm_experts` for
//! AI-powered analysis, `aggregator` for merging results, `chunk` for
//! splitting large repos, `context` for building expert context, and
//! `facts` for deterministic repository facts injected into prompts.

use anyhow::Result;
use async_trait::async_trait;

pub mod aggregator;
pub mod chunk;
pub mod context;
pub mod facts;
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
    /// Rendered [`facts::RepoFacts::to_prompt_block`] output, computed once
    /// per review over the FULL entry set (never per chunk) and shared with
    /// every LLM expert prompt. `None` on the local-only path, where no LLM
    /// prompt is built.
    pub facts_block: Option<String>,
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
    /// `true` when `score` is an explicit fallback rather than a genuine
    /// assessment — e.g. the LLM call failed, the response could not be
    /// parsed, or a static expert errored. Fallback scores must stay visible
    /// in reports instead of silently masquerading as model output.
    pub fallback: bool,
    /// Real LOC this expert evaluated (sum of entry LOCs), when known. The
    /// aggregator prefers this over its findings-count heuristic when
    /// LOC-weighting multi-chunk merges. `None` means "unknown — use the
    /// heuristic".
    pub evaluated_loc: Option<u64>,
    /// Raw per-sample scores when score sampling was active
    /// (`scoring.score_samples > 1`); the reported `score` is their median.
    /// `None` when sampling was disabled (the default).
    pub samples: Option<Vec<u8>>,
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

/// Score used when an LLM expert cannot produce a genuine assessment —
/// the call failed after all retries, or the response was empty,
/// unparseable, or schema-drifted. Every use must be paired with
/// [`ExpertScore::fallback`] `= true` at the layer that builds the
/// [`ExpertScore`], so reports can tell synthetic scores from model
/// output instead of silently masquerading as one.
pub(crate) const LLM_FALLBACK_SCORE: u8 = 70;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RiskLevel;

    fn score(name: &str, score: u8, weight: u8) -> ExpertScore {
        ExpertScore {
            expert_name: name.to_string(),
            weight,
            score,
            summary: String::new(),
            details: Vec::new(),
            fallback: false,
            evaluated_loc: None,
            samples: None,
        }
    }

    #[test]
    fn weighted_total_blends_scores_by_weight() {
        // 50/50 split of 90 and 10 → exactly 50.
        let (score, risk) = weighted_total(&[score("a", 90, 50), score("b", 10, 50)]);
        assert_eq!(score, 50);
        assert_eq!(risk, RiskLevel::High, "50 falls in 41–60 High band");
    }

    #[test]
    fn weighted_total_high_score_is_healthy() {
        let (score, risk) = weighted_total(&[score("a", 100, 100)]);
        assert_eq!(score, 100);
        assert_eq!(risk, RiskLevel::Healthy);
    }

    #[test]
    fn weighted_total_single_expert_returns_its_score() {
        let (score, risk) = weighted_total(&[score("a", 30, 20)]);
        assert_eq!(score, 30);
        assert_eq!(risk, RiskLevel::Critical, "30 ≤ 40 → Critical");
    }

    #[test]
    fn weighted_total_uneven_weights_favor_heavier_expert() {
        // (90, weight 80) + (10, weight 20) → 0.8*90 + 0.2*10 = 74.
        let (score, risk) = weighted_total(&[score("a", 90, 80), score("b", 10, 20)]);
        assert_eq!(score, 74);
        assert_eq!(risk, RiskLevel::Medium, "74 falls in 61–80 Medium band");
    }

    #[test]
    fn weighted_total_empty_scores_returns_zero_critical() {
        let (score, risk) = weighted_total(&[]);
        assert_eq!(score, 0);
        assert_eq!(risk, RiskLevel::Critical);
    }

    #[test]
    fn weighted_total_zero_total_weight_returns_zero() {
        let (score, risk) = weighted_total(&[score("a", 80, 0), score("b", 90, 0)]);
        assert_eq!(score, 0);
        assert_eq!(risk, RiskLevel::Critical);
    }

    #[test]
    fn weighted_total_rounds_half_up() {
        // (95, w50) + (90, w50) = 92.5 → rounds to 93.
        let (score, _) = weighted_total(&[score("a", 95, 50), score("b", 90, 50)]);
        assert_eq!(score, 93);
    }

    #[test]
    fn score_item_defaults_are_empty_and_fileless() {
        let item = ScoreItem::default();
        assert_eq!(item.severity, "");
        assert_eq!(item.message, "");
        assert_eq!(item.file, None);
        assert_eq!(item.confidence, None);
    }

    #[test]
    fn expert_score_fields_are_preserved_through_clone() {
        let details = vec![ScoreItem {
            severity: "high".to_string(),
            message: "missing bounds check".to_string(),
            ..Default::default()
        }];
        let original = ExpertScore {
            expert_name: "security".to_string(),
            weight: 15,
            score: 42,
            summary: "found 1 issue".to_string(),
            details,
            fallback: true,
            evaluated_loc: Some(1234),
            samples: Some(vec![40, 45]),
        };
        let cloned = original.clone();
        assert_eq!(cloned.expert_name, "security");
        assert_eq!(cloned.weight, 15);
        assert_eq!(cloned.score, 42);
        assert!(cloned.fallback);
        assert_eq!(cloned.evaluated_loc, Some(1234));
        assert_eq!(cloned.samples, Some(vec![40, 45]));
        assert_eq!(cloned.details[0].severity, "high");
        assert_eq!(cloned.details[0].message, "missing bounds check");
    }

    #[test]
    fn weighted_total_ignores_fallback_scores_only_in_details_not_math() {
        // Fallback flag does not change the math: both scores count.
        let mut a = score("a", 100, 50);
        a.fallback = true;
        let (score, risk) = weighted_total(&[a, score("b", 50, 50)]);
        assert_eq!(score, 75);
        assert_eq!(risk, RiskLevel::Medium);
    }
}
