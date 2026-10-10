use super::*;
use crate::team::{ExpertMetrics, ExpertReport};

fn make_test_finding(severity: Severity, file: &str) -> Finding {
    Finding {
        file: file.to_string(),
        line: Some(42),
        line_end: None,
        severity,
        confidence: 8,
        category: "test".to_string(),
        title: "Test finding".to_string(),
        summary: "A test finding for unit testing".to_string(),
        evidence: "```rust\nlet x = 1;\n```".to_string(),
        impact: "May cause issues".to_string(),
        recommendation: "Fix it".to_string(),
        effort: Effort::Small,
        expert_name: "tester".to_string(),
        expert_role: "Tester".to_string(),
        agrees_with: vec![],
        references: vec![],
    }
}

#[test]
fn test_render_team_report_empty() {
    let report = render_team_report("Test Team", &[], &[], &[]);
    assert!(report.contains("Test Team"));
    assert!(report.contains("0 reviewers"));
}

#[test]
fn test_render_team_report_with_findings() {
    let findings = vec![make_test_finding(Severity::High, "src/main.rs")];
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings,
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1500,
        tokens_used: 500,
    }];
    let report = render_team_report("CodeReview Board", &reports, &metrics, &[]);
    assert!(report.contains("CodeReview Board"));
    assert!(report.contains("security"));
    assert!(report.contains("src/main.rs"));
    assert!(report.contains("Overall Score"));
}

#[test]
fn test_render_team_report_with_errors() {
    let report = render_team_report("Test", &[], &[], &["Expert lead failed".to_string()]);
    assert!(report.contains("Errors"));
    assert!(report.contains("Expert lead failed"));
}

#[test]
fn test_generate_tldr_no_findings() {
    let tl_dr = generate_tldr(&[], &RiskLevel::Low);
    // Zero findings must be flagged as unverified, never "all experts approve".
    assert!(tl_dr.contains("reported no issues"), "got: {tl_dr}");
    assert!(tl_dr.contains("treat with caution"), "got: {tl_dr}");
    assert!(!tl_dr.contains("All"), "must not claim approval, got: {tl_dr}");
    assert!(!tl_dr.contains("No issues found. All"), "got: {tl_dr}");
}

#[test]
fn test_render_team_report_with_custom_scoring() {
    let findings = vec![make_test_finding(Severity::Critical, "src/main.rs")];
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings,
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1500,
        tokens_used: 500,
    }];
    let custom_scoring = ScoringConfig {
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
    };
    let report =
        render_team_report_with_scoring("CodeReview Board", &reports, &metrics, &[], Some(&custom_scoring), None);
    assert!(report.contains("CodeReview Board"));
    // custom critical penalty 50 with confidence factor 0.96 → score 48
    assert!(report.contains("| security | 48 | 100% | 48 |"));
    assert!(report.contains("Risk Level: high")); // 48 <= high_max=50
}

#[test]
fn test_render_team_report_backward_compatible() {
    // The wrapper without scoring should produce the same result as with None
    let findings = vec![make_test_finding(Severity::High, "src/main.rs")];
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings,
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1500,
        tokens_used: 500,
    }];
    let report1 = render_team_report("Test", &reports, &metrics, &[]);
    let report2 = render_team_report_with_scoring("Test", &reports, &metrics, &[], None, None);
    assert_eq!(report1, report2);
}

// ── render_lead_summary ──

fn make_consolidated(score: u8, risk_level: RiskLevel, tl_dr: &str) -> ConsolidatedReport {
    ConsolidatedReport {
        findings: vec![],
        low_confidence_removed: 0,
        confidence_downgraded: 0,
        duplicates_merged: 0,
        conflicts: vec![],
        assessment: OverallAssessment {
            score,
            risk_level,
            lead_override: None,
            tl_dr: tl_dr.to_string(),
            unverified: false,
            coverage_insufficient: false,
        },
        consensus_reached: false,
        total_files: 0,
        reviewed_files: 0,
        unreviewed_files: vec![],
        coverage: None,
        parse_status: vec![],
        verification_stats: None,
        injected_context: Default::default(),
        adjudicated_removed: vec![],
    }
}

