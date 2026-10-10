use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::models::*;
use crate::scoring::review;

/// Expert name → configured scoring weight (RENG-92).
///
/// Threaded from the review's `[review_experts]` config into overall scoring so
/// a configured weight actually moves the score. An expert with no entry (or a
/// weight of 0) falls back to equal weighting, so an empty map reproduces the
/// pre-RENG-92 behaviour exactly.
pub type ExpertWeights = HashMap<String, u8>;

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
    /// Expert name → configured scoring weight, used when combining expert
    /// scores into the overall assessment (RENG-92). Experts without an entry
    /// fall back to equal weighting, so an empty map scores exactly as before.
    pub expert_weights: ExpertWeights,
}

impl Default for ConsolidatorConfig {
    fn default() -> Self {
        Self {
            min_confidence: 6,
            drop_low_confidence: false,
            deduplicate: true,
            scoring: None,
            expert_weights: ExpertWeights::new(),
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

/// Verification-pass accounting for the lead summary (M18-W7).
///
/// The verifier checks every finding in batches, but the only signal the
/// report shipped before this struct existed was `dropped_findings.len()` —
/// the `checked` total and the parse-failure `skipped` count were visible
/// only in the verifier's own logs. The renderer now reads this struct so
/// the lead summary can say `Verification pass: ran on N, skipped M, dropped
/// K` instead of pretending every batch produced one.
///
/// `pairs_with_k` deliberately is NOT (just yet) plumbed through by this
/// mission — M31 owns the verifier and decides whether to count by batch or by
/// finding. The renderer treats `skipped == 0` as "we have no data yet" and
/// the line still reads correctly.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationStats {
    /// Total findings the verifier examined (kept + dropped). `0` means the
    /// pass did not run, or had nothing to examine — the renderer reads
    /// `checked == 0` as "the pass was a no-op".
    pub checked: usize,
    /// Batches (or findings, per M31) skipped because `parse_verdicts`
    /// returned an error. M18-W7 promises "ran on N, skipped M, dropped K"
    /// — a skipped batch is the *third* number, never invented from silence.
    pub skipped: usize,
    /// Findings the verifier explicitly marked as dropped.
    pub dropped: usize,
}

impl VerificationStats {
    /// Build from the kept-findings count and a dropped list: every kept
    /// finding and every dropped finding was examined, so `checked` is the
    /// sum. `skipped` stays 0 until the verifier signals a parse failure
    /// (M31 owns the count).
    pub fn from_outcome(kept: usize, dropped: usize) -> Self {
        Self {
            checked: kept + dropped,
            skipped: 0,
            dropped,
        }
    }
}

/// Summary of every truncated / partial context source injected into expert
/// prompts (M18-W5). The report renders a `## Injected Context` table from
/// this struct, so the audit's RENG-19 "截断不可见" pain point ("the
/// AGENTS.md was 13 KB but only 4 KB reached the expert — how do I know?")
/// stops being a log-only concern and becomes a visible report line.
///
/// Each row describes one source:
/// * the local `AGENTS.md`, possibly truncated to the project-config byte cap
///   (default [`crate::context::agents_md::DEFAULT_MAX_FILE_BYTES`]);
/// * each changed file whose full content was injected into the prompt
///   (per-file cap [`crate::context::file_contents::MAX_SINGLE_FILE_BYTES`]).
///
/// The pipeline (M31 / orchestrator) is the source of these rows — it already
/// builds the prompts and is the only place where the on-disk bytes and the
/// truncated bytes both exist. The consolidator's job is to carry them to
/// the renderer. Until M31 plumbs the data through, this struct is empty and
/// the `## Injected Context` section simply does not render.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InjectedContext {
    /// One row per injected source.
    pub entries: Vec<crate::context::agents_md::InjectedContextEntry>,
}

/// Per-expert parse status (M18-W8): distinguish "expert said no issues"
/// (a clean bill) from "expert output was unparseable" (a silent miss).
///
/// `parse_failed: true` is the renderer's signal to surface a ⚠️ "未审" badge
/// instead of "No issues found" — same as the existing
/// [`ExpertReport::parse_error`] field, but tracked on the metrics side so
/// dashboards and the reviewer table can flag at-risk experts by name
/// without re-parsing `raw_llm_response`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpertParseStatus {
    /// Expert name (matches `ExpertMetrics::name`).
    pub expert_name: String,
    /// True when the expert's response could not be parsed into findings.
    pub parse_failed: bool,
    /// First line of the parse error, when known — kept short so the report
    /// doesn't repeat the full `parse_error` prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Public attach helpers for fields the orchestrator sets after
/// `consolidate_with_coverage` returns. The consolidator computes the bulk of
/// the report, but a few signals only exist after the verifier / orchestrator
/// finishes (M18-W5 / W7 / W8 plumbing), so the orchestrator calls these
/// setters before the report ships.
impl ConsolidatedReport {
    /// Attach the verifier stats (M18-W7). `None` means "no run observed"
    /// and the renderer skips the line.
    pub fn with_verification_stats(mut self, verification: VerificationStats) -> Self {
        self.verification_stats = Some(verification);
        self
    }

