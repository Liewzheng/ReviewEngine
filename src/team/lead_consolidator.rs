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

/// Diff file coverage accounting for a review.
///
/// A file counts as *reviewed* only when it was assigned to at least one
/// expert task that produced a report. Under-coverage (files assigned to no
/// task, or only to failed tasks) is surfaced in the report and **caps the
/// score**, so a run that silently skipped files can never score higher than
/// an honest full-coverage run — the 4-of-29-files / fake-85 regression.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileCoverage {
    /// Number of files in the reviewed diff.
    pub total_files: usize,
    /// Number of files reviewed by at least one successful expert task.
    pub reviewed_files: usize,
    /// Files no expert reviewed.
    pub unreviewed_files: Vec<String>,
}

impl FileCoverage {
    /// Full coverage: every one of `total` files was reviewed.
    pub fn full(total: usize) -> Self {
        Self {
            total_files: total,
            reviewed_files: total,
            unreviewed_files: Vec::new(),
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
    /// Number of files in the reviewed diff (coverage accounting).
    #[serde(default)]
    pub total_files: usize,
    /// Number of files reviewed by at least one expert.
    #[serde(default)]
    pub reviewed_files: usize,
    /// Files no expert reviewed; their presence caps the score (anti-cheat).
    #[serde(default)]
    pub unreviewed_files: Vec<String>,
    /// Hunk-level coverage ledger summary (`None` when no ledger was supplied,
    /// e.g. the backward-compatible `consolidate` wrapper). When present, the
    /// report renders the changed-vs-touched ratio and the uncovered ranges
    /// (coverage debt), and an insufficient ratio contributes to `unverified`.
    #[serde(default)]
    pub coverage: Option<crate::coverage::CoverageSummary>,
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
    ///
    /// Backward-compatible wrapper assuming full diff-file coverage (no score
    /// cap). Prefer [`ConsolidatorConfig::consolidate_with_coverage`] so
    /// under-covered runs are scored honestly.
    pub fn consolidate(&self, reports: &[ExpertReport], total_score: Option<u8>) -> ConsolidatedReport {
        self.consolidate_with_coverage(reports, total_score, &FileCoverage::default(), None)
    }

    /// Run the full consolidation pipeline with diff-file coverage accounting.
    ///
    /// When `coverage` reports files that were not reviewed (`reviewed_files <
    /// total_files`), the final score is capped proportionally
    /// (`score × reviewed / total`) and the shortfall is called out in the
    /// TL;DR. This makes under-coverage impossible to hide behind a high
    /// score: a run that skipped files cannot outscore a full-coverage run.
    ///
    /// `ledger` carries the hunk-level coverage ledger (changed ranges vs.
    /// demonstrated touches from findings). When its ratio falls below
    /// [`crate::coverage::COVERAGE_THRESHOLD`], the assessment is flagged
    /// `coverage_insufficient` and `unverified`, and the coverage debt is
    /// stored on the report for rendering.
    pub fn consolidate_with_coverage(
        &self,
        reports: &[ExpertReport],
        total_score: Option<u8>,
        coverage: &FileCoverage,
        ledger: Option<&crate::coverage::CoverageLedger>,
    ) -> ConsolidatedReport {
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
        let mut score = total_score.unwrap_or_else(|| self.compute_score(reports));
        // Anti-cheat: cap the score by the reviewed-file fraction. A diff with
        // files no expert reviewed cannot honestly score higher than its
        // covered fraction allows.
        let coverage_capped = coverage.total_files > 0 && coverage.reviewed_files < coverage.total_files;
        if coverage_capped {
            let ratio = coverage.reviewed_files as f64 / coverage.total_files as f64;
            score = (score as f64 * ratio).round().clamp(0.0, 100.0) as u8;
        }
        let risk_level = match &self.scoring {
            Some(s) => review::score_to_risk_level_with_config(score, &s.risk_thresholds),
            None => review::score_to_risk_level(score),
        };
        let consensus_threshold = self.scoring.as_ref().map_or_else(
            || ScoringConfig::default().consensus_threshold,
            |s| s.consensus_threshold,
        );
        let consensus_reached = score >= consensus_threshold;
        let mut tl_dr = self.generate_tldr(reports, &risk_level, all_findings.len());
        if coverage_capped {
            tl_dr.push_str(&format!(
                "\n\n⚠️ Coverage: {}/{} files reviewed; {} file(s) not covered by any expert — score capped.",
                coverage.reviewed_files,
                coverage.total_files,
                coverage.unreviewed_files.len()
            ));
        }
        // Zero consolidated findings across every expert: the perfect score is
        // NOT evidence of quality — it may mean low coverage or a systemic
        // miss. Flag the assessment as unverified so the report never reads
        // "healthy / all experts approve" for an empty result. Hunk-level
        // coverage insufficiency (demonstrated-touch ratio below threshold)
        // also makes the result unverified — even when findings exist, a
        // review that demonstrably examined only part of the diff is not a
        // trustworthy overall verdict.
        let coverage_summary = ledger.map(|l| l.summary());
        let coverage_insufficient = coverage_summary.as_ref().map(|s| !s.is_sufficient()).unwrap_or(false);
        if coverage_insufficient {
            if let Some(s) = &coverage_summary {
                tl_dr.push_str(&format!(
                    "\n\n⚠️ 审查覆盖不足：{}/{} 行改动可追溯被审查（{} 处 hunk 未覆盖）——结果标记为不可信。\n\
                     ⚠️ Insufficient review coverage: {}/{} changed lines demonstrably reviewed ({} uncovered range(s)) — result marked unverified.",
                    s.covered_changed_lines,
                    s.total_changed_lines,
                    s.debt.len(),
                    s.covered_changed_lines,
                    s.total_changed_lines,
                    s.debt.len(),
                ));
            }
        }
        let unverified = all_findings.is_empty() || coverage_insufficient;

        let assessment = OverallAssessment {
            score,
            risk_level,
            lead_override: None,
            tl_dr,
            unverified,
            coverage_insufficient,
        };

        ConsolidatedReport {
            findings: all_findings,
            low_confidence_removed,
            duplicates_merged,
            conflicts,
            assessment,
            consensus_reached,
            total_files: coverage.total_files,
            reviewed_files: coverage.reviewed_files,
            unreviewed_files: coverage.unreviewed_files.clone(),
            coverage: coverage_summary,
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
            return format!(
                "{} 位专家均未发现问题，但全零发现可能意味着审查覆盖率不足或系统性漏报，请谨慎对待（结果标记为“未验证/不可信”）。\n\n\
                 {} experts reported no issues — this may indicate low coverage or a systemic issue; treat with caution (result marked unverified).",
                expert_count, expert_count,
            );
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
mod tests;