#[test]
fn test_render_lead_summary_without_conflicts() {
    let consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("## Lead Summary"));
    assert!(md.contains("Overall Score: **85/100**"));
    assert!(md.contains("Risk Level: low-medium"));
    assert!(md.contains("### TL;DR"));
    assert!(md.contains("1 high found by 3 reviewers."));
    assert!(!md.contains("⚖️ Reviewer Discussion"));
    // total_files == 0 → no coverage banner (backward compatible).
    assert!(!md.contains("files reviewed"));
}

#[test]
fn test_render_lead_summary_full_coverage_banner() {
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
    consolidated.total_files = 29;
    consolidated.reviewed_files = 29;
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("**File routing**: 29/29 files assigned to a successful expert task"));
    assert!(!md.contains("not assigned"));
}

#[test]
fn test_render_lead_summary_under_coverage_banner() {
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
    consolidated.total_files = 29;
    consolidated.reviewed_files = 27;
    consolidated.unreviewed_files = vec!["src/skip_a.rs".to_string(), "src/skip_b.rs".to_string()];
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("**File routing**: 27/29 files assigned to a successful expert task"));
    assert!(md.contains("src/skip_a.rs") && md.contains("src/skip_b.rs"));
}

/// Build a finding positioned at a conflict location, owned by an expert.
fn make_stance_finding(expert: &str, severity: Severity, file: &str, line: u32) -> Finding {
    let mut finding = make_test_finding(severity, file);
    finding.line = Some(line);
    finding.expert_name = expert.to_string();
    finding
}

#[test]
fn test_render_lead_summary_with_conflicts() {
    let mut consolidated = make_consolidated(70, RiskLevel::Medium, "2 reviewers disagree.");
    consolidated.findings = vec![
        make_stance_finding("security", Severity::Critical, "src/auth.rs", 42),
        make_stance_finding("performance", Severity::Low, "src/auth.rs", 42),
    ];
    consolidated
        .conflicts
        .push(crate::team::lead_consolidator::ExpertConflict {
            file: "src/auth.rs".to_string(),
            line: Some(42),
            issue: "Token comparison".to_string(),
            experts: vec!["security".to_string(), "performance".to_string()],
            resolutions: vec![
                "Use constant-time comparison".to_string(),
                "Cache the token hash".to_string(),
            ],
        });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("### ⚖️ Reviewer Discussion"));
    assert!(md.contains("#### `src/auth.rs:42` — Token comparison"));
    assert!(md.contains("- **security** (severity: Critical): Use constant-time comparison"));
    assert!(md.contains("- **performance** (severity: Low): Cache the token hash"));
    // The ruling adopts the highest-severity position (security / Critical).
    assert!(md.contains("**Lead resolution**: Adopt **security**'s position (highest severity: Critical)"));
}

#[test]
fn test_render_lead_summary_conflict_without_matching_findings() {
    // Conflicts whose findings are absent (e.g. filtered out) still render;
    // the ruling falls back to the first position and notes the missing severity.
    let mut consolidated = make_consolidated(70, RiskLevel::Medium, "2 reviewers disagree.");
    consolidated
        .conflicts
        .push(crate::team::lead_consolidator::ExpertConflict {
            file: "src/auth.rs".to_string(),
            line: Some(42),
            issue: "Token comparison".to_string(),
            experts: vec!["security".to_string(), "performance".to_string()],
            resolutions: vec![
                "Use constant-time comparison".to_string(),
                "Cache the token hash".to_string(),
            ],
        });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("### ⚖️ Reviewer Discussion"));
    assert!(md.contains("- **security**: Use constant-time comparison"));
    assert!(md.contains("- **performance**: Cache the token hash"));
    assert!(md.contains("**Lead resolution**: Adopt **security**'s position (no severity information available)"));
}

// ── unverified (zero-findings) rendering ───────────────────────

#[test]
fn test_render_lead_summary_unverified_not_healthy() {
    let mut consolidated = make_consolidated(100, RiskLevel::Healthy, "bilingual unverified note");
    consolidated.assessment.unverified = true;
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("unverified"), "risk label must say unverified, got: {md}");
    assert!(!md.contains("Risk Level: healthy"), "must not claim healthy, got: {md}");
    assert!(md.contains("zero findings"), "must carry the zero-findings warning");
}

#[test]
fn test_render_lead_summary_normal_keeps_risk_level() {
    let mut consolidated = make_consolidated(92, RiskLevel::Healthy, "1 high found by 3 reviewers.");
    consolidated.assessment.unverified = false;
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("Risk Level: healthy"), "non-unverified keeps its risk band");
    assert!(!md.contains("unverified"));
}

