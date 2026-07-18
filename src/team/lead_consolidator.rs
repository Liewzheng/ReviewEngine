use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::models::*;
use crate::scoring::review;

/// Configuration for the lead consolidator.
#[derive(Debug, Clone)]
pub struct ConsolidatorConfig {
    /// Minimum confidence threshold (1-10). Findings below this are downgraded/removed.
    pub min_confidence: u8,
    /// If true, findings below min_confidence are removed entirely.
    pub drop_low_confidence: bool,
    /// If true, remove findings that are identical across experts.
    pub deduplicate: bool,
    /// Optional scoring configuration for custom penalties and thresholds.
    pub scoring: Option<ScoringConfig>,
}

impl Default for ConsolidatorConfig {
    fn default() -> Self {
        Self {
            min_confidence: 6,
            drop_low_confidence: false,
            deduplicate: true,
            scoring: None,
        }
    }
}

/// Result of the consolidation process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedReport {
    /// Consolidated findings (deduplicated, filtered).
    pub findings: Vec<Finding>,
    /// Number of findings removed for low confidence.
    pub low_confidence_removed: usize,
    /// Number of duplicate findings merged.
    pub duplicates_merged: usize,
    /// Detected conflicts between experts.
    pub conflicts: Vec<ExpertConflict>,
    /// Overall assessment.
    pub assessment: OverallAssessment,
    /// Whether the overall weighted score reached `scoring.consensus_threshold`
    /// (default 70). Informational marker only — a score below the threshold
    /// is not modified.
    #[serde(default)]
    pub consensus_reached: bool,
}

/// A conflict between two or more experts on the same issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertConflict {
    pub file: String,
    pub line: Option<u32>,
    pub issue: String,
    pub experts: Vec<String>,
    pub resolutions: Vec<String>,
}

impl ConsolidatorConfig {
    /// Run the full consolidation pipeline.
    pub fn consolidate(&self, reports: &[ExpertReport], total_score: Option<u8>) -> ConsolidatedReport {
        let mut all_findings: Vec<Finding> = reports.iter().flat_map(|r| r.findings.clone()).collect();

        // Step 1: Filter by confidence
        let before_filter = all_findings.len();
        let (filtered, _low_conf_findings) = self.filter_by_confidence(all_findings);
        all_findings = filtered;
        let low_confidence_removed = before_filter - all_findings.len();

        // Step 2: Deduplicate
        let before_dedup = all_findings.len();
        let duplicates_merged = if self.deduplicate {
            all_findings = self.deduplicate_findings(all_findings);
            before_dedup - all_findings.len()
        } else {
            0
        };

        // Step 3: Detect conflicts
        let conflicts = self.detect_conflicts(&all_findings);

        // Step 4: Generate overall assessment
        let score = total_score.unwrap_or_else(|| self.compute_score(reports));
        let risk_level = match &self.scoring {
            Some(s) => review::score_to_risk_level_with_config(score, &s.risk_thresholds),
            None => review::score_to_risk_level(score),
        };
        let consensus_threshold = self.scoring.as_ref().map_or_else(
            || ScoringConfig::default().consensus_threshold,
            |s| s.consensus_threshold,
        );
        let consensus_reached = score >= consensus_threshold;
        let tl_dr = self.generate_tldr(reports, &risk_level, all_findings.len());

        let assessment = OverallAssessment {
            score,
            risk_level,
            lead_override: None,
            tl_dr,
        };

        ConsolidatedReport {
            findings: all_findings,
            low_confidence_removed,
            duplicates_merged,
            conflicts,
            assessment,
            consensus_reached,
        }
    }

    /// Filter findings by minimum confidence threshold.
    fn filter_by_confidence(&self, findings: Vec<Finding>) -> (Vec<Finding>, Vec<Finding>) {
        let mut kept = Vec::new();
        let mut removed = Vec::new();

        for finding in findings {
            if finding.confidence < self.min_confidence {
                if self.drop_low_confidence {
                    removed.push(finding);
                } else {
                    // Downgrade instead of removing
                    let mut downgraded = finding;
                    // Downgrade severity: Critical → High → Medium → Low → Note
                    downgraded.severity = match downgraded.severity {
                        Severity::Critical => Severity::High,
                        Severity::High => Severity::Medium,
                        Severity::Medium => Severity::Low,
                        _ => Severity::Note,
                    };
                    kept.push(downgraded);
                }
            } else {
                kept.push(finding);
            }
        }

        (kept, removed)
    }

