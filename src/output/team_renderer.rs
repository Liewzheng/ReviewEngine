//! Team report renderer. Formats expert findings into readable markdown reports.
//!
//! @module review-engine: CodeReview Board platform
use crate::models::*;
use crate::output::markdown::{close_unclosed_code_fences, strip_markdown_fences};
use crate::team::lead_consolidator::ConsolidatedReport;

/// How many characters of a raw LLM response to inline in the report before
/// pointing at the full dump file. Keeps the report readable while making the
/// LLM input/output inspectable.
const RAW_RESPONSE_EXCERPT_CHARS: usize = 500;

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Render one expert's report section for the final markdown report, including
/// the parse-failure and raw-response annotations that the pre-rendered
/// `report.markdown` does not carry.
///
/// - Base section: `report.markdown` (pre-rendered per-expert section).
/// - Parse failure (`parse_error` set): the misleading "No issues found" body
///   is replaced by an explicit ⚠️ note, and a truncated raw excerpt is shown
///   so the failure is diagnosable even without a dump file.
/// - `--verbose` dump (`raw_dump_path` set): a truncated raw-response summary
///   plus the full dump file path.
pub fn render_expert_section(report: &crate::models::ExpertReport) -> String {
    let mut section = report.markdown.clone();
    let mut extras = String::new();

    if let Some(err) = report.parse_error.as_deref() {
        if report.findings.is_empty() {
            // Replace the silent "No issues found" body with the parse failure.
            section = format!(
                "## {} Review\n\n> ⚠️ **{} 输出解析失败** / Expert \"{}\" failed to parse its output: {}\n\n",
                capitalize(&report.expert_name),
                report.expert_name,
                report.expert_name,
                err,
            );
        } else {
            extras.push_str(&format!(
                "> ⚠️ **{} 输出解析失败（部分结果）** / parse failed: {}\n\n",
                report.expert_name, err,
            ));
        }
    }

    let show_raw = report.parse_error.is_some() || report.raw_dump_path.is_some();
    if show_raw {
        let excerpt: String = report
            .raw_llm_response
            .chars()
            .take(RAW_RESPONSE_EXCERPT_CHARS)
            .collect();
        let truncated = report.raw_llm_response.chars().count() > RAW_RESPONSE_EXCERPT_CHARS;
        extras.push_str("**Raw LLM response**（原始 LLM 输出）\n\n```text\n");
        extras.push_str(&excerpt);
        if truncated {
            extras.push_str("… (truncated)");
        }
        extras.push_str("\n```\n\n");
        if let Some(path) = report.raw_dump_path.as_deref() {
            extras.push_str(&format!("> 完整原始响应 / full raw response: `{path}`\n\n"));
        }
    }

    if !extras.is_empty() {
        if !section.ends_with('\n') {
            section.push('\n');
        }
        section.push('\n');
        section.push_str(&extras);
    }
    section
}