// ── hunk-level coverage ledger rendering ───────────────────────

#[test]
fn test_render_lead_summary_hunk_coverage_debt() {
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
    consolidated.coverage = Some(crate::coverage::CoverageSummary {
        total_changed_lines: 25,
        covered_changed_lines: 19,
        ratio: 19.0 / 25.0,
        debt: vec![crate::coverage::UncoveredRange {
            file: "c.c".to_string(),
            range: (50, 55),
        }],
        findings_truncation: Default::default(),
    });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("## Coverage detail"), "ledger section must render");
    assert!(md.contains("19/25 changed lines demonstrably reviewed (76%)"));
    assert!(md.contains("未覆盖区域 / uncovered"));
    assert!(md.contains("c.c:50-55"), "coverage debt must list the uncovered range");
    assert!(md.contains("(6 lines)"), "the uncovered line count is stated");
    assert!(!md.contains("more spans not listed"), "short lists need no remainder");
}

#[test]
fn test_render_lead_summary_caps_a_long_uncovered_list_and_says_how_many() {
    // A sparse run over a large diff: hundreds of spans. The report must name a
    // bounded preview and state the remainder — the RENG-79 shape, where the
    // old per-hunk rule printed nothing at all.
    let mut consolidated = make_consolidated(40, RiskLevel::High, "sparse");
    let debt: Vec<crate::coverage::UncoveredRange> = (0..40)
        .map(|i| crate::coverage::UncoveredRange {
            file: "a.c".to_string(),
            range: (i * 10 + 1, i * 10 + 5),
        })
        .collect();
    consolidated.coverage = Some(crate::coverage::CoverageSummary {
        total_changed_lines: 400,
        covered_changed_lines: 8,
        ratio: 0.02,
        debt,
        findings_truncation: Default::default(),
    });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("a.c:1-5"));
    assert!(
        md.contains("另有 16 段未列出 / 16 more spans not listed"),
        "the withheld spans must be counted, not dropped: {md}"
    );
    assert!(!md.contains("a.c:381-385"), "the preview stops at the cap");
}

// ── RENG-79: truncation transparency ───────────────────────────

#[test]
fn test_render_lead_summary_flags_a_capped_expert() {
    let mut consolidated = make_consolidated(60, RiskLevel::Medium, "under the cap");
    consolidated.coverage = Some(crate::coverage::CoverageSummary {
        findings_truncation: crate::coverage::TruncationSummary {
            cap: 5,
            experts: vec![crate::coverage::ExpertTruncation {
                expert: "security".to_string(),
                listed: 5,
                cap: 5,
                declared_omitted: Some(7),
            }],
            declaring_reports: 1,
            declared_omitted_total: 7,
        },
        ..Default::default()
    });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("清单可能不完整 / findings may be truncated"), "got: {md}");
    assert!(md.contains("report.max_findings_per_expert = 5"));
    assert!(md.contains("另有 7 条未列出"), "the dropped count must be stated: {md}");
    assert!(md.contains("`security`"));
    assert!(
        md.contains("1 位专家自报共 7 条未列出"),
        "the total is attributed to the experts who actually answered: {md}"
    );
}

#[test]
fn test_render_lead_summary_says_unknown_when_the_expert_did_not_declare() {
    let mut consolidated = make_consolidated(60, RiskLevel::Medium, "capped");
    consolidated.coverage = Some(crate::coverage::CoverageSummary {
        findings_truncation: crate::coverage::TruncationSummary {
            cap: 5,
            experts: vec![crate::coverage::ExpertTruncation {
                expert: "quality".to_string(),
                listed: 5,
                cap: 5,
                declared_omitted: None,
            }],
            declaring_reports: 0,
            declared_omitted_total: 0,
        },
        ..Default::default()
    });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("未列出的条数未知"), "silence is unknown, not zero: {md}");
    assert!(!md.contains("另有 0 条未列出"), "never invent a zero count: {md}");
    // A sum of 0 with no declaration behind it must not become a total line:
    // `declared_omitted_total` alone cannot tell "nobody answered" from "they
    // answered zero", and printing the former as a number is the exact false
    // reassurance this warning exists to remove.
    assert!(
        !md.contains("自报共 0 条未列出"),
        "silence must not be summarised as a zero: {md}"
    );
}