    /// Deduplicate findings by (file, line, normalized title).
    fn deduplicate_findings(&self, findings: Vec<Finding>) -> Vec<Finding> {
        let mut seen: HashSet<(String, Option<u32>, String)> = HashSet::new();
        let mut deduped = Vec::new();

        for finding in findings {
            let key = (finding.file.clone(), finding.line, normalize_title(&finding.title));
            if seen.insert(key) {
                deduped.push(finding);
            } else {
                // Merge: mark as duplicate by adding to agrees_with
                if let Some(existing) = deduped.iter_mut().find(|f| {
                    f.file == finding.file
                        && f.line == finding.line
                        && normalize_title(&f.title) == normalize_title(&finding.title)
                }) {
                    if !existing.agrees_with.contains(&finding.expert_name) {
                        existing.agrees_with.push(finding.expert_name.clone());
                    }
                }
            }
        }

        deduped
    }

    /// Detect conflicts: same file/line but different recommendations.
    fn detect_conflicts(&self, findings: &[Finding]) -> Vec<ExpertConflict> {
        let mut conflicts = Vec::new();
        let mut seen: std::collections::HashMap<(String, Option<u32>), Vec<&Finding>> =
            std::collections::HashMap::new();

        for finding in findings {
            let key = (finding.file.clone(), finding.line);
            seen.entry(key).or_default().push(finding);
        }

        for ((file, line), group) in seen {
            if group.len() < 2 {
                continue;
            }
            // Check if experts disagree
            let unique_recommendations: HashSet<&str> = group.iter().map(|f| f.recommendation.as_str()).collect();
            if unique_recommendations.len() >= 2 {
                conflicts.push(ExpertConflict {
                    file,
                    line,
                    issue: group[0].title.clone(),
                    experts: group.iter().map(|f| f.expert_name.clone()).collect(),
                    resolutions: group.iter().map(|f| f.recommendation.clone()).collect(),
                });
            }
        }

        conflicts
    }

    /// Compute overall score from reports.
    fn compute_score(&self, reports: &[ExpertReport]) -> u8 {
        if reports.is_empty() {
            return 100;
        }
        let weight = 100 / reports.len() as u8;
        let data: Vec<(&str, &[Finding], u8)> = reports
            .iter()
            .map(|r| (r.expert_name.as_str(), r.findings.as_slice(), weight))
            .collect();
        match &self.scoring {
            Some(s) => {
                let (score, _) = review::compute_overall_with_config(&data, &s.penalties, &s.risk_thresholds);
                score
            }
            None => {
                let (score, _) = review::compute_overall(&data);
                score
            }
        }
    }

    /// Generate TL;DR summary.
    fn generate_tldr(&self, reports: &[ExpertReport], risk: &RiskLevel, total_findings: usize) -> String {
        let total_critical: usize = reports
            .iter()
            .flat_map(|r| r.findings.iter())
            .filter(|f| f.severity == Severity::Critical)
            .count();
        let total_high: usize = reports
            .iter()
            .flat_map(|r| r.findings.iter())
            .filter(|f| f.severity == Severity::High)
            .count();
        let expert_count = reports.len();

        if total_findings == 0 {
            return format!("All {} experts approve. No issues found.", expert_count);
        }

        let mut parts = Vec::new();
        if total_critical > 0 {
            parts.push(format!("{} critical", total_critical));
        }
        if total_high > 0 {
            parts.push(format!("{} high", total_high));
        }
        let remaining = total_findings.saturating_sub(total_critical + total_high);
        if remaining > 0 {
            parts.push(format!("{} other issues", remaining));
        }

        format!(
            "Risk Level: {:?}. {} found by {} reviewers.",
            risk,
            parts.join(", "),
            expert_count,
        )
    }
}

