use std::collections::HashSet;

use crate::models::*;
use crate::team::lead_consolidator::{ConsolidatedReport, ConsolidatorConfig, ExpertWeights, FileCoverage};
use crate::team::verifier::DroppedFinding;

/// Run lead consolidation over the validated expert findings.
///
/// Pure computation (no LLM calls): confidence filtering, deduplication,
/// conflict detection, and overall scoring, driven by `config.report`
/// (`min_confidence`, `drop_low_confidence`) and `config.scoring`. The
/// diff-file `coverage` is threaded in so the score is capped when files
/// were not reviewed by any expert (anti-cheat: under-coverage must never
/// inflate the score).
///
/// `experts` supplies the configured expert weights (RENG-92): each expert's
/// `config.weight` moves its share of the overall score. Only experts with a
/// positive configured weight participate — an expert without one (weight 0)
/// falls back to equal weighting in the consolidator.
pub(super) fn build_consolidated_report(
    reports: &[ExpertReport],
    config: &AppConfig,
    coverage: &FileCoverage,
    ledger: Option<&crate::coverage::CoverageLedger>,
    experts: &[ExpertDef],
) -> ConsolidatedReport {
    let expert_weights: ExpertWeights = experts
        .iter()
        .filter(|e| e.config.weight > 0)
        .map(|e| (e.name.clone(), e.config.weight))
        .collect();
    ConsolidatorConfig {
        min_confidence: config.report.min_confidence,
        drop_low_confidence: config.report.drop_low_confidence,
        scoring: Some(config.scoring.clone()),
        expert_weights,
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

/// Attach the per-expert truncation accounting (RENG-79) to the coverage block
/// that ships inside the report, and return it for logging.
///
/// `report.max_findings_per_expert` is applied in the *prompt*, so an expert
/// that returned exactly the cap may be a prefix of what it actually found, and
/// nothing in the report used to say so — a run of 39 findings over 10 experts
/// read as a complete list while 35 of them were five experts parked on the
/// same cap.
///
/// The count travels in [`crate::coverage::CoverageSummary`] (serialized inside
/// `reviews.result`, so the UI can read it) rather than on a struct of its own:
/// "how much of the diff did we reach" and "how complete is this list" are the
/// two halves of one question, and a second field would give the halves a way
/// to disagree.
///
/// Take `truncation` from the reports **as the experts returned them** — the
/// caller measures before validation trims out-of-diff lines — so a capped
/// expert whose lines were partly dropped is still counted as capped. A report
/// with no coverage block (the backward-compatible consolidation path) is left
/// untouched; the returned summary is accurate either way.
pub(super) fn attach_findings_truncation(
    consolidated: &mut ConsolidatedReport,
    truncation: crate::coverage::TruncationSummary,
) -> crate::coverage::TruncationSummary {
    if let Some(coverage) = consolidated.coverage.as_mut() {
        coverage.findings_truncation = truncation.clone();
    }
    truncation
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