#[test]
fn test_render_lead_summary_is_silent_when_nothing_hit_the_cap() {
    let mut consolidated = make_consolidated(90, RiskLevel::Low, "clean");
    consolidated.coverage = Some(crate::coverage::CoverageSummary {
        total_changed_lines: 10,
        covered_changed_lines: 10,
        ratio: 1.0,
        debt: vec![],
        findings_truncation: Default::default(),
    });
    let md = render_lead_summary(&consolidated);
    assert!(
        !md.contains("findings may be truncated"),
        "no cap hit → no warning: {md}"
    );
}

#[test]
fn test_render_lead_summary_reports_an_expert_that_declared_no_omissions() {
    let mut consolidated = make_consolidated(80, RiskLevel::Low, "exactly at the cap");
    consolidated.coverage = Some(crate::coverage::CoverageSummary {
        findings_truncation: crate::coverage::TruncationSummary {
            cap: 5,
            experts: vec![crate::coverage::ExpertTruncation {
                expert: "docs".to_string(),
                listed: 5,
                cap: 5,
                declared_omitted: Some(0),
            }],
            declaring_reports: 1,
            declared_omitted_total: 0,
        },
        ..Default::default()
    });
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("专家自报无遗漏"), "got: {md}");
    assert!(!md.contains("另有 0 条未列出"));
    assert!(
        !md.contains("自报共 0 条未列出"),
        "a declared zero is not a total worth printing: {md}"
    );
}

#[test]
fn test_render_lead_summary_coverage_insufficient_risk_label() {
    let mut consolidated = make_consolidated(70, RiskLevel::Medium, "1 high found by 2 reviewers.");
    consolidated.assessment.unverified = true;
    consolidated.assessment.coverage_insufficient = true;
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("unverified（审查覆盖不足 / insufficient coverage）"));
    assert!(md.contains("below the threshold"), "must explain why unverified");
    assert!(
        !md.contains("no expert reported any issue"),
        "coverage-insufficient is not zero-findings"
    );
}

#[test]
fn test_render_lead_summary_no_ledger_skips_hunk_section() {
    // Backward-compatible path (consolidate() without a ledger): no hunk
    // coverage section, and the risk band is untouched.
    let mut consolidated = make_consolidated(92, RiskLevel::Healthy, "1 high found by 3 reviewers.");
    consolidated.coverage = None;
    let md = render_lead_summary(&consolidated);
    assert!(
        !md.contains("## Coverage detail"),
        "no ledger → no Coverage detail section"
    );
    assert!(md.contains("Risk Level: healthy"));
}

// ─── M18-W5: Injected Context ────────────────────────────────────────

#[test]
fn test_render_lead_summary_injected_context_surfaces_truncation() {
    // RENG-19 pain point: AGENTS.md at 13171 bytes / cap 4000 used to log a
    // WARN and silently ship the head 4000 bytes; the renderer must surface
    // the original/injected bytes + truncated flag + sha256 fingerprint.
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "ok");
    consolidated.injected_context = crate::team::lead_consolidator::InjectedContext {
        entries: vec![
            crate::context::agents_md::InjectedContextEntry {
                source: "AGENTS.md".to_string(),
                original_bytes: 13_171,
                injected_bytes: 4_000,
                truncated: true,
                sha256: "a".repeat(64),
            },
            crate::context::agents_md::InjectedContextEntry {
                source: "changed file src/lib.rs".to_string(),
                original_bytes: 8_192,
                injected_bytes: 8_192,
                truncated: false,
                sha256: "b".repeat(64),
            },
        ],
    };
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("## Injected Context"), "M18-W5 section must render");
    assert!(md.contains("`AGENTS.md`"));
    assert!(md.contains("4000 / 13171 bytes"));
    assert!(md.contains("⚠ truncated"), "truncated rows are visible");
    assert!(md.contains("`changed file src/lib.rs`"));
    // SHA-256 row prefix: 16 chars of the full fingerprint.
    assert!(md.contains(&"a".repeat(16)));
    // Make sure the un-truncated file does not falsely flag truncation.
    assert!(md.matches("ok").count() >= 1, "an un-truncated row must read 'ok'");
}

#[test]
fn test_render_lead_summary_injected_context_section_is_hidden_when_empty() {
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "ok");
    consolidated.injected_context = Default::default();
    let md = render_lead_summary(&consolidated);
    assert!(
        !md.contains("## Injected Context"),
        "no rows → no section (backward compat)"
    );
}