/// Render a full team report as markdown.
///
/// # Parameters
/// * `team_name` — Title shown in the report header.
/// * `reports` — Findings produced by each expert reviewer.
/// * `metrics` — Per-expert latency and token usage.
/// * `errors` — Non-fatal errors encountered during review.
/// * `scoring` — Optional scoring configuration for custom penalties and thresholds.
///
/// # Returns
/// A Markdown string containing the overall assessment, score table, findings grouped by severity, and any errors.
pub fn render_team_report_with_scoring(
    team_name: &str,
    reports: &[crate::team::ExpertReport],
    metrics: &[crate::team::ExpertMetrics],
    errors: &[String],
    scoring: Option<&ScoringConfig>,
) -> String {
    let num_reviewers = metrics.len();
    let total_duration_ms: u64 = metrics.iter().map(|m| m.latency_ms).sum();
    let total_tokens: u64 = metrics.iter().map(|m| m.tokens_used).sum();
    let avg_duration = if num_reviewers > 0 {
        total_duration_ms / num_reviewers as u64
    } else {
        0
    };

    // Compute overall score from findings
    let expert_findings: Vec<(&str, &[Finding], u8)> = reports
        .iter()
        .map(|r| {
            (
                r.expert_name.as_str(),
                r.findings.as_slice(),
                100u8 / num_reviewers.max(1) as u8,
            )
        })
        .collect();

    let (overall_score, risk_level) = match scoring {
        Some(s) => {
            crate::scoring::review::compute_overall_with_config(&expert_findings, &s.penalties, &s.risk_thresholds)
        }
        None => crate::scoring::review::compute_overall(&expert_findings),
    };
    let tl_dr = generate_tldr(reports, &risk_level);

    // Flatten all findings (needed for both Findings section and footer)
    let all_findings: Vec<&Finding> = reports.iter().flat_map(|r| r.findings.iter()).collect();

    let mut out = String::new();

    // ── Header ──────────────────────────────────────────────────────────────
    out.push_str(&format!(
        "## {} — {} reviewers · {}s\n\n",
        team_name,
        num_reviewers,
        avg_duration / 1000,
    ));

    // ── Overall Assessment ──────────────────────────────────────────────────
    out.push_str(&format!(
        "**Overall Assessment**: Overall Score: **{}/100** (Risk Level: {})\n\n",
        overall_score, risk_level,
    ));

    // ── TL;DR ───────────────────────────────────────────────────────────────
    out.push_str(&format!("### TL;DR\n{}\n\n", close_unclosed_code_fences(&tl_dr)));

    // ── Reviewer List ───────────────────────────────────────────────────────
    out.push_str("### Reviewers\n\n");
    out.push_str("| Expert | Role | Findings | Latency | Tokens |\n");
    out.push_str("|--------|------|----------|---------|--------|\n");
    for report in reports {
        let metric = metrics.iter().find(|m| m.name == report.expert_name);
        let latency = metric.map(|m| format!("{}ms", m.latency_ms)).unwrap_or_default();
        let tokens = metric.map(|m| m.tokens_used.to_string()).unwrap_or_default();
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            report.expert_name,
            report.findings.first().map(|f| f.expert_role.as_str()).unwrap_or(""),
            report.findings.len(),
            latency,
            tokens,
        ));
    }
    out.push('\n');

    // ── Expert Score Table ──────────────────────────────────────────────────
    out.push_str("### Scores\n\n");
    out.push_str("| Expert | Score | Weight | Contribution |\n");
    out.push_str("|--------|-------|--------|-------------|\n");
    for report in reports {
        let score = match scoring {
            Some(s) => crate::scoring::expert_score_with_config(&report.findings, &s.penalties),
            None => crate::scoring::expert_score(&report.findings),
        };
        let weight = 100u8 / num_reviewers.max(1) as u8;
        let contribution = (score as f64 * weight as f64 / 100.0).round() as u8;
        out.push_str(&format!(
            "| {} | {} | {}% | {} |\n",
            report.expert_name, score, weight, contribution,
        ));
    }
    out.push('\n');

    // ── Findings grouped by severity ────────────────────────────────────────
    if !all_findings.is_empty() {
        out.push_str("### Findings\n\n");

        for severity in [
            Severity::Critical,
            Severity::High,
            Severity::Medium,
            Severity::Low,
            Severity::Note,
        ] {
            let severity_findings: Vec<&&Finding> = all_findings.iter().filter(|f| f.severity == severity).collect();

            if severity_findings.is_empty() {
                continue;
            }

            out.push_str(&format!("#### {:?}\n\n", severity));

            for f in severity_findings {
                out.push_str(&format!("**{}** — Confidence {}/10\n", f.title, f.confidence,));
                out.push_str(&format!(
                    "> [{}] {} `{}:{}`\n\n",
                    f.expert_name,
                    f.expert_role,
                    f.file,
                    f.line.unwrap_or(0),
                ));
                if !f.evidence.is_empty() {
                    let evidence = strip_markdown_fences(&f.evidence);
                    let evidence = close_unclosed_code_fences(&evidence);
                    out.push_str(&format!("**Evidence**:\n```\n{}\n```\n\n", evidence));
                }
                if !f.impact.is_empty() {
                    out.push_str(&format!("**Impact**: {}\n\n", close_unclosed_code_fences(&f.impact)));
                }
                if !f.recommendation.is_empty() {
                    out.push_str(&format!(
                        "**Recommendation**: {}\n\n",
                        close_unclosed_code_fences(&f.recommendation)
                    ));
                }
                out.push_str(&format!("Effort: {:?} | Severity: {:?}\n\n", f.effort, f.severity));
            }
        }
    }

    // ── Errors section ──────────────────────────────────────────────────────
    if !errors.is_empty() {
        out.push_str("### Errors\n\n");
        for err in errors {
            out.push_str(&format!("- {}\n", err));
        }
        out.push('\n');
    }

    // ── Footer ──────────────────────────────────────────────────────────────
    out.push_str(&format!(
        "---\n*{} · {} findings · {} errors · {} total tokens*\n",
        team_name,
        all_findings.len(),
        errors.len(),
        total_tokens,
    ));

    out
}