/// Normalize a finding title for comparison (lowercase, trim, remove punctuation).
fn normalize_title(title: &str) -> String {
    title
        .to_lowercase()
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ExpertReport;

    fn make_finding(severity: Severity, confidence: u8, file: &str, line: Option<u32>, title: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line,
            line_end: None,
            severity,
            confidence,
            category: String::new(),
            title: title.to_string(),
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

    fn make_report(expert_name: &str, findings: Vec<Finding>) -> ExpertReport {
        ExpertReport {
            expert_name: expert_name.to_string(),
            findings,
            markdown: String::new(),
            raw_llm_response: String::new(),
        }
    }

    #[test]
    fn test_filter_low_confidence_downgrades() {
        let config = ConsolidatorConfig::default();
        let findings = vec![
            make_finding(Severity::Critical, 10, "a.rs", Some(1), "Critical issue"),
            make_finding(Severity::High, 4, "b.rs", Some(2), "Low conf issue"),
        ];
        let reports = vec![make_report("tester", findings)];
        let result = config.consolidate(&reports, None);
        // Low confidence finding should be downgraded (not removed by default)
        assert_eq!(result.low_confidence_removed, 0);
        // The downgraded finding severity changed from High → Medium
        let downgraded = result.findings.iter().find(|f| f.file == "b.rs");
        assert!(downgraded.is_some());
        assert_eq!(downgraded.unwrap().severity, Severity::Medium);
    }

    #[test]
    fn test_filter_low_confidence_drops() {
        let config = ConsolidatorConfig {
            min_confidence: 6,
            drop_low_confidence: true,
            deduplicate: true,
            scoring: None,
        };
        let findings = vec![
            make_finding(Severity::High, 4, "b.rs", Some(2), "Low conf"),
            make_finding(Severity::Medium, 8, "a.rs", Some(1), "Good finding"),
        ];
        let reports = vec![make_report("tester", findings)];
        let result = config.consolidate(&reports, None);
        assert_eq!(result.low_confidence_removed, 1);
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn test_deduplicate_findings() {
        let config = ConsolidatorConfig::default();
        let findings = [
            make_finding(Severity::High, 8, "a.rs", Some(1), "Same issue"),
            make_finding(Severity::Medium, 7, "a.rs", Some(1), "Same issue"),
        ];
        let reports = vec![
            make_report("alice", vec![findings[0].clone()]),
            make_report("bob", vec![findings[1].clone()]),
        ];
        let result = config.consolidate(&reports, None);
        // Should be deduplicated to 1
        assert_eq!(result.findings.len(), 1);
        assert!(result.duplicates_merged > 0);
    }

    #[test]
    fn test_detect_conflicts() {
        // Disable dedup to test conflict detection in isolation
        let config = ConsolidatorConfig {
            deduplicate: false,
            ..Default::default()
        };
        let f1 = Finding {
            file: "a.rs".to_string(),
            line: Some(1),
            line_end: None,
            severity: Severity::Medium,
            confidence: 8,
            category: String::new(),
            title: "Style: tabs".to_string(),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: "Use tabs".to_string(),
            effort: Effort::Small,
            expert_name: "alice".to_string(),
            expert_role: String::new(),
            agrees_with: vec![],
            references: vec![],
        };
        let mut f2 = f1.clone();
        f2.title = "Style: spaces".to_string();
        f2.recommendation = "Use spaces".to_string();
        f2.expert_name = "bob".to_string();
        let reports = vec![make_report("alice", vec![f1]), make_report("bob", vec![f2])];
        let result = config.consolidate(&reports, None);
        // Same file/line but different recommendation → conflict
        assert!(!result.conflicts.is_empty(), "Expected conflicts but found none");
    }

    #[test]
    fn test_generate_assessment() {
        let config = ConsolidatorConfig::default();
        let findings = vec![make_finding(Severity::Critical, 9, "a.rs", Some(1), "Security hole")];
        let reports = vec![make_report("security", findings)];
        let result = config.consolidate(&reports, None);
        assert!(result.assessment.score < 100);
        // 1 critical finding (confidence 9): expert_score = 70 × 0.98 = 69, weight 100 → overall 69
        // score_to_risk_level(69) = LowMedium
        assert_eq!(result.assessment.risk_level, RiskLevel::LowMedium);
    }

    #[test]
    fn test_normalize_title() {
        assert_eq!(normalize_title("Hello World!"), "hello world");
        assert_eq!(normalize_title("  leading spaces  "), "leading spaces");
        assert_eq!(normalize_title("UPPERCASE"), "uppercase");
    }

    #[test]
    fn deduplicate_findings_empty_returns_empty() {
        let config = ConsolidatorConfig::default();
        let result = config.deduplicate_findings(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn deduplicate_findings_no_duplicates_keeps_all() {
        let config = ConsolidatorConfig::default();
        let findings = vec![
            make_finding(Severity::High, 8, "a.rs", Some(1), "Issue A"),
            make_finding(Severity::Medium, 7, "b.rs", Some(2), "Issue B"),
        ];
        let result = config.deduplicate_findings(findings);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn deduplicate_findings_exact_duplicates_merge_and_increment_agrees_with() {
        let config = ConsolidatorConfig::default();
        let mut first = make_finding(Severity::High, 8, "a.rs", Some(1), "Same issue");
        first.expert_name = "alice".to_string();
        let mut second = first.clone();
        second.expert_name = "bob".to_string();
        second.severity = Severity::Medium; // same key; should not matter for dedup

        let result = config.deduplicate_findings(vec![first, second]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].agrees_with.len(), 1);
        assert!(result[0].agrees_with.contains(&"bob".to_string()));
    }

    #[test]
    fn deduplicate_findings_different_findings_kept_separate() {
        let config = ConsolidatorConfig::default();
        let findings = vec![
            make_finding(Severity::High, 8, "a.rs", Some(1), "Issue A"),
            make_finding(Severity::High, 8, "a.rs", Some(1), "Issue B"),
        ];
        let result = config.deduplicate_findings(findings);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_consolidator_with_custom_scoring() {
        let config = ConsolidatorConfig {
            scoring: Some(ScoringConfig {
                enabled: true,
                display_individual_scores: true,
                display_weighted_score: true,
                penalties: PenaltyConfig {
                    critical: 50,
                    high: 25,
                    medium: 10,
                    low: 2,
                    note: 0,
                },
                consensus_threshold: 70,
                risk_thresholds: RiskThresholdConfig {
                    critical_max: 30,
                    high_max: 50,
                    medium_max: 70,
                    low_max: 90,
                    healthy_min: 95,
                },
            }),
            ..Default::default()
        };
        let findings = vec![make_finding(Severity::Critical, 9, "a.rs", Some(1), "Security hole")];
        let reports = vec![make_report("security", findings)];
        let result = config.consolidate(&reports, None);
        // 1 critical finding with custom penalty 50: (100 - 50) × 0.98 = 49, weight 100 -> overall 49
        assert_eq!(result.assessment.score, 49);
        // With custom thresholds (critical_max=30, high_max=50), score 49 => High
        assert_eq!(result.assessment.risk_level, RiskLevel::High);
        // 49 < consensus_threshold 70 → consensus not reached
        assert!(!result.consensus_reached);
    }

    #[test]
    fn test_consensus_reached_above_threshold() {
        let config = ConsolidatorConfig {
            scoring: Some(ScoringConfig {
                consensus_threshold: 70,
                ..Default::default()
            }),
            ..Default::default()
        };
        let reports = vec![make_report("security", vec![])];
        let result = config.consolidate(&reports, None);
        // No findings → score 100 ≥ 70 → consensus reached
        assert_eq!(result.assessment.score, 100);
        assert!(result.consensus_reached);
    }

    #[test]
    fn test_consensus_reached_uses_default_threshold_without_scoring() {
        // Without a scoring config the default threshold (70) applies.
        let config = ConsolidatorConfig::default();
        let reports = vec![make_report("security", vec![])];
        let result = config.consolidate(&reports, None);
        assert_eq!(result.assessment.score, 100);
        assert!(result.consensus_reached);
    }

    #[test]
    fn test_consensus_reached_with_explicit_total_score() {
        // An explicit total_score is also compared against the threshold.
        let config = ConsolidatorConfig {
            scoring: Some(ScoringConfig {
                consensus_threshold: 70,
                ..Default::default()
            }),
            ..Default::default()
        };
        let reports = vec![make_report("security", vec![])];
        let result = config.consolidate(&reports, Some(69));
        assert_eq!(result.assessment.score, 69);
        assert!(!result.consensus_reached);
    }

    #[test]
    fn test_consolidator_backward_compatible_without_scoring() {
        let config = ConsolidatorConfig::default();
        let findings = vec![make_finding(Severity::Critical, 9, "a.rs", Some(1), "Security hole")];
        let reports = vec![make_report("security", findings)];
        let result = config.consolidate(&reports, None);
        // Default penalty: critical = 30, so (100 - 30) × 0.98 = 69, weight 100 -> overall 69
        assert_eq!(result.assessment.score, 69);
        // Default thresholds: score 69 => LowMedium
        assert_eq!(result.assessment.risk_level, RiskLevel::LowMedium);
    }
}
