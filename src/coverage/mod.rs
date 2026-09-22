//! Coverage ledger: tracks which changed hunk ranges of the reviewed diff were
//! demonstrably examined, as a quantitative basis for distrusting zero-finding
//! results.
//!
//! Inspired by Imtiaz et al. 2023 ("code review coverage" = the proportion of
//! changes traceable to review evidence) and kodus's `coverage-ledger.ts`
//! (`~/Workspace/github.com/kodus-ai/libs/.../coverage-ledger.ts`): per file we
//! record the changed ranges parsed from the diff hunks (`changed_ranges`) and
//! the ranges the expert demonstrably touched (`touched_ranges`, the union of
//! line references found in that expert's findings).
//!
//! Tracking granularity note: the current architecture injects the full diff
//! into every expert in one shot (no per-expert `readFile` tool calls), so
//! every expert *sees* the whole diff. The ledger nevertheless computes
//! **evidence-based** coverage — the lines that findings actually reference —
//! so an all-zero run is quantified as 0% demonstrated coverage ("zero
//! findings ≠ clean"), and a sparse run (findings touching only part of the
//! diff) is flagged as under-covered. When per-expert read tracking lands
//! (chunked injection / explicit read calls), `touched_ranges` can be seeded
//! from the actually-read ranges instead; the summary/debt machinery below is
//! unchanged.

use crate::models::DiffHunk;
use serde::{Deserialize, Serialize};

/// Minimum fraction of changed lines that must be demonstrably touched for the
/// review to be considered sufficiently covered (mirrors kodus's 0.7 gate).
pub const COVERAGE_THRESHOLD: f64 = 0.7;

/// Whether a file target has been touched by any expert finding.
/// Whether a file target has been touched by any expert finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoverageStatus {
    /// No expert has yet referenced any line in this file's changed ranges.
    Pending,
    /// At least one expert finding references lines in this file.
    Touched,
}

/// Per-file coverage target: the changed ranges this review must cover and the
/// ranges demonstrably read/referenced so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageTarget {
    /// Relative file path (matches `DiffFile.path`).
    pub file: String,
    /// Changed line ranges (1-based, inclusive) parsed from the diff hunks.
    pub changed_ranges: Vec<(u32, u32)>,
    /// Union of line ranges touched by expert findings (sorted, merged).
    pub touched_ranges: Vec<(u32, u32)>,
    /// Current coverage status of this file (pending or touched).
    pub status: CoverageStatus,
    /// Experts whose findings touched this file.
    pub touched_by: Vec<String>,
}

/// A changed range no finding demonstrated a touch on — the "coverage debt"
/// that future iterations can feed back to experts for a re-read pass.
///
/// Granularity is the *uncovered span*, not the hunk: a hunk whose 10–17 is
/// referenced only at line 12 yields debt `13–17`, because "somebody looked at
/// one line of it" is not "somebody reviewed it". Reporting whole hunks only
/// made the debt list vanish exactly when it mattered most — a sparse run whose
/// few findings sprinkle one reference into every hunk leaves no hunk
/// completely untouched, so the old per-hunk rule reported **no** uncovered
/// ranges alongside a 12% coverage ratio (RENG-79).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncoveredRange {
    pub file: String,
    /// 1-based inclusive range in the new file.
    pub range: (u32, u32),
}

/// Aggregate coverage across the whole diff: how much of it the findings
/// demonstrably reached (lines + exact uncovered spans), and how complete each
/// expert's own list can be trusted to be.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub total_changed_lines: usize,
    pub covered_changed_lines: usize,
    /// `covered / total`; 1.0 when there are no changed lines (nothing to cover).
    pub ratio: f64,
    /// Changed ranges no finding referenced (coverage debt), sorted by file and
    /// start line. Each entry is the exact uncovered span within a changed
    /// range, so a partially referenced hunk still contributes its gaps.
    pub debt: Vec<UncoveredRange>,
    /// Per-expert truncation accounting (RENG-79): which experts returned
    /// exactly `report.max_findings_per_expert` findings and may therefore have
    /// withheld the rest. Coverage of the *diff* and completeness of the
    /// *list* are the two halves of "can I trust this report?", so they travel
    /// in one block.
    ///
    /// Empty (and `cap` 0) when nothing hit the cap — the common case.
    #[serde(default)]
    pub findings_truncation: TruncationSummary,
}

impl CoverageSummary {
    /// Whether demonstrated coverage meets the threshold.
    pub fn is_sufficient(&self) -> bool {
        self.ratio >= COVERAGE_THRESHOLD
    }

    /// Changed lines no finding demonstrated a touch on (`total - covered`).
    pub fn uncovered_changed_lines(&self) -> usize {
        self.total_changed_lines.saturating_sub(self.covered_changed_lines)
    }

