use super::validation::*;
use super::*;
use crate::team::lead_consolidator::FileCoverage;
use std::collections::HashSet;

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
        parse_error: None,
        raw_dump_path: None,
    }
}

fn test_config() -> AppConfig {
    AppConfig {
        project: None,
        report: ReportConfig::default(),
        review_experts: HashMap::new(),
        commands: HashMap::new(),
        scoring: ScoringConfig::default(),
        llm: Vec::new(),
        max_team_size: None,
        max_concurrent_llm_calls: None,
        output_dir: String::new(),
        diff: DiffConfig::default(),
        rate_limit: RateLimitConfig::default(),
        languages: LanguagesConfig::default(),
    }
}

#[test]
fn test_build_consolidated_report_respects_min_confidence_drop() {
    let mut config = test_config();
    config.report.min_confidence = 9;
    config.report.drop_low_confidence = true;
    let reports = vec![make_report(
        "security",
        vec![
            make_finding(Severity::High, 5, "a.rs", Some(1), "low confidence finding"),
            make_finding(Severity::Medium, 10, "b.rs", Some(2), "confident finding"),
        ],
    )];
    let consolidated = build_consolidated_report(&reports, &config, &FileCoverage::full(2), None);
    assert_eq!(consolidated.low_confidence_removed, 1);
    assert_eq!(consolidated.findings.len(), 1);
    assert_eq!(consolidated.findings[0].title, "confident finding");
}

#[test]
fn test_build_consolidated_report_downgrades_by_default() {
    // Default config: min_confidence = 6, drop_low_confidence = false
    let config = test_config();
    let reports = vec![make_report(
        "security",
        vec![make_finding(Severity::High, 4, "a.rs", Some(1), "shaky finding")],
    )];
    let consolidated = build_consolidated_report(&reports, &config, &FileCoverage::full(1), None);
    assert_eq!(consolidated.low_confidence_removed, 0);
    assert_eq!(consolidated.findings.len(), 1);
    // Downgraded one severity step: High → Medium
    assert_eq!(consolidated.findings[0].severity, Severity::Medium);
}

#[test]
fn test_build_consolidated_report_detects_conflicts_and_scores() {
    let config = test_config();
    let mut f1 = make_finding(Severity::Medium, 8, "a.rs", Some(1), "Style");
    f1.recommendation = "Use tabs".to_string();
    f1.expert_name = "alice".to_string();
    let mut f2 = make_finding(Severity::Medium, 8, "a.rs", Some(1), "Style");
    f2.title = "Other take".to_string();
    f2.recommendation = "Use spaces".to_string();
    f2.expert_name = "bob".to_string();
    let reports = vec![make_report("alice", vec![f1]), make_report("bob", vec![f2])];
    let consolidated = build_consolidated_report(&reports, &config, &FileCoverage::full(1), None);
    assert!(!consolidated.conflicts.is_empty());
    assert!(consolidated.assessment.score <= 100);
    assert!(!consolidated.assessment.tl_dr.is_empty());
}

/// End-to-end wiring check: `run_experts` always returns a lead
/// consolidation summary, even with an empty expert team (no LLM calls).
#[tokio::test]
async fn test_run_experts_returns_consolidated_report() {
    let config = test_config();
    let mr_info = MRInfo::new(
        "test/project".to_string(),
        "Test review".to_string(),
        "feat/test".to_string(),
        "main".to_string(),
    );
    let (reports, _global_context, dropped_findings, consolidated) =
        run_experts(&[], &mr_info, "", &[], &config, None, "test-review-id", None)
            .await
            .expect("run_experts with empty team should succeed");
    assert!(reports.is_empty());
    assert!(dropped_findings.is_empty());
    // Empty team → perfect score, no conflicts, non-empty TL;DR.
    assert_eq!(consolidated.assessment.score, 100);
    assert!(consolidated.conflicts.is_empty());
    assert!(consolidated.findings.is_empty());
    assert!(!consolidated.assessment.tl_dr.is_empty());
}

