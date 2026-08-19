use std::collections::HashSet;

use crate::models::*;
use crate::team::lead_consolidator::{ConsolidatedReport, ConsolidatorConfig, FileCoverage};
use crate::team::verifier::DroppedFinding;

/// Run lead consolidation over the validated expert findings.
///
/// Pure computation (no LLM calls): confidence filtering, deduplication,
/// conflict detection, and overall scoring, driven by `config.report`
/// (`min_confidence`, `drop_low_confidence`) and `config.scoring`. The
/// diff-file `coverage` is threaded in so the score is capped when files
/// were not reviewed by any expert (anti-cheat: under-coverage must never
/// inflate the score).
pub(super) fn build_consolidated_report(
    reports: &[ExpertReport],
    config: &AppConfig,
    coverage: &FileCoverage,
    ledger: Option<&crate::coverage::CoverageLedger>,
) -> ConsolidatedReport {
    ConsolidatorConfig {
        min_confidence: config.report.min_confidence,
        drop_low_confidence: config.report.drop_low_confidence,
        scoring: Some(config.scoring.clone()),
        ..Default::default()
    }
    .consolidate_with_coverage(reports, None, coverage, ledger)
}

/// Build the hunk-level coverage ledger from the parsed diff and the expert
/// reports: changed ranges come from the diff hunks; touched ranges come from
/// the lines that expert findings actually reference (evidence-based coverage —
/// see the module docs for why). A file-scoped finding (`line: None`) marks
/// the file's full changed range as read, since the expert demonstrated
/// awareness of the file as a whole.
pub(super) fn build_coverage_ledger(
    diff_files: &[(String, Vec<DiffHunk>)],
    reports: &[ExpertReport],
) -> crate::coverage::CoverageLedger {
    let mut ledger = crate::coverage::CoverageLedger::from_diff_files(diff_files);
    for report in reports {
        for finding in &report.findings {
            match (finding.line, finding.line_end) {
                (Some(l), Some(e)) => ledger.mark_touched(&finding.file, (l, e.max(l)), &report.expert_name),
                (Some(l), None) => ledger.mark_touched(&finding.file, (l, l), &report.expert_name),
                (None, _) => {
                    let ranges: Vec<(u32, u32)> = ledger
                        .targets
                        .iter()
                        .find(|t| t.file == finding.file)
                        .map(|t| t.changed_ranges.clone())
                        .unwrap_or_default();
                    for &(a, b) in &ranges {
                        ledger.mark_touched(&finding.file, (a, b), &report.expert_name);
                    }
                }
            }
        }
    }
    ledger
}

/// Reason recorded in [`DroppedFinding`] for findings filtered out because
/// the user previously marked them as false positives via the feedback API.
const FEEDBACK_FALSE_POSITIVE_REASON: &str = "marked false positive by user feedback";

/// Drop findings the user previously marked as false positives via the
/// feedback API (A9 feedback loop, second half).
///
/// No-op when `enabled` is `false` (`[report] feedback_filtering`) or when
/// the feedback store yields no false-positive fingerprints — the loader is
/// fail-open, so a missing or unreadable feedback file simply disables the
/// filter. Returns the dropped findings for the report appendix.
pub(super) fn apply_feedback_filter(reports: &mut [ExpertReport], enabled: bool) -> Vec<DroppedFinding> {
    if !enabled {
        return Vec::new();
    }
    let false_positives = crate::feedback::load_false_positive_fingerprints();
    if false_positives.is_empty() {
        return Vec::new();
    }
    filter_feedback_false_positives(reports, &false_positives)
}

/// Remove findings whose fingerprint is in `false_positives` from every
/// report, returning them as [`DroppedFinding`]s with the feedback reason.
/// Findings marked `useful` are neither filtered nor boosted.
pub(super) fn filter_feedback_false_positives(
    reports: &mut [ExpertReport],
    false_positives: &HashSet<String>,
) -> Vec<DroppedFinding> {
    let mut dropped = Vec::new();
    for report in reports {
        let (kept, removed): (Vec<Finding>, Vec<Finding>) = std::mem::take(&mut report.findings)
            .into_iter()
            .partition(|f| !false_positives.contains(&f.fingerprint()));
        report.findings = kept;
        dropped.extend(removed.into_iter().map(|finding| DroppedFinding {
            finding,
            reason: FEEDBACK_FALSE_POSITIVE_REASON.to_string(),
        }));
    }
    dropped
}