    /// Attach the injected-context rows (M18-W5). Pass an empty `InjectedContext`
    /// to suppress the section; pass one with rows to render the table.
    pub fn with_injected_context(mut self, injected: InjectedContext) -> Self {
        self.injected_context = injected;
        self
    }
}

/// Result of the consolidation process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedReport {
    /// Consolidated findings (deduplicated, filtered).
    pub findings: Vec<Finding>,
    /// Number of findings removed for low confidence.
    pub low_confidence_removed: usize,
    /// Number of findings downgraded by the confidence filter (kept but with a
    /// lower severity). M18-W6 surfaces this so the user can see when a
    /// configuration like `adjudicate_min_severity = "low"` is competing with
    /// the confidence filter — without it, the silence of "0 candidates at or
    /// above Low" is unreadable.
    #[serde(default)]
    pub confidence_downgraded: usize,
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
    /// Per-expert parse status (M18-W8). Always populated for any review that
    /// actually ran experts; the renderer uses it to flag a silent parse
    /// failure with a ⚠️ badge instead of "No issues found".
    #[serde(default)]
    pub parse_status: Vec<ExpertParseStatus>,
    /// Verification-pass accounting (M18-W7). Populated by the pipeline
    /// after the verifier runs; `None` when no reviewer ran at all.
    #[serde(default)]
    pub verification_stats: Option<VerificationStats>,
    /// Injected-context summary (M18-W5). The orchestrator populates this
    /// from `read_local_agents_md_with_meta` and
    /// `build_file_contents_section_with_meta`; empty when neither injection
    /// ran (e.g. pure server-side reviews without a local checkout).
    #[serde(default)]
    pub injected_context: InjectedContext,
    /// High-severity findings dropped by the final adjudication pass as false
    /// positives, each with the adjudicator's reason (transparency — drops are
    /// recorded, never silent). Empty when adjudication was disabled or
    /// confirmed everything.
    #[serde(default)]
    pub adjudicated_removed: Vec<crate::team::verifier::DroppedFinding>,
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
        let (filtered, _low_conf_findings, downgraded_count) = self.filter_by_confidence(all_findings);
        all_findings = filtered;
        let low_confidence_removed = before_filter - all_findings.len();
        // M18-W6: how many findings the filter actually re-routed (vs. just
        // passing through). The number matters precisely because the default
        // `adjudicate_min_severity` is "high" — when a user drops it to
        // "low" they expect to see what the confidence filter quietly
        // re-routed to Note. Returned by the filter itself so the count is
        // exact (Note findings can be passed through unchanged by other code
        // paths, and we must not double-count those).
        let confidence_downgraded = downgraded_count;
        // M18-W6: make the filter's policy visible to the `tracing::info!`
        // line in `append_coverage_banners` so a user filtering logs by
        // `reason_code=confidence_filter_downgraded` also sees the threshold
        // and the drop policy. Thread-local is fine — the consolidator runs
        // on a single task and the helpers stay on this thread.
        set_confidence_filter_context(self.min_confidence, self.drop_low_confidence);

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
        let coverage_summary = ledger.map(|l| l.summary());
        // M18-W4: the hunk-level threshold is the source of truth — when the
        // ratio is below `COVERAGE_THRESHOLD` we treat the run as unverified
        // even if the file routing is complete (the audit's 21-of-21 / 0-of-1211
        // regression). The file-coverage score cap above is a *different* signal
        // (file routing, anti-cheat) and remains — they hold to paper at
        // different layers.
        let coverage_insufficient = coverage_summary.as_ref().map(|s| !s.is_sufficient()).unwrap_or(false);
        // The TL;DR counts are taken from the consolidated findings — the same
        // set the report publishes — so the prose can never disagree with the
        // shipped list. `score` / `risk_level` / `consensus_reached` above stay
        // derived from the raw per-expert reports (the pre-adjudication signal).
        let mut tl_dr = generate_tldr(&risk_level, &all_findings, reports.len(), false);
        append_coverage_banners(
            &mut tl_dr,
            coverage.total_files,
            coverage.reviewed_files,
            coverage.unreviewed_files.len(),
            coverage_summary.as_ref(),
            confidence_downgraded,
        );
        // Zero consolidated findings across every expert: the perfect score is
        // NOT evidence of quality — it may mean low coverage or a systemic
        // miss. Flag the assessment as unverified so the report never reads
        // "healthy / all experts approve" for an empty result. Hunk-level
        // coverage insufficiency (demonstrated-touch ratio below threshold)
        // also makes the result unverified — even when findings exist, a
        // review that demonstrably examined only part of the diff is not a
        // trustworthy overall verdict.
        //
        // This is the pre-adjudication snapshot: the pipeline calls
        // [`ConsolidatedReport::refresh_assessment`] after the adjudication
        // pass, which re-derives `tl_dr`/`unverified` from the findings that
        // actually ship (a review whose findings were all rejected as false
        // positives is unverified too — see [`generate_tldr`]).
        let unverified = all_findings.is_empty() || coverage_insufficient;