/// Backward-compatible wrapper that uses default scoring configuration.
pub fn render_team_report(
    team_name: &str,
    reports: &[crate::team::ExpertReport],
    metrics: &[crate::team::ExpertMetrics],
    errors: &[String],
) -> String {
    render_team_report_with_scoring(team_name, reports, metrics, errors, None)
}

/// Render the lead consolidation summary as a Markdown section.
///
/// Uses the same Overall Assessment / TL;DR formats as the team report.
/// When expert conflicts were detected, they are presented as a
/// "⚖️ Reviewer Discussion" section: each conflict lists the location,
/// the issue, every expert's position (with the severity they assigned,
/// when known), and a suggested lead resolution that adopts the position
/// raised at the highest severity.
/// Rendered after the per-expert reports and before the "Dropped by
/// verification" appendix in both CLI Markdown output and MR comments.
pub fn render_lead_summary(consolidated: &ConsolidatedReport) -> String {
    let assessment = &consolidated.assessment;
    let mut out = String::from("## Lead Summary\n\n");

    // Zero findings across every expert: never present a perfect score as
    // "healthy". The risk band is replaced by an explicit "unverified" marker
    // and a bilingual warning, so an empty result cannot read as a clean bill.
    let risk_label = if assessment.unverified {
        "unverified（全零发现 / zero findings）".to_string()
    } else {
        format!("{}", assessment.risk_level)
    };
    out.push_str(&format!(
        "**Overall Assessment**: Overall Score: **{}/100** (Risk Level: {})\n\n",
        assessment.score, risk_label,
    ));
    if assessment.unverified {
        out.push_str(
            "> ⚠️ **Unverified result**: no expert reported any issue — a zero-finding \
             outcome may indicate low coverage or a systemic miss, not a clean codebase. \
             / 全零发现，结果未验证，可能为覆盖率不足或系统性漏报，请谨慎对待。\n\n",
        );
    }
    // Coverage banner: honest about how much of the diff was actually
    // reviewed. Under-coverage is never hidden — it also caps the score.
    if consolidated.total_files > 0 {
        if consolidated.unreviewed_files.is_empty() {
            out.push_str(&format!(
                "**Coverage**: {} of {} files reviewed\n\n",
                consolidated.reviewed_files, consolidated.total_files,
            ));
        } else {
            out.push_str(&format!(
                "**Coverage**: {} of {} files reviewed; **{} files not covered by any expert**: {}\n\n",
                consolidated.reviewed_files,
                consolidated.total_files,
                consolidated.unreviewed_files.len(),
                consolidated.unreviewed_files.join(", "),
            ));
        }
    }
    out.push_str(&format!(
        "### TL;DR\n{}\n\n",
        close_unclosed_code_fences(&assessment.tl_dr)
    ));

    if !consolidated.conflicts.is_empty() {
        out.push_str("### ⚖️ Reviewer Discussion\n\n");
        for conflict in &consolidated.conflicts {
            let line = conflict.line.map_or(String::new(), |l| format!(":{}", l));
            out.push_str(&format!(
                "#### `{file}{line}` — {issue}\n\n",
                file = conflict.file,
                line = line,
                issue = conflict.issue,
            ));

            // Look up the severity each expert assigned to this location from
            // the consolidated findings, so the discussion shows how strongly
            // each side flagged the issue.
            let severity_of = |expert: &str| {
                consolidated
                    .findings
                    .iter()
                    .find(|f| f.expert_name == expert && f.file == conflict.file && f.line == conflict.line)
                    .map(|f| &f.severity)
            };

            for (expert, resolution) in conflict.experts.iter().zip(conflict.resolutions.iter()) {
                match severity_of(expert) {
                    Some(severity) => out.push_str(&format!(
                        "- **{}** (severity: {:?}): {}\n",
                        expert,
                        severity,
                        close_unclosed_code_fences(resolution)
                    )),
                    None => out.push_str(&format!(
                        "- **{}**: {}\n",
                        expert,
                        close_unclosed_code_fences(resolution)
                    )),
                }
            }

            // Suggested ruling: adopt the position raised at the highest severity.
            if !conflict.experts.is_empty() {
                let mut winner = 0usize;
                let mut winner_rank = 0u8;
                for (i, expert) in conflict.experts.iter().enumerate() {
                    let rank = severity_of(expert).map(severity_rank).unwrap_or(0);
                    if rank > winner_rank {
                        winner = i;
                        winner_rank = rank;
                    }
                }
                let basis = match severity_of(&conflict.experts[winner]) {
                    Some(severity) => format!("highest severity: {:?}", severity),
                    None => "no severity information available".to_string(),
                };
                let resolution = conflict.resolutions.get(winner).map_or("", String::as_str);
                out.push_str(&format!(
                    "\n**Lead resolution**: Adopt **{}**'s position ({}): {}\n\n",
                    conflict.experts[winner],
                    basis,
                    close_unclosed_code_fences(resolution),
                ));
            }
        }
    }

    out
}