    /// Debt at a size a report can print, plus how many entries were withheld.
    ///
    /// A sparse review over a large diff produces one debt entry per uncovered
    /// span — hundreds of them — which on one markdown line is unreadable and
    /// on a UI panel is a wall of text. Callers render the first `max` entries
    /// and state the remainder (`and N more`) rather than either dumping the
    /// lot or silently dropping it: the whole point of the list is that the
    /// reader can tell how much was *not* reviewed.
    ///
    /// `max == 0` yields everything, matching "no limit".
    pub fn debt_preview(&self, max: usize) -> (&[UncoveredRange], usize) {
        if max == 0 || self.debt.len() <= max {
            return (&self.debt, 0);
        }
        (&self.debt[..max], self.debt.len() - max)
    }
}

/// The ledger for one review run: one target per changed file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoverageLedger {
    pub targets: Vec<CoverageTarget>,
}

impl CoverageLedger {
    /// Build the ledger's changed ranges from parsed diff files.
    /// Files with no changed (new-file) ranges — e.g. pure deletions — are
    /// skipped: there is nothing to cover in the new file.
    pub fn from_diff_files(diff_files: &[(String, Vec<DiffHunk>)]) -> Self {
        let targets = diff_files
            .iter()
            .filter_map(|(path, hunks)| {
                let changed_ranges: Vec<(u32, u32)> = hunks.iter().filter_map(hunk_new_range).collect();
                if changed_ranges.is_empty() {
                    return None;
                }
                Some(CoverageTarget {
                    file: path.clone(),
                    changed_ranges,
                    touched_ranges: Vec::new(),
                    status: CoverageStatus::Pending,
                    touched_by: Vec::new(),
                })
            })
            .collect();
        Self { targets }
    }

    /// Record that `by` (an expert) touched `range` (1-based inclusive) in
    /// `file`. The range is merged into that file's touched union.
    pub fn mark_touched(&mut self, file: &str, range: (u32, u32), by: &str) {
        if let Some(target) = self.targets.iter_mut().find(|t| t.file == file) {
            target.touched_ranges = union_push(&target.touched_ranges, range);
            target.status = CoverageStatus::Touched;
            if !target.touched_by.iter().any(|e| e == by) {
                target.touched_by.push(by.to_string());
            }
        }
    }

    /// Aggregate coverage: fraction of changed lines touched by any finding,
    /// plus the per-range debt (the exact uncovered spans).
    pub fn summary(&self) -> CoverageSummary {
        let mut total = 0usize;
        let mut covered = 0usize;
        let mut debt = Vec::new();
        for target in &self.targets {
            // Canonicalise the gaps (`union_push` sorts, dedupes and coalesces
            // adjacent spans) so consumers get sorted, non-overlapping debt.
            let mut ranges: Vec<(u32, u32)> = Vec::new();
            for (s, e) in uncovered_within(&target.changed_ranges, &target.touched_ranges) {
                ranges = union_push(&ranges, (s, e));
            }
            debt.extend(ranges.into_iter().map(|range| UncoveredRange {
                file: target.file.clone(),
                range,
            }));
            for &(a, b) in &target.changed_ranges {
                let len = (b.saturating_sub(a) + 1) as usize;
                total += len;
                covered += target
                    .touched_ranges
                    .iter()
                    .map(|&(s, e)| overlap_len(s, e, a, b))
                    .sum::<usize>();
            }
        }
        let ratio = if total == 0 { 1.0 } else { covered as f64 / total as f64 };
        CoverageSummary {
            total_changed_lines: total,
            covered_changed_lines: covered,
            ratio,
            debt,
            findings_truncation: TruncationSummary::default(),
        }
    }
}

// ─── finding-level recall: per-expert truncation (RENG-79) ──────────────

/// The key an expert is asked to declare when `report.max_findings_per_expert`
/// stopped it from listing everything it found (see
/// [`crate::prompt`]'s review system template). An integer, at the top level of
/// the response's YAML block; the reader accepts any indentation, since models
/// nest it under `review:` about as often as they leave it flush.
pub const DECLARED_OMITTED_KEY: &str = "findings_omitted";

/// Truncation accounting for one expert report.
///
/// `report.max_findings_per_expert` is applied **in the prompt** — the model is
/// told `Max findings: N` and is expected to stop there — so an expert that
/// returns exactly `N` is at the limit and may have found more. Nothing in the
/// report said so, which is how a run of 39 findings spread over 10 experts
/// read as a complete list when 35 of those findings were five experts sitting
/// on the same cap (RENG-79). This restores the missing sentence: *listed N of
/// at least N, ask again or raise the cap for the rest*.
///
/// Note the unit: one entry describes one **report**, not one expert. A large
/// PR is chunked, so the same expert answers more than once and appears here
/// more than once — count entries, not distinct `expert` names, and never sum
/// `listed` across entries of the same name and call it "the expert's findings".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertTruncation {
    /// Expert whose report is (possibly) a prefix of what it found. May repeat
    /// across entries of one summary (chunked review).
    pub expert: String,
    /// Findings this report carries — as the expert returned them, before any
    /// later pass trims out-of-diff lines (see
    /// [`TruncationSummary::from_reports`]).
    pub listed: usize,
    /// `report.max_findings_per_expert` for this run.
    pub cap: usize,
    /// Findings the expert declared it left out, when it said so (`None` when
    /// the model did not answer — the at-cap signal still stands).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_omitted: Option<usize>,
}

