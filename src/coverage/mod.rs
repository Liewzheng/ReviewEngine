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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoverageStatus {
    Pending,
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
    pub status: CoverageStatus,
    /// Experts whose findings touched this file.
    pub touched_by: Vec<String>,
}

/// A changed hunk range with no demonstrated touch — the "coverage debt" that
/// future iterations can feed back to experts for a re-read pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UncoveredRange {
    pub file: String,
    /// 1-based inclusive range in the new file.
    pub range: (u32, u32),
}

/// Aggregate coverage across the whole diff.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub total_changed_lines: usize,
    pub covered_changed_lines: usize,
    /// `covered / total`; 1.0 when there are no changed lines (nothing to cover).
    pub ratio: f64,
    /// Changed ranges no finding referenced (coverage debt).
    pub debt: Vec<UncoveredRange>,
}

impl CoverageSummary {
    /// Whether demonstrated coverage meets the threshold.
    pub fn is_sufficient(&self) -> bool {
        self.ratio >= COVERAGE_THRESHOLD
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
    /// plus the per-hunk debt (uncovered ranges).
    pub fn summary(&self) -> CoverageSummary {
        let mut total = 0usize;
        let mut covered = 0usize;
        let mut debt = Vec::new();
        for target in &self.targets {
            for &(a, b) in &target.changed_ranges {
                let len = (b.saturating_sub(a) + 1) as usize;
                total += len;
                let hit = target
                    .touched_ranges
                    .iter()
                    .map(|&(s, e)| overlap_len(s, e, a, b))
                    .sum::<usize>();
                covered += hit;
                if hit == 0 {
                    debt.push(UncoveredRange {
                        file: target.file.clone(),
                        range: (a, b),
                    });
                }
            }
        }
        let ratio = if total == 0 { 1.0 } else { covered as f64 / total as f64 };
        CoverageSummary {
            total_changed_lines: total,
            covered_changed_lines: covered,
            ratio,
            debt,
        }
    }
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
}