// ─── feedback-driven filtering ───────────────

fn make_categorized_finding(file: &str, line: Option<u32>, title: &str, category: &str) -> Finding {
    let mut f = make_finding(Severity::High, 9, file, line, title);
    f.category = category.to_string();
    f
}

#[test]
fn test_filter_feedback_false_positives_removes_hits_keeps_misses() {
    let hit = make_categorized_finding("src/main.rs", Some(42), "SQL injection", "security");
    let miss = make_categorized_finding("src/lib.rs", Some(7), "style nit", "style");
    let false_positives: HashSet<String> = [hit.fingerprint()].into_iter().collect();
    let mut reports = vec![make_report("security", vec![hit.clone(), miss.clone()])];

    let dropped = filter_feedback_false_positives(&mut reports, &false_positives);

    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].finding.title, "SQL injection");
    assert_eq!(dropped[0].reason, "marked false positive by user feedback");
    assert_eq!(reports[0].findings.len(), 1);
    assert_eq!(reports[0].findings[0].title, "style nit");
}

#[test]
fn test_filter_feedback_false_positives_empty_set_keeps_all() {
    let mut reports = vec![make_report(
        "security",
        vec![make_categorized_finding(
            "src/main.rs",
            Some(42),
            "SQL injection",
            "security",
        )],
    )];
    let dropped = filter_feedback_false_positives(&mut reports, &HashSet::new());
    assert!(dropped.is_empty());
    assert_eq!(reports[0].findings.len(), 1);
}

#[test]
fn test_apply_feedback_filter_disabled_is_noop() {
    let mut reports = vec![make_report(
        "security",
        vec![make_categorized_finding(
            "src/main.rs",
            Some(42),
            "SQL injection",
            "security",
        )],
    )];
    let dropped = apply_feedback_filter(&mut reports, false);
    assert!(dropped.is_empty());
    assert_eq!(reports[0].findings.len(), 1);
}

/// End-to-end: a feedback JSON file on disk (written through
/// `FeedbackStore`) drives the filter — the false-positive-marked
/// finding is dropped, the useful-marked and unmarked ones are kept.
#[test]
fn test_feedback_filter_end_to_end_from_feedback_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("feedback.json");

    let false_positive = make_categorized_finding("src/main.rs", Some(42), "SQL injection", "security");
    let useful = make_categorized_finding("src/lib.rs", Some(7), "missing test", "quality");
    let unmarked = make_categorized_finding("src/api.rs", Some(3), "n+1 query", "performance");

    let record = |finding: &Finding, verdict: crate::feedback::Verdict| crate::feedback::FindingFeedback {
        finding_fingerprint: finding.fingerprint(),
        verdict,
        comment: None,
        category: Some(finding.category.clone()),
        created_at: chrono::Utc::now(),
    };
    let store = crate::feedback::FeedbackStore::with_path(Some(path.clone()));
    store
        .record(record(&false_positive, crate::feedback::Verdict::FalsePositive))
        .unwrap();
    store.record(record(&useful, crate::feedback::Verdict::Useful)).unwrap();
    drop(store);

    let false_positives = crate::feedback::load_false_positive_fingerprints_from(&path);
    let mut reports = vec![make_report("security", vec![false_positive, useful, unmarked])];
    let dropped = filter_feedback_false_positives(&mut reports, &false_positives);

    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].finding.title, "SQL injection");
    assert_eq!(dropped[0].reason, "marked false positive by user feedback");
    let kept_titles: Vec<&str> = reports[0].findings.iter().map(|f| f.title.as_str()).collect();
    assert_eq!(kept_titles, ["missing test", "n+1 query"]);
}

// ─── coverage ledger ────────────────────────

