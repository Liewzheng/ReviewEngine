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
        parse_error: None,
        raw_dump_path: None,
    }
}

fn make_hunk(new_start: u32, new_lines: u32) -> DiffHunk {
    DiffHunk {
        header: format!("@@ -1,1 +{new_start},{new_lines} @@"),
        old_start: 1,
        old_lines: 1,
        new_start,
        new_lines,
        lines: vec![],
    }
}

// ─── hunk-level coverage ledger integration ────────────────────

#[test]
fn test_coverage_insufficient_marks_unverified_even_with_findings() {
    let config = ConsolidatorConfig::default();
    // 20 changed lines, only 2 demonstrably touched → ratio 0.1 < 0.7.
    let diff_files = vec![("a.rs".to_string(), vec![make_hunk(10, 20)])];
    let mut ledger = crate::coverage::CoverageLedger::from_diff_files(&diff_files);
    ledger.mark_touched("a.rs", (10, 11), "security");
    let findings = vec![make_finding(Severity::High, 8, "a.rs", Some(10), "Real issue")];
    let reports = vec![make_report("security", findings)];
    let result = config.consolidate_with_coverage(&reports, None, &FileCoverage::full(1), Some(&ledger));
    assert!(result.assessment.coverage_insufficient);
    assert!(
        result.assessment.unverified,
        "coverage insufficiency must make result unverified"
    );
    assert!(result.assessment.tl_dr.contains("审查覆盖不足"));
    assert!(
        result.coverage.is_some(),
        "coverage summary must be stored on the report"
    );
}

#[test]
fn test_sufficient_coverage_with_findings_not_unverified() {
    let config = ConsolidatorConfig::default();
    let diff_files = vec![("a.rs".to_string(), vec![make_hunk(10, 10)])];
    let mut ledger = crate::coverage::CoverageLedger::from_diff_files(&diff_files);
    ledger.mark_touched("a.rs", (10, 19), "security"); // full coverage
    let findings = vec![make_finding(Severity::High, 8, "a.rs", Some(12), "Real issue")];
    let reports = vec![make_report("security", findings)];
    let result = config.consolidate_with_coverage(&reports, None, &FileCoverage::full(1), Some(&ledger));
    assert!(!result.assessment.coverage_insufficient);
    assert!(
        !result.assessment.unverified,
        "covered review with findings stays verified"
    );
}

#[test]
fn test_ledger_without_findings_yields_zero_coverage_and_unverified() {
    let config = ConsolidatorConfig::default();
    let diff_files = vec![("a.rs".to_string(), vec![make_hunk(10, 20)])];
    let ledger = crate::coverage::CoverageLedger::from_diff_files(&diff_files);
    let reports = vec![make_report("security", vec![])];
    let result = config.consolidate_with_coverage(&reports, None, &FileCoverage::full(1), Some(&ledger));
    assert!(
        result.assessment.unverified,
        "zero findings ⇒ zero demonstrated coverage ⇒ unverified"
    );
    assert!(result.assessment.coverage_insufficient);
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
            score_samples: 1,
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

#[test]
fn test_consolidate_full_coverage_no_cap() {
    let config = ConsolidatorConfig::default();
    let reports = vec![make_report("security", vec![])];
    let coverage = FileCoverage {
        total_files: 10,
        reviewed_files: 10,
        unreviewed_files: vec![],
    };
    let result = config.consolidate_with_coverage(&reports, Some(85), &coverage, None);
    assert_eq!(result.assessment.score, 85);
    assert!(result.unreviewed_files.is_empty());
    assert!(!result.assessment.tl_dr.contains("Coverage"));
}

#[test]
fn test_consolidate_coverage_shortfall_caps_score() {
    // Anti-cheat: 10 files, only 4 reviewed → the fake 85 must be capped.
    let config = ConsolidatorConfig::default();
    let reports = vec![make_report("security", vec![])];
    let coverage = FileCoverage {
        total_files: 10,
        reviewed_files: 4,
        unreviewed_files: vec!["f5.rs".into()],
    };
    let result = config.consolidate_with_coverage(&reports, Some(85), &coverage, None);
    // 85 × 4/10 = 34
    assert_eq!(result.assessment.score, 34);
    assert_eq!(result.unreviewed_files, vec!["f5.rs".to_string()]);
    assert!(result.consensus_reached == (34 >= 70));
    assert!(result.assessment.tl_dr.contains("4/10 files reviewed"));
}

#[test]
fn test_consolidate_backward_compatible_default_full_coverage() {
    // `consolidate` (no coverage) must behave exactly as before: no cap.
    let config = ConsolidatorConfig::default();
    let reports = vec![make_report("security", vec![])];
    let result = config.consolidate(&reports, Some(85));
    assert_eq!(result.assessment.score, 85);
    assert_eq!(result.total_files, 0);
    assert!(result.unreviewed_files.is_empty());
}

// ─── zero-findings unverified flag ─────────────────────────────

#[test]
fn test_zero_findings_marked_unverified_and_tldr_cautions() {
    let config = ConsolidatorConfig::default();
    let reports = vec![make_report("security", vec![])];
    let result = config.consolidate(&reports, None);
    // All-zero → the perfect score is flagged unverified, and the TL;DR
    // must not claim "All N experts approve".
    assert!(
        result.assessment.unverified,
        "all-zero result must be flagged unverified"
    );
    assert!(result.assessment.score == 100, "score stays 100 (backward compat)");
    let tl_dr = &result.assessment.tl_dr;
    assert!(tl_dr.contains("reported no issues"), "got: {tl_dr}");
    assert!(tl_dr.contains("treat with caution"), "got: {tl_dr}");
    assert!(!tl_dr.contains("approve"), "must not claim approval, got: {tl_dr}");
}

#[test]
fn test_non_zero_findings_not_unverified() {
    let config = ConsolidatorConfig::default();
    let findings = vec![make_finding(Severity::High, 8, "a.rs", Some(1), "Real issue")];
    let reports = vec![make_report("security", findings)];
    let result = config.consolidate(&reports, None);
    assert!(!result.assessment.unverified, "findings present ⇒ not unverified");
    assert!(result.assessment.score < 100);
    assert!(!result.assessment.tl_dr.contains("treat with caution"));
}
