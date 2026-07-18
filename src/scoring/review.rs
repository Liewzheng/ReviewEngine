//! Scoring logic for individual experts and overall review quality.
//!
//! Computes numerical scores (0–100) from expert findings using a
//! penalty-based system: critical findings deduct 30 points, high
//! deduct 15, medium deduct 5, low deduct 1, and notes deduct 0.
//! Two adjustment factors refine the raw penalty score:
//!
//! - **Consensus multiplier** — a finding corroborated by at least one
//!   other expert (`agrees_with` non-empty) deducts 1.5× its base penalty.
//! - **Confidence factor** — the expert's average finding confidence,
//!   mapped to [0.8, 1.0] (confidence 10 → 1.0, 0 → 0.8), is multiplied
//!   onto the post-deduction score.
//!
//! [`expert_score`] calculates a single expert's score, while
//! [`weighted_overall_score`] combines multiple expert scores
//! weighted by their configured importance. The module also defines
//! [`ReviewScoreRecord`] and [`ExpertScoreRecord`] for reporting.

use crate::models::{Finding, PenaltyConfig, RiskLevel, RiskThresholdConfig};

/// A record of an expert's contribution to the overall score.
#[derive(Debug, Clone)]
pub struct ExpertScoreRecord {
    pub expert_name: String,
    /// Individual score (0-100) based on this expert's findings.
    pub individual_score: u8,
    /// This expert's configured weight (0-100).
    pub weight: u8,
}

/// A record of the overall review score.
#[derive(Debug, Clone)]
pub struct ReviewScoreRecord {
    pub overall_score: u8,
    pub risk_level: RiskLevel,
    pub expert_scores: Vec<ExpertScoreRecord>,
}

/// Compute an individual expert score from their findings using the configured penalties.
///
/// Scoring logic:
/// - Start at 100 (perfect score)
/// - Critical findings: -penalties.critical each
/// - High findings: -penalties.high each
/// - Medium findings: -penalties.medium each
/// - Low findings: -penalties.low each
/// - Note findings: -penalties.note each (0 by default)
/// - Consensus multiplier: a finding with a non-empty `agrees_with` list
///   (corroborated by ≥1 other expert) deducts 1.5× its base penalty
/// - Clamp the post-deduction score to 0-100
/// - Confidence factor: the findings' average confidence mapped to
///   [0.8, 1.0] (confidence 10 → 1.0, 0 → 0.8), multiplied onto the
///   post-deduction score; experts reporting low-confidence findings
///   keep a higher score
/// - Round and clamp the final score to 0-100
pub fn expert_score_with_config(findings: &[Finding], penalties: &PenaltyConfig) -> u8 {
    if findings.is_empty() {
        return 100;
    }

    let mut penalty = 0f64;
    let mut confidence_sum = 0u32;
    for finding in findings {
        let base = match finding.severity {
            crate::models::Severity::Critical => penalties.critical,
            crate::models::Severity::High => penalties.high,
            crate::models::Severity::Medium => penalties.medium,
            crate::models::Severity::Low => penalties.low,
            crate::models::Severity::Note => penalties.note,
        } as f64;
        let consensus_multiplier = if finding.agrees_with.is_empty() { 1.0 } else { 1.5 };
        penalty += base * consensus_multiplier;
        confidence_sum += finding.confidence as u32;
    }

    let score_after_penalty = (100f64 - penalty).clamp(0.0, 100.0);
    let avg_confidence = (confidence_sum as f64 / findings.len() as f64).min(10.0);
    let confidence_factor = 0.8 + 0.02 * avg_confidence;
    (score_after_penalty * confidence_factor).round().clamp(0.0, 100.0) as u8
}

/// Backward-compatible wrapper that uses the built-in default penalties.
///
/// Defaults: Critical -30, High -15, Medium -5, Low -1, Note -0.
pub fn expert_score(findings: &[Finding]) -> u8 {
    expert_score_with_config(findings, &PenaltyConfig::default())
}

/// Compute the weighted score from a list of (score, weight) pairs.
///
/// Returns `sum(score * weight) / sum(weight)`, clamped to 0–100.
/// Returns 0 if the total weight is zero.
pub(crate) fn compute_weighted(scores: &[(u8, u8)]) -> u8 {
    let total_weight: u32 = scores.iter().map(|(_, w)| *w as u32).sum();
    if total_weight == 0 {
        return 0;
    }

    let weighted_sum: f64 = scores
        .iter()
        .map(|(s, w)| *s as f64 * (*w as f64 / total_weight as f64))
        .sum();

    weighted_sum.round().clamp(0.0, 100.0) as u8
}