fn diff_hunk(new_start: u32, new_lines: u32) -> DiffHunk {
    DiffHunk {
        header: String::new(),
        old_start: 1,
        old_lines: 0,
        new_start,
        new_lines,
        lines: Vec::new(),
    }
}

#[test]
fn coverage_ledger_marks_single_line_finding_touched() {
    let diff_files = vec![("src/a.rs".to_string(), vec![diff_hunk(10, 11)])]; // changed 10..=20
    let finding = make_categorized_finding("src/a.rs", Some(15), "bug", "correctness");
    let reports = vec![make_report("security", vec![finding])];

    let ledger = build_coverage_ledger(&diff_files, &reports);
    assert_eq!(ledger.targets.len(), 1);
    let target = &ledger.targets[0];
    assert_eq!(target.touched_ranges, vec![(15, 15)]);
    assert_eq!(target.touched_by, vec!["security"]);
    assert_eq!(target.status, crate::coverage::CoverageStatus::Touched);
}

#[test]
fn coverage_ledger_clamps_reversed_line_end_to_start() {
    let diff_files = vec![("src/a.rs".to_string(), vec![diff_hunk(1, 5)])];
    let mut finding = make_categorized_finding("src/a.rs", Some(3), "range", "correctness");
    finding.line_end = Some(2); // end < start → clamped to start
    let reports = vec![make_report("quality", vec![finding])];

    let ledger = build_coverage_ledger(&diff_files, &reports);
    assert_eq!(ledger.targets[0].touched_ranges, vec![(3, 3)]);
}

#[test]
fn coverage_ledger_file_scoped_finding_marks_full_changed_range() {
    let diff_files = vec![("src/a.rs".to_string(), vec![diff_hunk(10, 11), diff_hunk(30, 5)])];
    // line: None → the expert is deemed aware of the whole file.
    let finding = make_categorized_finding("src/a.rs", None, "reviewed", "quality");
    let reports = vec![make_report("lead", vec![finding])];

    let ledger = build_coverage_ledger(&diff_files, &reports);
    let target = &ledger.targets[0];
    assert_eq!(target.changed_ranges, vec![(10, 20), (30, 34)]);
    assert_eq!(target.touched_ranges, vec![(10, 20), (30, 34)]);
}

#[test]
fn coverage_ledger_ignores_finding_for_file_not_in_diff() {
    let diff_files = vec![("src/a.rs".to_string(), vec![diff_hunk(1, 3)])];
    let finding = make_categorized_finding("src/other.rs", Some(1), "stray", "quality");
    let reports = vec![make_report("security", vec![finding])];

    let ledger = build_coverage_ledger(&diff_files, &reports);
    assert_eq!(ledger.targets.len(), 1);
    assert!(
        ledger.targets[0].touched_ranges.is_empty(),
        "unknown file must not be touched"
    );
}

#[test]
fn coverage_ledger_empty_hunks_produce_no_targets() {
    let diff_files = vec![
        ("src/a.rs".to_string(), vec![]),
        ("src/b.rs".to_string(), vec![diff_hunk(1, 0)]),
    ];
    let ledger = build_coverage_ledger(&diff_files, &[]);
    assert!(ledger.targets.is_empty(), "no changed ranges → no targets");
}

#[test]
fn coverage_ledger_merges_overlapping_touches_from_two_experts() {
    let diff_files = vec![("src/a.rs".to_string(), vec![diff_hunk(1, 20)])];
    let f1 = make_categorized_finding("src/a.rs", Some(5), "x", "quality");
    let f2 = make_categorized_finding("src/a.rs", Some(15), "y", "security");
    let reports = vec![make_report("q", vec![f1]), make_report("s", vec![f2])];

    let ledger = build_coverage_ledger(&diff_files, &reports);
    let target = &ledger.targets[0];
    assert_eq!(target.touched_ranges, vec![(5, 5), (15, 15)]);
    let mut by = target.touched_by.clone();
    by.sort();
    assert_eq!(by, vec!["q", "s"]);
}