impl ExpertTruncation {
    /// Whether the expert stopped at the configured limit, i.e. its list may be
    /// a prefix rather than the whole set. A `cap` of 0 means "no limit".
    pub fn at_cap(&self) -> bool {
        self.cap > 0 && self.listed >= self.cap
    }

    /// Findings this expert explicitly reported omitting. Never invents a
    /// number: absent declaration → `None`, even when [`Self::at_cap`].
    pub fn omitted(&self) -> Option<usize> {
        self.declared_omitted
    }
}

/// Truncation accounting for one review run: which expert reports may have had
/// findings withheld by the configured cap, and how many were declared.
///
/// This struct is part of the payload a UI reads
/// (`consolidated.coverage.findings_truncation`), so its fields are worded for a
/// consumer that did not read this file. Two readings a caller can get wrong:
///
/// - **`experts` is one entry per report.** A chunked review asks the same
///   expert several times; the name repeats. Use [`Self::at_cap_count`] for
///   "how many lists are affected" and never treat `experts` as a set of
///   distinct experts.
/// - **`declared_omitted_total` alone cannot tell "nobody declared" from "the
///   experts declared zero".** It is a *sum*, so `0` is ambiguous; printing it
///   beside a truncation warning would assert "0 omitted" on the strength of
///   silence. [`Self::declaring_reports`] is the count that resolves it: render
///   the total only when it is greater than zero, and keep it out of the report
///   otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TruncationSummary {
    /// `report.max_findings_per_expert` the run used.
    pub cap: usize,
    /// Every expert **report** that returned at least `cap` findings, in report
    /// order. One expert may appear more than once (one entry per chunk).
    pub experts: Vec<ExpertTruncation>,
    /// How many entries of [`Self::experts`] stated a number at all. Distinguishes
    /// a total of 0 that means "every declaring expert said zero" from one that
    /// means "not one expert answered the question".
    #[serde(default)]
    pub declaring_reports: usize,
    /// Sum of the experts' own declarations. `0` when every expert declared zero
    /// **or** when every at-cap expert stayed silent — check
    /// [`Self::declaring_reports`] before presenting this as "N omitted".
    pub declared_omitted_total: usize,
}

impl TruncationSummary {
    /// Whether any expert hit the cap.
    pub fn is_empty(&self) -> bool {
        self.experts.is_empty()
    }

    /// Number of experts that hit the cap.
    pub fn at_cap_count(&self) -> usize {
        self.experts.len()
    }

    /// Build from `(expert, listed)` counts. Experts below the cap are not
    /// truncation entries at all; `cap == 0` means no limit and yields an empty
    /// summary. Declared counts start out unknown — see [`Self::from_reports`].
    pub fn from_counts<I>(counts: I, cap: usize) -> Self
    where
        I: IntoIterator<Item = (String, usize)>,
    {
        let experts: Vec<ExpertTruncation> = counts
            .into_iter()
            .map(|(expert, listed)| ExpertTruncation {
                expert,
                listed,
                cap,
                declared_omitted: None,
            })
            .filter(|e| e.at_cap())
            .collect();
        Self {
            cap,
            experts,
            declaring_reports: 0,
            declared_omitted_total: 0,
        }
    }

    /// Build from expert reports, picking up each report's own declaration of
    /// what the cap made it leave out.
    ///
    /// One entry per **report**, not per expert name: a large PR is chunked, so
    /// the same expert answers more than once and each answer carries its own
    /// cap (RENG-79's report of ten experts each sitting at five is exactly
    /// this shape). Measure it on the reports **as the experts returned them**
    /// — before validation trims findings whose line is outside the diff — so a
    /// capped expert is not laundered into an under-cap one by a later pass.
    pub fn from_reports(reports: &[crate::models::ExpertReport], cap: usize) -> Self {
        let experts: Vec<ExpertTruncation> = reports
            .iter()
            .map(|r| ExpertTruncation {
                expert: r.expert_name.clone(),
                listed: r.findings.len(),
                cap,
                declared_omitted: parse_declared_omitted(&r.raw_llm_response),
            })
            .filter(ExpertTruncation::at_cap)
            .collect();
        let declaring_reports = experts.iter().filter(|e| e.declared_omitted.is_some()).count();
        let declared_omitted_total = experts.iter().filter_map(|e| e.declared_omitted).sum();
        Self {
            cap,
            experts,
            declaring_reports,
            declared_omitted_total,
        }
    }
}