/// Compute weighted overall score from multiple expert scores.
///
/// Each expert's score is weighted by their configured weight.
/// The sum of weights must be 100 for a meaningful overall score.
pub fn weighted_overall_score(expert_scores: &[(String, u8, u8)]) -> u8 {
    let pairs: Vec<(u8, u8)> = expert_scores.iter().map(|(_, s, w)| (*s, *w)).collect();
    compute_weighted(&pairs)
}

/// Map an overall score (0-100) to a RiskLevel using configurable thresholds.
///
/// Uses the provided `RiskThresholdConfig` to determine boundaries:
/// - score > thresholds.healthy_min → Healthy
/// - score <= thresholds.critical_max → Critical
/// - score <= thresholds.high_max → High
/// - score <= thresholds.medium_max → Medium
/// - score <= thresholds.low_max → LowMedium
/// - otherwise → Low
pub fn score_to_risk_level_with_config(score: u8, thresholds: &RiskThresholdConfig) -> RiskLevel {
    if score > thresholds.healthy_min {
        RiskLevel::Healthy
    } else if score <= thresholds.critical_max {
        RiskLevel::Critical
    } else if score <= thresholds.high_max {
        RiskLevel::High
    } else if score <= thresholds.medium_max {
        RiskLevel::Medium
    } else if score <= thresholds.low_max {
        RiskLevel::LowMedium
    } else {
        RiskLevel::Low
    }
}

/// Backward-compatible wrapper that uses the original hardcoded thresholds.
///
/// Original defaults: Critical (0-20), High (21-40), Medium (41-60), LowMedium (61-80),
/// Low (81-90), Healthy (91+). These values are frozen to preserve backward
/// compatibility for callers that do not pass a config.
pub fn score_to_risk_level(score: u8) -> RiskLevel {
    let old_thresholds = RiskThresholdConfig {
        critical_max: 20,
        high_max: 40,
        medium_max: 60,
        low_max: 80,
        healthy_min: 90,
    };
    score_to_risk_level_with_config(score, &old_thresholds)
}

/// Compute the overall review score and risk level from expert findings using configurable thresholds and penalties.
pub fn compute_overall_with_config(
    expert_data: &[(&str, &[Finding], u8)],
    penalties: &PenaltyConfig,
    thresholds: &RiskThresholdConfig,
) -> (u8, RiskLevel) {
    let scores: Vec<(String, u8, u8)> = expert_data
        .iter()
        .map(|(name, findings, weight)| {
            let score = expert_score_with_config(findings, penalties);
            (name.to_string(), score, *weight)
        })
        .collect();

    let overall = weighted_overall_score(&scores);
    let risk = score_to_risk_level_with_config(overall, thresholds);
    (overall, risk)
}