        // M18-W8: parse-status snapshot. Build from the input reports rather
        // than the (post-validation) filtered set so a parse failure is
        // visible even when every "finding" it produced was a Note. The
        // renderer reads this to put ⚠️ on the expert section.
        let parse_status = reports
            .iter()
            .filter(|r| r.parse_error.is_some())
            .map(|r| ExpertParseStatus {
                expert_name: r.expert_name.clone(),
                parse_failed: true,
                // Keep only the first line so the lead summary stays compact;
                // the full prose is on `ExpertReport::parse_error` for the
                // per-expert section.
                error: r
                    .parse_error
                    .as_deref()
                    .and_then(|e| e.lines().next())
                    .map(|s| s.to_string()),
            })
            .collect();

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
            confidence_downgraded,
            duplicates_merged,
            conflicts,
            assessment,
            consensus_reached,
            total_files: coverage.total_files,
            reviewed_files: coverage.reviewed_files,
            unreviewed_files: coverage.unreviewed_files.clone(),
            coverage: coverage_summary,
            parse_status,
            verification_stats: None,
            injected_context: InjectedContext::default(),
            adjudicated_removed: Vec::new(),
        }
    }

    /// Filter findings by minimum confidence threshold.
    ///
    /// Returns `(kept, removed, downgraded_count)` — kept includes both
    /// pass-through findings AND downgraded ones (default
    /// `drop_low_confidence = false` path); the third element is exactly the
    /// count of findings the filter re-routed, which feeds
    /// [`ConsolidatorConfig::consolidate_with_coverage`]'s M18-W6
    /// confidence-vs-adjudication transparency signal.
    fn filter_by_confidence(&self, findings: Vec<Finding>) -> (Vec<Finding>, Vec<Finding>, usize) {
        let mut kept = Vec::new();
        let mut removed = Vec::new();
        let mut downgraded_count = 0usize;

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
                    downgraded_count += 1;
                    kept.push(downgraded);
                }
            } else {
                kept.push(finding);
            }
        }

        (kept, removed, downgraded_count)
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
        // Each report is weighted by its expert's configured weight when one is
        // set; otherwise it falls back to equal weighting, so a run without a
        // mapping (or a report whose expert has no configured weight) scores
        // exactly as it did before RENG-92. `compute_weighted` normalises by
        // the weight sum, so the weights need not add up to 100 to be
        // meaningful.
        let equal_weight = 100 / reports.len() as u8;
        let data: Vec<(&str, &[Finding], u8)> = reports
            .iter()
            .map(|r| {
                let weight = self.expert_weights.get(&r.expert_name).copied().unwrap_or(equal_weight);
                (r.expert_name.as_str(), r.findings.as_slice(), weight)
            })
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
}