/// Read the `findings_omitted` integer an expert declared in its response.
///
/// Deliberately forgiving — a model that ignores the instruction yields `None`
/// and the caller falls back to the at-cap signal — but strict enough not to
/// invent a number from prose: the key must start a line (optionally indented)
/// and be followed by an integer, nothing else but a trailing comment. The
/// first such line wins.
pub fn parse_declared_omitted(raw_response: &str) -> Option<usize> {
    for line in raw_response.lines() {
        let line = line.trim_end();
        let Some(rest) = line.trim_start().strip_prefix(DECLARED_OMITTED_KEY) else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let value = value.trim();
        let digits: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        // Only a bare integer or an integer followed by a YAML comment counts:
        // "findings_omitted: none were listed" must not be read as a number.
        let tail = value[digits.len()..].trim_start();
        if !tail.is_empty() && !tail.starts_with('#') {
            continue;
        }
        if let Ok(n) = digits.parse::<usize>() {
            return Some(n);
        }
    }
    None
}

/// New-file range of a hunk (1-based inclusive); `None` for pure-deletion
/// hunks (`new_lines == 0`), which have no lines to cover in the new file.
fn hunk_new_range(hunk: &DiffHunk) -> Option<(u32, u32)> {
    if hunk.new_lines == 0 {
        return None;
    }
    Some((
        hunk.new_start,
        hunk.new_start.saturating_add(hunk.new_lines.saturating_sub(1)),
    ))
}

/// Overlap length of `[s,e]` with `[a,b]` (inclusive ranges), 0 when disjoint.
fn overlap_len(s: u32, e: u32, a: u32, b: u32) -> usize {
    let lo = s.max(a);
    let hi = e.min(b);
    if lo > hi {
        0
    } else {
        (hi - lo + 1) as usize
    }
}

/// The ranges of `changed` that `touched` does not cover — the set difference,
/// as 1-based inclusive ranges.
///
/// Both inputs are sorted, non-overlapping unions (`changed_ranges` comes from
/// the diff hunks, `touched_ranges` from [`union_push`]). A partially covered
/// changed range therefore contributes its gaps rather than nothing: `changed
/// 10–17` with `touched 12–13` yields `10–11` and `14–17`. Ranges of `touched`
/// that fall outside `changed` are ignored; an empty result means everything
/// changed was demonstrably touched.
fn uncovered_within(changed: &[(u32, u32)], touched: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &(a, b) in changed {
        if a > b {
            continue;
        }
        let mut cursor = a;
        let mut closed = false;
        for &(s, e) in touched {
            if s > b {
                break;
            }
            if e < cursor {
                continue;
            }
            if s > cursor {
                out.push((cursor, s - 1));
            }
            if e >= b {
                closed = true;
                break;
            }
            cursor = e + 1;
        }
        if !closed {
            out.push((cursor, b));
        }
    }
    out
}

