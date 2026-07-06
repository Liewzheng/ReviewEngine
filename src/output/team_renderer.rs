//! Team report renderer. Formats expert findings into readable markdown reports.
//!
//! @module review-engine: CodeReview Board platform
use crate::models::*;
use crate::output::markdown::{close_unclosed_code_fences, strip_markdown_fences};

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
        return format!("No issues found. All {} experts approve.", expert_count);
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
        assert!(tl_dr.contains("No issues found"));
    }

    #[test]
    fn test_render_team_report_with_custom_scoring() {
        let findings = vec![make_test_finding(Severity::Critical, "src/main.rs")];
        let reports = vec![ExpertReport {
            expert_name: "security".to_string(),
            findings,
            markdown: String::new(),
            raw_llm_response: String::new(),
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
            },
        };
        let report =
            render_team_report_with_scoring("CodeReview Board", &reports, &metrics, &[], Some(&custom_scoring));
        assert!(report.contains("CodeReview Board"));
        assert!(report.contains("50")); // custom critical penalty = 50
        assert!(report.contains("Risk Level: high")); // 50 <= high_max=50
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
}