impl ConsolidatedReport {
    /// Recompute the assessment prose from the findings this report actually
    /// publishes.
    ///
    /// [`ConsolidatorConfig::consolidate_with_coverage`] computes the TL;DR
    /// before the adjudication pass mutates [`ConsolidatedReport::findings`]
    /// (downgrades and removals), so without this refresh the prose can state
    /// severity counts for findings that were never shipped. Called
    /// unconditionally after adjudication — including when `adjudicate` is
    /// disabled — so the counts always describe the published list.
    ///
    /// `expert_count` is the number of expert reports the consolidation ran
    /// over; it is a parameter rather than a struct field so the serialized
    /// payload keeps its shape. `score` / `risk_level` / `consensus_reached`
    /// are deliberately NOT recomputed: they remain the pre-adjudication
    /// signal (see `design/` notes in `pipeline.rs`).
    pub fn refresh_assessment(&mut self, expert_count: usize) {
        let coverage_insufficient = self.assessment.coverage_insufficient;
        let all_removed_by_adjudication = self.findings.is_empty() && !self.adjudicated_removed.is_empty();
        let mut tl_dr = generate_tldr(
            &self.assessment.risk_level,
            &self.findings,
            expert_count,
            all_removed_by_adjudication,
        );
        append_coverage_banners(
            &mut tl_dr,
            self.total_files,
            self.reviewed_files,
            self.unreviewed_files.len(),
            self.coverage.as_ref(),
            self.confidence_downgraded,
        );
        self.assessment.tl_dr = tl_dr;
        // An empty published list is never evidence of quality, whether it is
        // empty because no expert reported anything or because adjudication
        // removed everything that was reported.
        self.assessment.unverified = self.findings.is_empty() || coverage_insufficient;
    }
}

/// Append the coverage banners (file-coverage score cap and hunk-level
/// coverage insufficiency) to a TL;DR.
///
/// Shared by consolidation and [`ConsolidatedReport::refresh_assessment`] so
/// both call sites emit byte-identical banners. The insufficiency banner is
/// emitted whenever a ledger summary is present and its demonstrated-touch
/// ratio is below [`crate::coverage::COVERAGE_THRESHOLD`].
///
/// `confidence_downgraded` (M18-W6) is appended as a separate line so a user
/// who configures `adjudicate_min_severity = "low"` can see *why* the
/// adjudication pass came back with 0 candidates: the confidence filter
/// silently re-routed findings to Note below the floor. Silent loss is the
/// RENG-19/115 issue; the banner names the silent actor and the affected
/// count.
fn append_coverage_banners(
    tl_dr: &mut String,
    total_files: usize,
    reviewed_files: usize,
    unreviewed_files: usize,
    coverage: Option<&crate::coverage::CoverageSummary>,
    confidence_downgraded: usize,
) {
    if total_files > 0 && reviewed_files < total_files {
        tl_dr.push_str(&format!(
            "\n\n⚠️ Coverage: {}/{} files reviewed; {} file(s) not covered by any expert — score capped.",
            reviewed_files, total_files, unreviewed_files
        ));
    }
    if let Some(s) = coverage {
        if !s.is_sufficient() {
            // `debt` is one entry per uncovered *span*, not per hunk: a single
            // partially reviewed hunk contributes several. Name the unit
            // accordingly and give the line count first — "486 of 1351 lines
            // were reached, 865 were not" is the fact a reader is looking for,
            // and calling the spans "hunks" overstated how coarse the list was.
            tl_dr.push_str(&format!(
                "\n\n⚠️ 审查覆盖不足：{}/{} 行改动可追溯被审查（{} 行未覆盖，分布在 {} 处区间）——结果标记为不可信。\n\
                 ⚠️ Insufficient review coverage: {}/{} changed lines demonstrably reviewed ({} lines uncovered across {} range(s)) — result marked unverified.",
                s.covered_changed_lines,
                s.total_changed_lines,
                s.uncovered_changed_lines(),
                s.debt.len(),
                s.covered_changed_lines,
                s.total_changed_lines,
                s.uncovered_changed_lines(),
                s.debt.len(),
            ));
        }
    }
    if confidence_downgraded > 0 {
        // M18-W6: the banner plus an INFO log make the silent re-routing
        // visible both in the rendered report and in `logs.ndjson`. The
        // audit's RENG-19/115 trace reproduced "Adjudication pass: 0
        // candidates at or above Low, dropped 0" exactly when the default
        // confidence filter (min_confidence = 6, drop_low_confidence =
        // false) quietly re-routed findings to Note below the adjudication
        // floor; without this signal the operator has no way to see *why*
        // the adjudicator ran on nothing. The INFO log uses the
        // `reason_code` field so it is grep-able.
        tracing::info!(
            confidence_downgraded = confidence_downgraded,
            min_confidence = min_confidence_for_log(),
            drop_low_confidence = drop_low_confidence_for_log(),
            reason_code = "confidence_filter_downgraded",
            "confidence filter re-routed {confidence_downgraded} finding(s) to Note before adjudication"
        );
        tl_dr.push_str(&format!(
            "\n\n⚠️ 信心阈值过滤：{} 条 finding 被下调至 Note（默认 min_confidence = 6 不删除而是降级）。\
             如要保留原严重度以便裁决，需调高 min_confidence 或开 drop_low_confidence。\
             / Confidence filter downgraded {} finding(s) to Note (default policy: downgrade, don't drop).\
             To keep the original severity for adjudication, raise min_confidence or enable drop_low_confidence.",
            confidence_downgraded, confidence_downgraded
        ));
    }
}