/// Backward-compatible wrapper that uses original hardcoded penalties and thresholds.
///
/// Preserves v0.6.11 scoring behavior exactly: penalties of 30/15/5/1/0 and
/// risk thresholds of 20/40/60/80 (with scores above 90 mapping to Healthy).
pub fn compute_overall(expert_data: &[(&str, &[Finding], u8)]) -> (u8, RiskLevel) {
    compute_overall_with_config(
        expert_data,
        &PenaltyConfig {
            critical: 30,
            high: 15,
            medium: 5,
            low: 1,
            note: 0,
        },
        &RiskThresholdConfig {
            critical_max: 20,
            high_max: 40,
            medium_max: 60,
            low_max: 80,
            healthy_min: 90,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Effort, Finding, Severity};

    fn make_finding(severity: Severity) -> Finding {
        Finding {
            file: "test.rs".to_string(),
            line: Some(1),
            line_end: None,
            severity,
            confidence: 8,
            category: String::new(),
            title: "test".to_string(),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: String::new(),
            effort: Effort::Small,
            expert_name: "test".to_string(),
            expert_role: String::new(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    #[test]
    fn test_expert_score_perfect() {
        let score = expert_score(&[]);
        assert_eq!(score, 100);
    }

    #[test]
    fn test_expert_score_critical() {
        // 100 - 30 = 70, confidence factor 0.96 (avg confidence 8) → 67
        let score = expert_score(&[make_finding(Severity::Critical)]);
        assert_eq!(score, 67);
    }

    #[test]
    fn test_expert_score_high() {
        // 100 - 15 = 85, × 0.96 → 82
        let score = expert_score(&[make_finding(Severity::High)]);
        assert_eq!(score, 82);
    }

    #[test]
    fn test_expert_score_multiple() {
        let findings = vec![
            make_finding(Severity::Critical),
            make_finding(Severity::High),
            make_finding(Severity::Medium),
        ];
        let score = expert_score(&findings);
        assert_eq!(score, 48); // (100 - 30 - 15 - 5) × 0.96 = 48
    }

    #[test]
    fn test_expert_score_clamp_min() {
        let findings = vec![
            make_finding(Severity::Critical),
            make_finding(Severity::Critical),
            make_finding(Severity::Critical),
            make_finding(Severity::Critical),
        ];
        let score = expert_score(&findings);
        assert_eq!(score, 0); // clamped
    }

    #[test]
    fn test_weighted_overall_score_equal_weights() {
        let scores = vec![("alice".to_string(), 80u8, 50u8), ("bob".to_string(), 60u8, 50u8)];
        let overall = weighted_overall_score(&scores);
        assert_eq!(overall, 70); // (80*0.5 + 60*0.5) = 70
    }

    #[test]
    fn test_weighted_overall_score_unequal_weights() {
        let scores = vec![("alice".to_string(), 100u8, 75u8), ("bob".to_string(), 0u8, 25u8)];
        let overall = weighted_overall_score(&scores);
        assert_eq!(overall, 75); // (100*0.75 + 0*0.25) = 75
    }

    #[test]
    fn test_score_to_risk_level() {
        assert_eq!(score_to_risk_level(100), RiskLevel::Healthy);
        assert_eq!(score_to_risk_level(70), RiskLevel::LowMedium);
        assert_eq!(score_to_risk_level(50), RiskLevel::Medium);
        assert_eq!(score_to_risk_level(30), RiskLevel::High);
        assert_eq!(score_to_risk_level(10), RiskLevel::Critical);
    }

    #[test]
    fn test_compute_overall() {
        let bob_findings = [make_finding(Severity::Critical)];
        let data = vec![("alice", &[] as &[Finding], 50u8), ("bob", &bob_findings[..], 50u8)];
        let (score, risk) = compute_overall(&data);
        assert_eq!(score, 84); // (100*0.5 + 67*0.5) = 83.5 → 84
        assert_eq!(risk, RiskLevel::Low);
    }

    #[test]
    fn test_expert_score_mixed_severities() {
        let findings = vec![
            make_finding(Severity::Critical), // -30
            make_finding(Severity::High),     // -15
            make_finding(Severity::High),     // -15
            make_finding(Severity::Medium),   // -5
            make_finding(Severity::Low),      // -1
            make_finding(Severity::Low),      // -1
            make_finding(Severity::Note),     // -0
        ];
        // (100 - 30 - 15 - 15 - 5 - 1 - 1 - 0) × 0.96 = 33 × 0.96 = 31.68 → 32
        let score = expert_score(&findings);
        assert_eq!(score, 32);
    }

    #[test]
    fn test_expert_score_only_notes() {
        // Notes deduct 0, but the confidence factor (avg 8 → 0.96) still applies.
        let findings = vec![make_finding(Severity::Note), make_finding(Severity::Note)];
        assert_eq!(expert_score(&findings), 96);
    }

    #[test]
    fn test_expert_score_large_penalty_clamp_min() {
        let findings = vec![make_finding(Severity::Critical); 10]; // -300 = clamped to 0
        assert_eq!(expert_score(&findings), 0);
    }

    #[test]
    fn test_expert_score_single_low() {
        // 99 × 0.96 = 95.04 → 95
        assert_eq!(expert_score(&[make_finding(Severity::Low)]), 95);
    }

    #[test]
    fn test_expert_score_single_medium() {
        // 95 × 0.96 = 91.2 → 91
        assert_eq!(expert_score(&[make_finding(Severity::Medium)]), 91);
    }

    #[test]
    fn test_score_to_risk_level_boundary_values() {
        // Boundary: 0 -> Critical
        assert_eq!(score_to_risk_level(0), RiskLevel::Critical);
        // Upper boundary of Critical: 20
        assert_eq!(score_to_risk_level(20), RiskLevel::Critical);
        // Just over: 21 -> High
        assert_eq!(score_to_risk_level(21), RiskLevel::High);
        // Upper boundary of High: 40
        assert_eq!(score_to_risk_level(40), RiskLevel::High);
        // Just over: 41 -> Medium
        assert_eq!(score_to_risk_level(41), RiskLevel::Medium);
        // Upper boundary of Medium: 60
        assert_eq!(score_to_risk_level(60), RiskLevel::Medium);
        // Just over: 61 -> LowMedium
        assert_eq!(score_to_risk_level(61), RiskLevel::LowMedium);
        // Upper boundary of LowMedium: 80
        assert_eq!(score_to_risk_level(80), RiskLevel::LowMedium);
        // Just over: 81 -> Low
        assert_eq!(score_to_risk_level(81), RiskLevel::Low);
        // Upper boundary of Low: 90
        assert_eq!(score_to_risk_level(90), RiskLevel::Low);
        // Just over: 91 -> Healthy
        assert_eq!(score_to_risk_level(91), RiskLevel::Healthy);
        // u8::MAX = 255 -> Healthy
        assert_eq!(score_to_risk_level(u8::MAX), RiskLevel::Healthy);
        assert_eq!(score_to_risk_level(100), RiskLevel::Healthy);
    }

    #[test]
    fn test_weighted_overall_score_zero_weight_sum() {
        let scores = vec![("a".to_string(), 100u8, 0u8), ("b".to_string(), 50u8, 0u8)];
        assert_eq!(weighted_overall_score(&scores), 0);
    }

    #[test]
    fn test_weighted_overall_score_single_expert() {
        let scores = vec![("a".to_string(), 85u8, 100u8)];
        assert_eq!(weighted_overall_score(&scores), 85);
    }

    #[test]
    fn test_weighted_overall_score_rounding() {
        // (80 * 33.33... + 70 * 33.33... + 90 * 33.33...) / 100 = 80
        let scores = vec![
            ("a".to_string(), 80u8, 33u8),
            ("b".to_string(), 70u8, 33u8),
            ("c".to_string(), 90u8, 34u8),
        ];
        let result = weighted_overall_score(&scores);
        assert!(result > 0);
        assert!(result <= 100);
    }

    #[test]
    fn test_compute_overall_no_findings() {
        let data = vec![("expert", &[] as &[Finding], 100u8)];
        let (score, risk) = compute_overall(&data);
        assert_eq!(score, 100);
        assert_eq!(risk, RiskLevel::Healthy);
    }

    #[test]
    fn test_compute_overall_multiple_experts() {
        let alice_findings = [make_finding(Severity::High)]; // score 82
        let bob_findings = [make_finding(Severity::Medium), make_finding(Severity::Low)]; // score 90
        let data = vec![("alice", &alice_findings[..], 50u8), ("bob", &bob_findings[..], 50u8)];
        let (score, _risk) = compute_overall(&data);
        // (82*0.5 + 90*0.5) = 86
        assert_eq!(score, 86);
    }

    #[test]
    fn test_compute_overall_zero_weight() {
        let data = vec![("a", &[] as &[Finding], 0u8)];
        let (score, risk) = compute_overall(&data);
        assert_eq!(score, 0);
        assert_eq!(risk, RiskLevel::Critical);
    }

    #[test]
    fn test_expert_score_all_severities() {
        // One of each severity
        let findings = vec![
            make_finding(Severity::Critical), // -30
            make_finding(Severity::High),     // -15
            make_finding(Severity::Medium),   // -5
            make_finding(Severity::Low),      // -1
            make_finding(Severity::Note),     // -0
        ];
        assert_eq!(expert_score(&findings), 47); // (100 - 30 - 15 - 5 - 1) × 0.96 = 47.04 → 47
    }

    #[test]
    fn test_expert_score_confidence_factor_full_confidence() {
        // confidence 10 → factor 1.0: pure severity penalty
        let mut finding = make_finding(Severity::Critical);
        finding.confidence = 10;
        assert_eq!(expert_score(&[finding]), 70);
    }

    #[test]
    fn test_expert_score_confidence_factor_zero_confidence() {
        // confidence 0 → factor 0.8: (100 - 30) × 0.8 = 56
        let mut finding = make_finding(Severity::Critical);
        finding.confidence = 0;
        assert_eq!(expert_score(&[finding]), 56);
    }

    #[test]
    fn test_expert_score_consensus_multiplier() {
        // Corroborated finding (agrees_with non-empty): penalty ×1.5
        // (100 - 30×1.5) × 0.96 = 55 × 0.96 = 52.8 → 53
        let mut finding = make_finding(Severity::Critical);
        finding.agrees_with = vec!["other-expert".to_string()];
        assert_eq!(expert_score(&[finding]), 53);
    }

    #[test]
    fn test_expert_score_consensus_multiplier_with_full_confidence() {
        // Isolates the consensus multiplier: 100 - 30×1.5 = 55
        let mut finding = make_finding(Severity::Critical);
        finding.confidence = 10;
        finding.agrees_with = vec!["other-expert".to_string()];
        assert_eq!(expert_score(&[finding]), 55);
    }

    #[test]
    fn test_weighted_overall_score_large_weights() {
        // Weights that sum to 100
        let scores = vec![("a".to_string(), 100u8, 99u8), ("b".to_string(), 0u8, 1u8)];
        assert_eq!(weighted_overall_score(&scores), 99);
    }

    #[test]
    fn test_weighted_overall_score_all_zero_scores() {
        let scores = vec![("a".to_string(), 0u8, 50u8), ("b".to_string(), 0u8, 50u8)];
        assert_eq!(weighted_overall_score(&scores), 0);
    }

    #[test]
    fn test_weighted_overall_score_no_experts() {
        let scores: Vec<(String, u8, u8)> = vec![];
        assert_eq!(weighted_overall_score(&scores), 0);
    }

    #[test]
    fn test_score_to_risk_level_max_value() {
        assert_eq!(score_to_risk_level(200), RiskLevel::Healthy);
    }

    #[test]
    fn test_score_to_risk_level_min_value() {
        assert_eq!(score_to_risk_level(0), RiskLevel::Critical);
    }

    #[test]
    fn test_compute_overall_single_expert() {
        let findings = [make_finding(Severity::Critical)]; // score 67
        let data = vec![("security", &findings[..], 100u8)];
        let (score, risk) = compute_overall(&data);
        assert_eq!(score, 67);
        assert_eq!(risk, RiskLevel::LowMedium);
    }

    #[test]
    fn test_compute_overall_three_experts_asymmetric_weights() {
        let a = [make_finding(Severity::Critical)]; // score 67
        let b = [make_finding(Severity::High)]; // score 82
        let c = [make_finding(Severity::Medium)]; // score 91
        let data = vec![
            ("lead", &a[..], 50u8),
            ("quality", &b[..], 30u8),
            ("docs", &c[..], 20u8),
        ];
        let (score, _) = compute_overall(&data);
        // (67*0.5 + 82*0.3 + 91*0.2) = 33.5 + 24.6 + 18.2 = 76.3 → 76
        assert_eq!(score, 76);
    }

    #[test]
    fn test_expert_score_record_fields() {
        let record = ExpertScoreRecord {
            expert_name: "test".to_string(),
            individual_score: 85,
            weight: 50,
        };
        assert_eq!(record.expert_name, "test");
        assert_eq!(record.individual_score, 85);
        assert_eq!(record.weight, 50);
    }

    #[test]
    fn test_compute_weighted_equal_weights() {
        let scores = [(80, 50), (60, 50)];
        assert_eq!(compute_weighted(&scores), 70);
    }

    #[test]
    fn test_compute_weighted_zero_total_weight() {
        let scores: &[(u8, u8)] = &[];
        assert_eq!(compute_weighted(scores), 0);
    }

    #[test]
    fn test_compute_weighted_single_pair() {
        let scores = [(42, 100)];
        assert_eq!(compute_weighted(&scores), 42);
    }

    #[test]
    fn test_compute_weighted_rounds_half_up() {
        let scores = [(33, 1), (34, 1)];
        assert_eq!(compute_weighted(&scores), 34);
    }

    // ─── _with_config tests ───────────────────────

    #[test]
    fn test_expert_score_with_config_custom_penalties() {
        let penalties = PenaltyConfig {
            critical: 50,
            high: 25,
            medium: 10,
            low: 2,
            note: 0,
        };
        // All findings have confidence 8 → confidence factor 0.96
        let score = expert_score_with_config(&[make_finding(Severity::Critical)], &penalties);
        assert_eq!(score, 48); // (100 - 50) × 0.96

        let score = expert_score_with_config(&[make_finding(Severity::High)], &penalties);
        assert_eq!(score, 72); // (100 - 25) × 0.96

        let score = expert_score_with_config(&[make_finding(Severity::Medium)], &penalties);
        assert_eq!(score, 86); // (100 - 10) × 0.96 = 86.4 → 86
    }

    #[test]
    fn test_expert_score_with_config_note_penalty() {
        let penalties = PenaltyConfig {
            critical: 30,
            high: 15,
            medium: 5,
            low: 1,
            note: 5,
        };
        let findings = vec![make_finding(Severity::Note), make_finding(Severity::Note)];
        let score = expert_score_with_config(&findings, &penalties);
        assert_eq!(score, 86); // (100 - 5 - 5) × 0.96 = 86.4 → 86
    }

    #[test]
    fn test_score_to_risk_level_with_config_custom_thresholds() {
        let thresholds = RiskThresholdConfig {
            critical_max: 30,
            high_max: 50,
            medium_max: 70,
            low_max: 90,
            healthy_min: 95,
        };
        assert_eq!(score_to_risk_level_with_config(25, &thresholds), RiskLevel::Critical);
        assert_eq!(score_to_risk_level_with_config(40, &thresholds), RiskLevel::High);
        assert_eq!(score_to_risk_level_with_config(60, &thresholds), RiskLevel::Medium);
        assert_eq!(score_to_risk_level_with_config(80, &thresholds), RiskLevel::LowMedium);
        assert_eq!(score_to_risk_level_with_config(95, &thresholds), RiskLevel::Low);
        assert_eq!(score_to_risk_level_with_config(96, &thresholds), RiskLevel::Healthy);
    }

    #[test]
    fn test_compute_overall_with_config() {
        let penalties = PenaltyConfig {
            critical: 50,
            high: 25,
            medium: 10,
            low: 2,
            note: 0,
        };
        let thresholds = RiskThresholdConfig {
            critical_max: 30,
            high_max: 50,
            medium_max: 70,
            low_max: 90,
            healthy_min: 90,
        };
        let findings = [make_finding(Severity::Critical)];
        let data = vec![("security", &findings[..], 100u8)];
        let (score, risk) = compute_overall_with_config(&data, &penalties, &thresholds);
        assert_eq!(score, 48); // (100 - 50) × 0.96
        assert_eq!(risk, RiskLevel::High); // 48 <= high_max=50
    }

    #[test]
    fn test_backward_compatible_expert_score_uses_defaults() {
        // The wrapper should produce the same result as the _with_config with defaults
        let findings = vec![
            make_finding(Severity::Critical),
            make_finding(Severity::High),
            make_finding(Severity::Medium),
        ];
        let score1 = expert_score(&findings);
        let score2 = expert_score_with_config(&findings, &PenaltyConfig::default());
        assert_eq!(score1, score2);
        assert_eq!(score1, 48); // (100 - 30 - 15 - 5) × 0.96
    }

    #[test]
    fn test_backward_compatible_score_to_risk_level_uses_defaults() {
        let score1 = score_to_risk_level(70);
        let old_thresholds = RiskThresholdConfig {
            critical_max: 20,
            high_max: 40,
            medium_max: 60,
            low_max: 80,
            healthy_min: 90,
        };
        let score2 = score_to_risk_level_with_config(70, &old_thresholds);
        assert_eq!(score1, score2);
        assert_eq!(score1, RiskLevel::LowMedium);
    }

    #[test]
    fn test_backward_compatible_compute_overall_uses_defaults() {
        let findings = [make_finding(Severity::Critical)];
        let data = vec![("security", &findings[..], 100u8)];
        let (score1, risk1) = compute_overall(&data);
        let old_penalties = PenaltyConfig {
            critical: 30,
            high: 15,
            medium: 5,
            low: 1,
            note: 0,
        };
        let old_thresholds = RiskThresholdConfig {
            critical_max: 20,
            high_max: 40,
            medium_max: 60,
            low_max: 80,
            healthy_min: 90,
        };
        let (score2, risk2) = compute_overall_with_config(&data, &old_penalties, &old_thresholds);
        assert_eq!(score1, score2);
        assert_eq!(risk1, risk2);
    }
}