// ─── M18-W7: verification-pass stats line ────────────────────────────

#[test]
fn test_render_lead_summary_verification_stats_skipped_nonzero() {
    // M18-W7: "Verification pass: ran on N, skipped M, dropped K" must
    // surface whenever M31 plumbs verification_stats; the previous Reng
    // behaviour of "ran, dropped 0" implied every finding was a true
    // positive, which was false when parse failures silently passed through.
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "ok");
    consolidated.verification_stats = Some(crate::team::lead_consolidator::VerificationStats {
        checked: 11,
        skipped: 2,
        dropped: 0,
    });
    let md = render_lead_summary(&consolidated);
    assert!(
        md.contains("**Verification pass**: ran on 11, skipped 2, dropped 0"),
        "M18-W7 line must surface ran/skipped/dropped, got: {md}"
    );
    assert!(md.contains("解析失败 2 批"));
}

#[test]
fn test_render_lead_summary_verification_stats_absent_is_silent() {
    // No verification_stats means the verifier didn't run (e.g. an empty
    // expert team); the line is suppressed to keep backward-compat noise
    // out of the lead summary.
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "ok");
    consolidated.verification_stats = None;
    let md = render_lead_summary(&consolidated);
    assert!(!md.contains("**Verification pass**"), "no run → no line");
}

#[test]
fn test_render_lead_summary_verification_stats_zero_total_stays_silent() {
    // Empty / no-op verification (e.g. nothing to verify) renders nothing,
    // so an empty-checks state doesn't claim the verifier ran.
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "ok");
    consolidated.verification_stats = Some(Default::default());
    let md = render_lead_summary(&consolidated);
    assert!(!md.contains("**Verification pass**"));
}

#[test]
fn test_render_lead_summary_lists_adjudicated_removed() {
    let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
    consolidated.adjudicated_removed = vec![crate::team::verifier::DroppedFinding {
        finding: make_test_finding(Severity::Critical, "src/session.rs"),
        reason: "guard present at lines 1099-1134".to_string(),
    }];
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("Adjudicated Away"));
    assert!(md.contains("src/session.rs"));
    assert!(md.contains("guard present at lines 1099-1134"));
}

#[test]
fn test_render_lead_summary_no_adjudications_skips_section() {
    let consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
    let md = render_lead_summary(&consolidated);
    assert!(!md.contains("Adjudicated Away"));
}

/// RENG-73: an empty published list is not "zero findings" — every finding was
/// reported and then removed by adjudication. The lead summary must say which
/// of the two happened instead of implying the experts found nothing.
#[test]
fn test_render_lead_summary_all_adjudicated_away_label() {
    let mut consolidated = make_consolidated(
        45,
        RiskLevel::Medium,
        "every reported finding was removed by the final adjudication pass as a false positive",
    );
    consolidated.assessment.unverified = true;
    consolidated.adjudicated_removed = vec![crate::team::verifier::DroppedFinding {
        finding: make_test_finding(Severity::High, "src/session.rs"),
        reason: "guard present at lines 1099-1134".to_string(),
    }];
    let md = render_lead_summary(&consolidated);
    assert!(
        md.contains("all findings adjudicated away"),
        "risk label must name adjudication, got: {md}"
    );
    assert!(
        !md.contains("zero findings"),
        "adjudicated-away is not zero-findings, got: {md}"
    );
    assert!(
        md.contains("adjudication pass as a false positive"),
        "the warning must explain the empty list, got: {md}"
    );
    assert!(md.contains("Adjudicated Away"), "the drop list must still render");
    assert!(
        !md.contains("Risk Level: medium"),
        "an unverified result must not carry its risk band, got: {md}"
    );
}

/// The third branch is keyed on `adjudicated_removed` being non-empty AND the
/// result being unverified without coverage insufficiency — a non-unverified
/// report with drops keeps the ordinary risk band (see
/// `test_render_lead_summary_lists_adjudicated_removed`), and a coverage-driven
/// unverified result keeps its own label.
#[test]
fn test_render_lead_summary_coverage_insufficient_beats_adjudicated_away() {
    let mut consolidated = make_consolidated(45, RiskLevel::Medium, "bilingual unverified note");
    consolidated.assessment.unverified = true;
    consolidated.assessment.coverage_insufficient = true;
    consolidated.adjudicated_removed = vec![crate::team::verifier::DroppedFinding {
        finding: make_test_finding(Severity::High, "src/session.rs"),
        reason: "guard present".to_string(),
    }];
    let md = render_lead_summary(&consolidated);
    assert!(md.contains("insufficient coverage"), "got: {md}");
    assert!(!md.contains("all findings adjudicated away"), "got: {md}");
}