// M18-W6 helpers: `append_coverage_banners` is called from
// `consolidate_with_coverage` and `refresh_assessment`, and the latter does
// not carry the filter config. To keep both call sites emitting the same
// INFO line, the banner code reads its config from thread-local state the
// consolidator sets just before the call — same trick the M18-W6 audit
// suggests for the "0 candidates" silent case. In the absence of a set
// call, the defaults are reported.
//
// The thread-locals carry `Option<u8>` / `Option<bool>` so the unset case is
// distinguishable from "set to the default"; a future refactor can replace
// them with a borrowed input rather than a thread-local.
thread_local! {
    static LAST_MIN_CONFIDENCE: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
    static LAST_DROP_LOW: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn set_confidence_filter_context(min_confidence: u8, drop_low_confidence: bool) {
    LAST_MIN_CONFIDENCE.with(|c| c.set(Some(min_confidence)));
    LAST_DROP_LOW.with(|c| c.set(Some(drop_low_confidence)));
}

fn min_confidence_for_log() -> u8 {
    LAST_MIN_CONFIDENCE.with(|c| c.get().unwrap_or(6))
}

fn drop_low_confidence_for_log() -> bool {
    LAST_DROP_LOW.with(|c| c.get().unwrap_or(false))
}

/// Generate the TL;DR summary for the *published* findings.
///
/// Every count is taken from `findings` — the consolidated, deduplicated,
/// adjudicated set that ships — and the reviewer count from `expert_count`, so
/// a single sentence can never mix two different sets.
/// `all_reported_removed_by_adjudication` selects dedicated wording for the
/// case where adjudication removed everything: "no issues found" would be a
/// lie there, because issues *were* found and then rejected as false
/// positives.
fn generate_tldr(
    risk: &RiskLevel,
    findings: &[Finding],
    expert_count: usize,
    all_reported_removed_by_adjudication: bool,
) -> String {
    if all_reported_removed_by_adjudication {
        return format!(
            "{} 位专家报告的问题全部在最终裁决中被判定为假阳性并移除，发布列表为空——这不代表代码库没有质量问题，请谨慎对待（结果标记为“未验证/不可信”）。\n\n\
             Every finding reported by the {} experts was removed by the final adjudication pass as a false positive — an empty published list here is not evidence of a clean codebase; treat with caution (result marked unverified).",
            expert_count, expert_count,
        );
    }

    if findings.is_empty() {
        return format!(
            "{} 位专家均未发现问题，但全零发现可能意味着审查覆盖率不足或系统性漏报，请谨慎对待（结果标记为“未验证/不可信”）。\n\n\
             {} experts reported no issues — this may indicate low coverage or a systemic issue; treat with caution (result marked unverified).",
            expert_count, expert_count,
        );
    }

    let total_critical = findings.iter().filter(|f| f.severity == Severity::Critical).count();
    let total_high = findings.iter().filter(|f| f.severity == Severity::High).count();
    let mut parts = Vec::new();
    if total_critical > 0 {
        parts.push(format!("{} critical", total_critical));
    }
    if total_high > 0 {
        parts.push(format!("{} high", total_high));
    }
    let remaining = findings.len().saturating_sub(total_critical + total_high);
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