/// Insert `new` into a sorted, non-overlapping range list, re-sorting and
/// merging overlapping or adjacent ranges so the union stays canonical.
fn union_push(ranges: &[(u32, u32)], new: (u32, u32)) -> Vec<(u32, u32)> {
    let mut all: Vec<(u32, u32)> = ranges.to_vec();
    all.push(new);
    all.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(all.len());
    for (s, e) in all {
        if let Some(last) = out.last_mut() {
            // Overlap or adjacency ([10,17] + [18,20] → [10,20]).
            if s <= last.1.saturating_add(1) {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        out.push((s, e));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(new_start: u32, new_lines: u32) -> DiffHunk {
        DiffHunk {
            header: format!("@@ -1,1 +{new_start},{new_lines} @@"),
            old_start: 1,
            old_lines: 1,
            new_start,
            new_lines,
            lines: vec![],
        }
    }

    /// Minimal finding — the truncation tests only care about the count.
    fn finding() -> crate::models::Finding {
        crate::models::Finding {
            file: "a.c".to_string(),
            line: Some(1),
            line_end: None,
            severity: crate::models::Severity::Medium,
            confidence: 8,
            category: "correctness".to_string(),
            title: "t".to_string(),
            summary: "s".to_string(),
            evidence: "e".to_string(),
            impact: "i".to_string(),
            recommendation: "r".to_string(),
            effort: crate::models::Effort::Small,
            expert_name: "security".to_string(),
            expert_role: String::new(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    /// Minimal report with `listed` findings and a raw response body.
    fn report(expert: &str, listed: usize, raw: &str) -> crate::models::ExpertReport {
        crate::models::ExpertReport {
            expert_name: expert.to_string(),
            findings: (0..listed).map(|_| finding()).collect(),
            markdown: String::new(),
            raw_llm_response: raw.to_string(),
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }
    }

    #[test]
    fn from_diff_files_parses_changed_ranges() {
        let diff_files = vec![
            ("a.c".to_string(), vec![hunk(10, 8)]),  // 10..=17
            ("b.c".to_string(), vec![hunk(30, 11)]), // 30..=40
            ("del.c".to_string(), vec![hunk(5, 0)]), // pure deletion → skipped
            ("empty.c".to_string(), vec![]),         // no hunks → skipped
        ];
        let ledger = CoverageLedger::from_diff_files(&diff_files);
        assert_eq!(
            ledger.targets.len(),
            2,
            "pure-deletion and hunk-less files must be skipped"
        );
        assert_eq!(ledger.targets[0].changed_ranges, vec![(10, 17)]);
        assert_eq!(ledger.targets[1].changed_ranges, vec![(30, 40)]);
    }

    #[test]
    fn mark_touched_merges_union_and_sets_status() {
        let diff_files = vec![("a.c".to_string(), vec![hunk(10, 20)])]; // 10..=29
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        ledger.mark_touched("a.c", (12, 15), "security");
        ledger.mark_touched("a.c", (14, 22), "security"); // overlaps → union [12,22]
        ledger.mark_touched("a.c", (40, 45), "quality"); // disjoint → separate range
        assert_eq!(ledger.targets[0].status, CoverageStatus::Touched);
        assert_eq!(ledger.targets[0].touched_ranges, vec![(12, 22), (40, 45)]);
        assert_eq!(ledger.targets[0].touched_by, vec!["security", "quality"]);
    }

    #[test]
    fn adjacent_ranges_merge_into_union() {
        let diff_files = vec![("a.c".to_string(), vec![hunk(1, 100)])];
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        ledger.mark_touched("a.c", (10, 17), "x");
        ledger.mark_touched("a.c", (18, 25), "x"); // adjacent → merged
        assert_eq!(ledger.targets[0].touched_ranges, vec![(10, 25)]);
    }

    #[test]
    fn summary_ratio_and_debt() {
        // a.c 10..=17 (8 lines), b.c 30..=40 (11 lines), c.c 50..=55 (6 lines)
        // → 25 changed lines.
        let diff_files = vec![
            ("a.c".to_string(), vec![hunk(10, 8)]),
            ("b.c".to_string(), vec![hunk(30, 11)]),
            ("c.c".to_string(), vec![hunk(50, 6)]),
        ];
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        ledger.mark_touched("a.c", (10, 17), "security"); // 8/8 covered
        ledger.mark_touched("b.c", (30, 40), "quality"); // 11/11 covered
                                                         // c.c untouched → fully uncovered (coverage debt).

        let summary = ledger.summary();
        assert_eq!(summary.total_changed_lines, 25);
        assert_eq!(summary.covered_changed_lines, 19);
        assert!((summary.ratio - 19.0 / 25.0).abs() < 1e-9);
        assert_eq!(summary.debt.len(), 1);
        assert_eq!(summary.debt[0].file, "c.c");
        assert_eq!(summary.debt[0].range, (50, 55));
        assert!(summary.is_sufficient(), "0.76 >= 0.7 must be sufficient");
    }

    #[test]
    fn zero_touch_yields_zero_coverage_and_insufficient() {
        let diff_files = vec![("a.c".to_string(), vec![hunk(10, 8)])];
        let ledger = CoverageLedger::from_diff_files(&diff_files);
        let summary = ledger.summary();
        assert_eq!(summary.covered_changed_lines, 0);
        assert_eq!(summary.ratio, 0.0);
        assert_eq!(summary.debt.len(), 1);
        assert!(
            !summary.is_sufficient(),
            "zero demonstrated coverage must be insufficient"
        );
    }

    #[test]
    fn no_changed_lines_is_fully_covered() {
        let ledger = CoverageLedger::from_diff_files(&[]);
        let summary = ledger.summary();
        assert_eq!(summary.total_changed_lines, 0);
        assert_eq!(summary.ratio, 1.0);
        assert!(summary.is_sufficient());
        assert!(summary.debt.is_empty());
    }

    #[test]
    fn partial_touch_below_threshold_is_insufficient() {
        // 19 changed lines, touch only 2 → ratio ≈ 0.105 < 0.7.
        let diff_files = vec![
            ("a.c".to_string(), vec![hunk(10, 8)]),
            ("b.c".to_string(), vec![hunk(30, 11)]),
        ];
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        ledger.mark_touched("a.c", (10, 11), "security");
        let summary = ledger.summary();
        assert!(!summary.is_sufficient());
    }

    #[test]
    fn file_scoped_touch_covers_whole_changed_range() {
        let diff_files = vec![("a.c".to_string(), vec![hunk(10, 8)])];
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        // A file-level finding (line: None) marks the whole file read.
        ledger.mark_touched("a.c", (10, 17), "security");
        assert!(ledger.summary().is_sufficient());
    }

    // ─── pure helpers ──────────────────────────

    #[test]
    fn hunk_new_range_is_start_to_start_plus_lines_minus_one() {
        assert_eq!(hunk_new_range(&hunk(10, 8)), Some((10, 17)));
        assert_eq!(hunk_new_range(&hunk(1, 1)), Some((1, 1)));
        // Pure-deletion hunks have no new lines to cover.
        assert_eq!(hunk_new_range(&hunk(5, 0)), None);
        // Zero new_lines still yields None even with a nonzero start.
        assert_eq!(hunk_new_range(&hunk(9, 0)), None);
    }

    #[test]
    fn hunk_new_range_never_underflows_start() {
        // new_start 0 + 0 lines would saturate; new_lines 0 → None anyway.
        assert_eq!(hunk_new_range(&hunk(0, 0)), None);
        assert_eq!(hunk_new_range(&hunk(0, 1)), Some((0, 0)));
    }

    #[test]
    fn overlap_len_counts_shared_inclusive_range() {
        assert_eq!(overlap_len(10, 20, 10, 20), 11, "identical ranges");
        assert_eq!(overlap_len(10, 20, 15, 17), 3, "contained");
        assert_eq!(overlap_len(10, 20, 5, 12), 3, "left overlap");
        assert_eq!(overlap_len(10, 20, 18, 30), 3, "right overlap");
        assert_eq!(overlap_len(10, 20, 21, 30), 0, "disjoint (gap of one)");
        assert_eq!(overlap_len(10, 20, 30, 40), 0, "fully disjoint");
    }

    #[test]
    fn overlap_len_single_point_and_reversed_ranges() {
        assert_eq!(overlap_len(5, 5, 5, 5), 1, "point overlap");
        assert_eq!(overlap_len(5, 5, 6, 6), 0, "adjacent points do not overlap");
        // Reversed inputs are not normalised by the caller contract.
        assert_eq!(overlap_len(20, 10, 10, 20), 0);
    }

    #[test]
    fn union_push_merges_overlapping_and_adjacent_ranges() {
        assert_eq!(union_push(&[(10, 17)], (15, 20)), vec![(10, 20)]);
        assert_eq!(union_push(&[(10, 17)], (18, 20)), vec![(10, 20)], "adjacent merges");
        assert_eq!(union_push(&[(10, 17)], (9, 10)), vec![(9, 17)]);
    }

    #[test]
    fn union_push_keeps_disjoint_ranges_sorted() {
        assert_eq!(union_push(&[(1, 2), (10, 12)], (5, 6)), vec![(1, 2), (5, 6), (10, 12)]);
        // Out-of-order input is re-sorted.
        assert_eq!(union_push(&[(10, 12), (1, 2)], (5, 6)), vec![(1, 2), (5, 6), (10, 12)]);
    }

    #[test]
    fn union_push_empty_input_builds_singleton() {
        assert_eq!(union_push(&[], (3, 4)), vec![(3, 4)]);
    }

    #[test]
    fn union_push_merges_chains_into_one_span() {
        assert_eq!(
            union_push(&[(1, 2), (4, 5)], (3, 3)),
            vec![(1, 5)],
            "gap of one merges through"
        );
        assert_eq!(union_push(&[(1, 2), (4, 5)], (3, 4)), vec![(1, 5)]);
    }

    // ─── uncovered_within (exact debt spans) ─────

    #[test]
    fn uncovered_within_reports_the_complement() {
        // Fully untouched hunk → the whole hunk.
        assert_eq!(uncovered_within(&[(10, 17)], &[]), vec![(10, 17)]);
        // Partially touched → both gaps, not "covered" and not the whole hunk.
        assert_eq!(uncovered_within(&[(10, 17)], &[(12, 13)]), vec![(10, 11), (14, 17)]);
        // Touch at the edges leaves only the middle.
        assert_eq!(uncovered_within(&[(10, 17)], &[(10, 10), (17, 17)]), vec![(11, 16)]);
        // Fully touched → nothing.
        assert_eq!(uncovered_within(&[(10, 17)], &[(10, 17)]), Vec::new());
        // A superset touch covers the hunk.
        assert_eq!(uncovered_within(&[(10, 17)], &[(5, 25)]), Vec::new());
    }

    #[test]
    fn uncovered_within_ignores_touches_outside_the_changed_range() {
        assert_eq!(uncovered_within(&[(10, 17)], &[(1, 5), (30, 40)]), vec![(10, 17)]);
        // A touch that starts before the range still covers its start.
        assert_eq!(uncovered_within(&[(10, 17)], &[(5, 12)]), vec![(13, 17)]);
    }

    #[test]
    fn uncovered_within_walks_multiple_changed_ranges() {
        assert_eq!(
            uncovered_within(&[(10, 12), (20, 22)], &[(11, 11)]),
            vec![(10, 10), (12, 12), (20, 22)]
        );
    }

    #[test]
    fn uncovered_within_handles_degenerate_input() {
        assert_eq!(uncovered_within(&[], &[(1, 2)]), Vec::new());
        assert_eq!(uncovered_within(&[(10, 9)], &[]), Vec::new(), "empty changed range");
        assert_eq!(uncovered_within(&[(7, 7)], &[(7, 7)]), Vec::new());
        assert_eq!(uncovered_within(&[(7, 7)], &[]), vec![(7, 7)]);
    }

    #[test]
    fn uncovered_within_does_not_overflow_at_u32_max() {
        assert_eq!(
            uncovered_within(&[(u32::MAX - 1, u32::MAX)], &[]),
            vec![(u32::MAX - 1, u32::MAX)]
        );
        assert_eq!(
            uncovered_within(&[(u32::MAX - 1, u32::MAX)], &[(u32::MAX, u32::MAX)]),
            vec![(u32::MAX - 1, u32::MAX - 1)]
        );
    }

    // ─── RENG-79: sparse runs must still list what they missed ───

    #[test]
    fn sparse_touch_lists_the_uncovered_spans_not_nothing() {
        // The regression this fixes: one finding per hunk used to leave the debt
        // list EMPTY, so a 12%-covered run printed a ratio and no ranges.
        let diff_files = vec![
            ("a.c".to_string(), vec![hunk(10, 100)]), // 10..=109
            ("b.c".to_string(), vec![hunk(200, 100)]),
        ];
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        ledger.mark_touched("a.c", (50, 50), "security"); // one line of 100
        ledger.mark_touched("b.c", (250, 250), "quality");

        let summary = ledger.summary();
        assert!(summary.ratio < 0.02, "2 of 200 lines: {}", summary.ratio);
        assert_eq!(
            summary.debt,
            vec![
                UncoveredRange {
                    file: "a.c".to_string(),
                    range: (10, 49)
                },
                UncoveredRange {
                    file: "a.c".to_string(),
                    range: (51, 109)
                },
                UncoveredRange {
                    file: "b.c".to_string(),
                    range: (200, 249)
                },
                UncoveredRange {
                    file: "b.c".to_string(),
                    range: (251, 299)
                },
            ],
            "every gap must be listed, and the covered line excluded"
        );
        assert_eq!(summary.uncovered_changed_lines(), 198);
    }

    #[test]
    fn debt_stays_empty_when_everything_is_touched() {
        let diff_files = vec![("a.c".to_string(), vec![hunk(10, 8)])];
        let mut ledger = CoverageLedger::from_diff_files(&diff_files);
        ledger.mark_touched("a.c", (10, 17), "security");
        let summary = ledger.summary();
        assert!(summary.debt.is_empty());
        assert_eq!(summary.uncovered_changed_lines(), 0);
        assert_eq!(summary.debt_preview(3), (&[][..], 0));
    }

    #[test]
    fn debt_preview_caps_the_list_and_reports_the_remainder() {
        let diff_files = vec![("a.c".to_string(), vec![hunk(1, 10), hunk(30, 10), hunk(60, 10)])];
        let ledger = CoverageLedger::from_diff_files(&diff_files);
        let summary = ledger.summary();
        assert_eq!(summary.debt.len(), 3);

        let (head, remaining) = summary.debt_preview(2);
        assert_eq!(head, &summary.debt[..2]);
        assert_eq!(remaining, 1);
        assert_eq!(summary.debt_preview(3).1, 0, "exactly enough is not truncated");
        assert_eq!(summary.debt_preview(0).0.len(), 3, "0 means no limit");
    }

    // ─── RENG-79: per-expert truncation accounting ───

    #[test]
    fn truncation_flags_only_experts_at_the_cap() {
        let summary = TruncationSummary::from_counts(
            vec![
                ("security".to_string(), 5),
                ("quality".to_string(), 2),
                ("performance".to_string(), 7),
            ],
            5,
        );
        assert_eq!(summary.cap, 5);
        assert_eq!(summary.at_cap_count(), 2, "5 and 7 are at/over the cap");
        assert_eq!(
            summary.experts.iter().map(|e| e.expert.as_str()).collect::<Vec<_>>(),
            vec!["security", "performance"]
        );
        assert!(!summary.is_empty());
        assert!(summary.experts.iter().all(|e| e.at_cap()));
        assert_eq!(summary.declared_omitted_total, 0, "nothing declared yet");
    }

    #[test]
    fn truncation_is_empty_when_nobody_reaches_the_cap() {
        let summary = TruncationSummary::from_counts(vec![("security".to_string(), 4)], 5);
        assert!(summary.is_empty());
        assert_eq!(summary.at_cap_count(), 0);
    }

    #[test]
    fn a_cap_of_zero_means_no_limit() {
        let summary = TruncationSummary::from_counts(vec![("security".to_string(), 99)], 0);
        assert!(summary.is_empty(), "an uncapped run cannot be truncated");
        assert_eq!(summary.cap, 0);
    }

    #[test]
    fn truncation_never_invents_an_omitted_count() {
        let summary = TruncationSummary::from_counts(vec![("security".to_string(), 5)], 5);
        assert_eq!(summary.experts[0].omitted(), None);
        assert!(summary.experts[0].at_cap(), "at cap is knowable without the model");
    }

    #[test]
    fn truncation_reads_the_experts_own_declaration() {
        let reports = vec![
            report("security", 5, "findings:\n  - title: x\nfindings_omitted: 7\n"),
            report("quality", 5, "findings:\n  - title: x\n"),
        ];
        let summary = TruncationSummary::from_reports(&reports, 5);
        assert_eq!(summary.at_cap_count(), 2);
        assert_eq!(summary.experts[0].omitted(), Some(7));
        assert_eq!(summary.experts[1].omitted(), None, "silence is not a zero");
        assert_eq!(summary.declared_omitted_total, 7);
    }

    #[test]
    fn truncation_from_reports_measures_the_count_it_is_given() {
        // The caller hands over the reports as the experts returned them; a
        // below-cap expert is not a truncation entry even if another is.
        let reports = vec![report("security", 5, ""), report("quality", 1, "")];
        let summary = TruncationSummary::from_reports(&reports, 5);
        assert_eq!(summary.declared_omitted_total, 0);
        assert_eq!(summary.declaring_reports, 0, "nobody answered the question");
        assert_eq!(summary.experts.iter().map(|e| e.listed).collect::<Vec<_>>(), vec![5]);
    }

    #[test]
    fn truncation_counts_one_entry_per_report_not_per_expert() {
        // A chunked PR asks the same expert several times; a UI must see three
        // lists, not "one expert with 15 findings".
        let reports = vec![
            report("security", 5, "findings_omitted: 2\n"),
            report("security", 5, "findings_omitted: 2\n"),
            report("security", 5, ""),
        ];
        let summary = TruncationSummary::from_reports(&reports, 5);
        assert_eq!(summary.at_cap_count(), 3, "three reports hit the cap");
        assert_eq!(summary.declaring_reports, 2);
        assert_eq!(summary.declared_omitted_total, 4);
        assert_eq!(
            summary.experts.iter().filter(|e| e.expert == "security").count(),
            3,
            "the name repeats on purpose"
        );
    }

    #[test]
    fn a_zero_total_is_distinguishable_from_nobody_declaring() {
        // The trap this field exists for: both summaries below have
        // declared_omitted_total == 0, and only `declaring_reports` says which
        // one may be printed as "N omitted".
        let silent = TruncationSummary::from_reports(&[report("a", 5, "")], 5);
        assert_eq!(silent.declared_omitted_total, 0);
        assert_eq!(silent.declaring_reports, 0);

        let declared_zero = TruncationSummary::from_reports(&[report("a", 5, "findings_omitted: 0\n")], 5);
        assert_eq!(declared_zero.declared_omitted_total, 0);
        assert_eq!(declared_zero.declaring_reports, 1, "an answered zero is not silence");
        assert_eq!(declared_zero.experts[0].omitted(), Some(0));
    }

    #[test]
    fn truncation_picks_up_an_indented_declaration() {
        let summary = TruncationSummary::from_counts(vec![("security".to_string(), 5)], 5);
        assert_eq!(summary.experts.len(), 1);
        assert_eq!(
            parse_declared_omitted("report:\n  findings_omitted: 3\n"),
            Some(3),
            "an indented key still counts"
        );
    }

    #[test]
    fn parse_declared_omitted_ignores_prose_and_other_keys() {
        assert_eq!(parse_declared_omitted(""), None);
        assert_eq!(parse_declared_omitted("findings:\n  - title: x"), None);
        assert_eq!(parse_declared_omitted("findings_omitted: none were withheld"), None);
        assert_eq!(
            parse_declared_omitted("findings_omitted_count: 4"),
            None,
            "a longer key is a different key"
        );
        assert_eq!(
            parse_declared_omitted("the model reported findings_omitted: 9 in prose"),
            None,
            "only a line-leading key counts"
        );
        assert_eq!(parse_declared_omitted("findings_omitted: 12"), Some(12));
        assert_eq!(parse_declared_omitted("findings_omitted: 12  # cap hit"), Some(12));
        assert_eq!(
            parse_declared_omitted("findings_omitted:\n  - 3"),
            None,
            "not an integer"
        );
    }

    #[test]
    fn truncation_summary_serializes_for_the_report_payload() {
        let summary = TruncationSummary::from_counts(vec![("security".to_string(), 5)], 5);
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["cap"], 5);
        assert_eq!(json["experts"][0]["expert"], "security");
        assert_eq!(json["experts"][0]["listed"], 5);
        assert!(
            json["experts"][0].get("declared_omitted").is_none(),
            "an unknown count is omitted, not reported as 0"
        );
        assert_eq!(json["declared_omitted_total"], 0);
        assert_eq!(
            json["declaring_reports"], 0,
            "the consumer must be able to tell silence from a declared zero"
        );
    }
}