// ── render_expert_section (parse error + raw dump) ─────────────

#[test]
fn test_render_expert_section_plain_report_unchanged() {
    let findings = vec![make_test_finding(Severity::High, "src/main.rs")];
    let report = ExpertReport {
        expert_name: "security".to_string(),
        findings,
        markdown: "## Security Review\n\n...".to_string(),
        raw_llm_response: "raw".to_string(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    };
    assert_eq!(render_expert_section(&report), report.markdown);
}

#[test]
fn test_render_expert_section_parse_error_surfaces_instead_of_no_issues() {
    let report = ExpertReport {
        expert_name: "performance".to_string(),
        findings: vec![],
        markdown: "## Performance Review\n\nNo issues found.\n".to_string(),
        raw_llm_response: "review:\n  findings: [unclosed".to_string(),
        parse_error: Some("YAML parse failed".to_string()),
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    };
    let section = render_expert_section(&report);
    assert!(
        section.contains("输出解析失败"),
        "parse failure must be surfaced, got: {section}"
    );
    assert!(!section.contains("No issues found"), "must not silently say no issues");
    assert!(
        section.contains("review:\n  findings: [unclosed"),
        "raw excerpt must be inlined"
    );
}

#[test]
fn test_render_expert_section_raw_dump_path_referenced() {
    let report = ExpertReport {
        expert_name: "security".to_string(),
        findings: vec![make_test_finding(Severity::High, "src/main.rs")],
        markdown: "## Security Review\n\nfinding...".to_string(),
        raw_llm_response: "x".repeat(1200),
        parse_error: None,
        raw_dump_path: Some("/tmp/report.raw/security.1.response.txt".to_string()),
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    };
    let section = render_expert_section(&report);
    assert!(section.contains("Raw LLM response"), "raw section must be present");
    assert!(section.contains("… (truncated)"), "long raw must be truncated");
    assert!(section.contains("/tmp/report.raw/security.1.response.txt"));
    // Not the full 1200 chars inline.
    assert!(!section.contains(&"x".repeat(1200)));
}

// ─── RENG-92: the rendered total agrees with the consolidator's total ───

/// Extract the overall score from a rendered team report's assessment line
/// ("Overall Score: **N/100**").
fn parse_overall_score(md: &str) -> usize {
    let marker = "Overall Score: **";
    let start = md.find(marker).expect("report must carry an overall score") + marker.len();
    let end = md[start..].find('/').expect("score ends at '/100'") + start;
    md[start..end].parse().expect("score must be numeric")
}

/// The rendered markdown total for a weighted mapping must equal the lead
/// consolidator's total for the same reports and weights — the two sites that
/// score a review can never disagree.
#[test]
fn test_render_team_report_weighted_total_matches_consolidator() {
    use crate::team::lead_consolidator::{ConsolidatorConfig, ExpertWeights};

    let make_report = |name: &str, severity: Severity| ExpertReport {
        expert_name: name.to_string(),
        findings: vec![make_test_finding(severity, "src/main.rs")],
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    };
    let reports = vec![
        make_report("security", Severity::Critical),
        make_report("docs", Severity::Medium),
    ];
    let metrics = vec![
        ExpertMetrics {
            name: "security".to_string(),
            latency_ms: 1000,
            tokens_used: 100,
        },
        ExpertMetrics {
            name: "docs".to_string(),
            latency_ms: 800,
            tokens_used: 80,
        },
    ];
    let mut weights = ExpertWeights::new();
    weights.insert("security".to_string(), 90);
    weights.insert("docs".to_string(), 10);

    let rendered = render_team_report_with_scoring("Team", &reports, &metrics, &[], None, Some(&weights));
    let rendered_total = parse_overall_score(&rendered);

    let consolidated = ConsolidatorConfig {
        expert_weights: weights,
        ..Default::default()
    }
    .consolidate(&reports, None);
    assert_eq!(rendered_total, consolidated.assessment.score as usize);
    assert_eq!(rendered_total, 69, "(67*0.9 + 91*0.1)");

    // The weight column shows the configured weights, not the legacy equal weight.
    assert!(rendered.contains("| security | 67 | 90% |"), "got: {rendered}");
    assert!(rendered.contains("| docs | 91 | 10% |"), "got: {rendered}");
}

// ── RENG-113: credential redaction at the report render boundary ─────────

/// RENG-113: an Evidence block quoting a config line with `password = "…"`
/// must not carry the plaintext value through to the rendered report. The
/// marker preserves key + length + fingerprint so the reader can still see
/// *which* line had the hit.
#[test]
fn render_team_report_redacts_password_in_evidence_block() {
    let mut finding = make_test_finding(Severity::High, "config.toml");
    finding.evidence = r#"```toml
password = "hunter2_real_secret"
api_key = "sk-live-XXXXXXXXXXXXXXXXXXXX"
```"#
        .to_string();
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings: vec![finding],
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1000,
        tokens_used: 100,
    }];

    let rendered = render_team_report_with_scoring("Team", &reports, &metrics, &[], None, None);

    // The plaintext values are gone.
    assert!(
        !rendered.contains("hunter2_real_secret"),
        "Evidence must not carry the plaintext password: {rendered}"
    );
    assert!(
        !rendered.contains("sk-live-XXXXXXXXXXXXXXXXXXXX"),
        "Evidence must not carry the plaintext API key: {rendered}"
    );
    // The key names and the redaction metadata stay.
    assert!(
        rendered.contains("password = "),
        "the assignment structure is preserved: {rendered}"
    );
    assert!(
        rendered.contains("api_key = "),
        "the api_key assignment structure is preserved: {rendered}"
    );
    assert!(
        rendered.contains("kind=password"),
        "the redaction marker names the kind: {rendered}"
    );
    assert!(
        rendered.contains("kind=api_key"),
        "the redaction marker names the kind: {rendered}"
    );
}

/// RENG-113: the raw-LLM-response excerpt rendered alongside a parse
/// failure must also be redacted, since a parse failure is exactly when
/// the operator has to read the raw text — i.e. the path most likely to
/// carry a verbatim secret.
#[test]
fn render_expert_section_redacts_raw_llm_response_excerpt() {
    let report = ExpertReport {
        expert_name: "security".to_string(),
        findings: Vec::new(),
        markdown: String::new(),
        // Use a plain text excerpt (the raw path can also carry JSON
        // with escaped quotes, which the detector doesn't chase — that's
        // a deliberate trade-off, see [`crate::models::redact_text`]).
        raw_llm_response: "leaked value: password = \"hunter2_real_secret\"\n".to_string(),
        parse_error: Some("unparseable JSON".to_string()),
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    };

    let section = render_expert_section(&report);

    assert!(
        !section.contains("hunter2_real_secret"),
        "raw excerpt must not carry the plaintext password: {section}"
    );
    assert!(
        section.contains("kind=password"),
        "raw excerpt is redacted through the same path: {section}"
    );
}

/// RENG-113: a TL;DR that quotes a config line is redacted before it
/// reaches the report header.
#[test]
fn render_team_report_redacts_password_in_summary_field() {
    let mut finding = make_test_finding(Severity::High, "config.toml");
    finding.summary = r#"see config: password = "hunter2_real""#.to_string();
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings: vec![finding],
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1000,
        tokens_used: 100,
    }];

    let rendered = render_team_report_with_scoring("Team", &reports, &metrics, &[], None, None);

    assert!(
        !rendered.contains("hunter2_real"),
        "summary field must be redacted: {rendered}"
    );
}

/// RENG-113: benign text is left untouched — the redaction must not eat
/// useful prose (the security expert's own description of the issue).
///
/// Note: the `team_renderer` doesn't surface `Finding::summary` (it relies
/// on the pre-rendered `report.markdown` for prose). The assertion below
/// uses the fields the team renderer actually renders: title, evidence,
/// impact, recommendation.
#[test]
fn render_team_report_leaves_benign_text_unchanged() {
    let mut finding = make_test_finding(Severity::High, "src/auth.rs");
    finding.title = "Constant-time comparison missing".to_string();
    finding.summary = "Use subtle::ConstantTimeEq for token comparison.".to_string();
    finding.evidence = "```rust\nlet ok = a == b;\n```".to_string();
    finding.impact = "Tokens leak via timing.".to_string();
    finding.recommendation = "Switch to a constant-time compare.".to_string();
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings: vec![finding],
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1000,
        tokens_used: 100,
    }];

    let rendered = render_team_report_with_scoring("Team", &reports, &metrics, &[], None, None);

    assert!(rendered.contains("Constant-time comparison missing"));
    assert!(rendered.contains("let ok = a == b;"));
    assert!(rendered.contains("Tokens leak via timing."));
    assert!(rendered.contains("Switch to a constant-time compare."));
    // The pre-rendered expert markdown (render_expert_markdown) *does* carry
    // the summary — and a benign summary must survive the redact pass.
    let summary = crate::output::renderer::render_expert_markdown("security", &reports[0].findings);
    assert!(summary.contains("subtle::ConstantTimeEq"));
}

/// RENG-113 round 2: `Finding::title` reaches both the team-rendered
/// findings section and the per-expert pre-rendered markdown. The
/// reviewer's probe (`/tmp/reng113-probe/rust-probe`) showed a title like
/// `leaked: password = "hunter2_real_secret"` carrying the plaintext
/// through `render_expert_markdown` line 39 — the publish gate caught the
/// subsequent board (its `title` field becomes the inline note body), but
/// the local report file at 0600 was the unredacted exposure.
#[test]
fn render_team_report_redacts_finding_title() {
    let mut finding = make_test_finding(Severity::High, "src/auth.rs");
    finding.title = "leaked: password = \"hunter2_real_secret\"".to_string();
    let reports = vec![ExpertReport {
        expert_name: "security".to_string(),
        findings: vec![finding.clone()],
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
        llm_provider: None,
        llm_model: None,
        llm_fp: None,
    }];
    let metrics = vec![ExpertMetrics {
        name: "security".to_string(),
        latency_ms: 1000,
        tokens_used: 100,
    }];

    let rendered = render_team_report_with_scoring("Team", &reports, &metrics, &[], None, None);

    assert!(
        !rendered.contains("hunter2_real_secret"),
        "the team renderer must not carry the plaintext title: {rendered}"
    );
    assert!(
        rendered.contains("kind=password"),
        "the title redaction is named in the marker: {rendered}"
    );

    // Same expectation in the pre-rendered per-expert path that the team
    // renderer pulls from (render_expert_markdown → render_expert_section).
    let expert_md = crate::output::renderer::render_expert_markdown("security", &reports[0].findings);
    assert!(
        !expert_md.contains("hunter2_real_secret"),
        "the per-expert pre-rendered markdown must not carry the plaintext title either: {expert_md}"
    );

    // And the aggregated path (render_aggregated_markdown), which is the
    // second board-side source.
    let aggregated = crate::output::renderer::render_aggregated_markdown(&reports[0].findings);
    assert!(
        !aggregated.contains("hunter2_real_secret"),
        "the aggregated report must not carry the plaintext title either: {aggregated}"
    );
}

/// RENG-113 round 2: the dropped-findings appendix exposes the verifier's
/// `reason` (LLM-derived) and the finding's `title` (LLM-derived). Both
/// pass through `redact` so a verifier explaining "the password = '…'
/// claim is wrong, the real value is in env vars" doesn't smuggle the
/// plaintext through the appendix.
#[test]
fn render_dropped_findings_appendix_redacts_title_and_reason() {
    use crate::team::verifier::DroppedFinding;
    let mut finding = make_test_finding(Severity::Medium, "config.toml");
    finding.title = "leaked via password = \"hunter2_real_secret\"".to_string();
    let dropped = vec![DroppedFinding {
        finding,
        reason: "the credential is loaded from os.getenv(\"TOKEN\") at startup, not from config.toml".to_string(),
    }];
    let md = crate::output::renderer::render_dropped_findings_appendix(&dropped, false, 1);
    assert!(!md.contains("hunter2_real_secret"), "title plaintext is gone: {md}");
    assert!(md.contains("kind=password"), "title is masked: {md}");

    // The "list-only" path (publish / no run-summary) — same redaction.
    let md = crate::output::renderer::render_dropped_findings_appendix_with_state(&dropped, false, false, 1);
    assert!(
        !md.contains("hunter2_real_secret"),
        "title plaintext is gone in the list-only path: {md}"
    );

    // The "ran" path (with run-summary line at the end) — same redaction.
    let md = crate::output::renderer::render_dropped_findings_appendix_with_state(&dropped, true, true, 1);
    assert!(
        !md.contains("hunter2_real_secret"),
        "title plaintext is gone in the ran path: {md}"
    );
}