/// Rank a severity for comparing conflicting positions (higher = more severe).
fn severity_rank(severity: &Severity) -> u8 {
    match severity {
        Severity::Critical => 4,
        Severity::High => 3,
        Severity::Medium => 2,
        Severity::Low => 1,
        Severity::Note => 0,
    }
}

/// Generate a concise TL;DR summary from expert reports.
fn generate_tldr(reports: &[crate::team::ExpertReport], risk: &RiskLevel) -> String {
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
    let total_medium: usize = reports
        .iter()
        .flat_map(|r| r.findings.iter())
        .filter(|f| f.severity == Severity::Medium)
        .count();

    let expert_count = reports.len();
    let total_findings: usize = reports.iter().map(|r| r.findings.len()).sum();

    if total_findings == 0 {
        return format!(
            "{} 位专家均未发现问题，但全零发现可能意味着审查覆盖率不足或系统性漏报，请谨慎对待（结果标记为“未验证/不可信”）。\n\n\
             {} experts reported no issues — this may indicate low coverage or a systemic issue; treat with caution (result marked unverified).",
            expert_count, expert_count,
        );
    }

    let mut parts = Vec::new();
    if total_critical > 0 {
        parts.push(format!("{} critical issues", total_critical));
    }
    if total_high > 0 {
        parts.push(format!("{} high-severity issues", total_high));
    }
    if total_medium > 0 {
        parts.push(format!("{} medium-severity issues", total_medium));
    }

    let summary = if parts.is_empty() {
        format!("{} minor issues found", total_findings)
    } else {
        parts.join(", ")
    };

    format!(
        "**Risk Level**: {:?}. {} found across {} reviewers. Estimated fix effort varies by severity.",
        risk, summary, expert_count,
    )
}

#[cfg(test)]
mod tests {
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
            risk_thresholds: RiskThresholdConfig {
                critical_max: 30,
                high_max: 50,
                medium_max: 70,
                low_max: 90,
                healthy_min: 95,
            },
        };
        let report =
            render_team_report_with_scoring("CodeReview Board", &reports, &metrics, &[], Some(&custom_scoring));
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
        }];
        let metrics = vec![ExpertMetrics {
            name: "security".to_string(),
            latency_ms: 1500,
            tokens_used: 500,
        }];
        let report1 = render_team_report("Test", &reports, &metrics, &[]);
        let report2 = render_team_report_with_scoring("Test", &reports, &metrics, &[], None);
        assert_eq!(report1, report2);
    }

    // ── render_lead_summary ──

    fn make_consolidated(score: u8, risk_level: RiskLevel, tl_dr: &str) -> ConsolidatedReport {
        ConsolidatedReport {
            findings: vec![],
            low_confidence_removed: 0,
            duplicates_merged: 0,
            conflicts: vec![],
            assessment: OverallAssessment {
                score,
                risk_level,
                lead_override: None,
                tl_dr: tl_dr.to_string(),
                unverified: false,
            },
            consensus_reached: false,
            total_files: 0,
            reviewed_files: 0,
            unreviewed_files: vec![],
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
        assert!(md.contains("**Coverage**: 29 of 29 files reviewed"));
        assert!(!md.contains("not covered"));
    }

    #[test]
    fn test_render_lead_summary_under_coverage_banner() {
        let mut consolidated = make_consolidated(85, RiskLevel::LowMedium, "1 high found by 3 reviewers.");
        consolidated.total_files = 29;
        consolidated.reviewed_files = 27;
        consolidated.unreviewed_files = vec!["src/skip_a.rs".to_string(), "src/skip_b.rs".to_string()];
        let md = render_lead_summary(&consolidated);
        assert!(md.contains("**Coverage**: 27 of 29 files reviewed"));
        assert!(md.contains("**2 files not covered by any expert**: src/skip_a.rs, src/skip_b.rs"));
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
        };
        let section = render_expert_section(&report);
        assert!(section.contains("Raw LLM response"), "raw section must be present");
        assert!(section.contains("… (truncated)"), "long raw must be truncated");
        assert!(section.contains("/tmp/report.raw/security.1.response.txt"));
        // Not the full 1200 chars inline.
        assert!(!section.contains(&"x".repeat(1200)));
    }
}
