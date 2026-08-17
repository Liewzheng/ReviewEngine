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

/// Render an inclusive line range as `L` (single line) or `L-H`.
fn range_label(range: (u32, u32)) -> String {
    if range.0 == range.1 {
        format!("{}", range.0)
    } else {
        format!("{}-{}", range.0, range.1)
    }
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

    // Zero findings across every expert, or demonstrably insufficient hunk
    // coverage: never present the result as "healthy". The risk band is
    // replaced by an explicit "unverified" marker and a bilingual warning.
    let risk_label = if assessment.unverified {
        if assessment.coverage_insufficient {
            "unverified（审查覆盖不足 / insufficient coverage）".to_string()
        } else {
            "unverified（全零发现 / zero findings）".to_string()
        }
    } else {
        format!("{}", assessment.risk_level)
    };
    out.push_str(&format!(
        "**Overall Assessment**: Overall Score: **{}/100** (Risk Level: {})\n\n",
        assessment.score, risk_label,
    ));
    if assessment.unverified {
        if assessment.coverage_insufficient {
            out.push_str(
                "> ⚠️ **Unverified result**: demonstrated review coverage is below the \
                 threshold — most of the diff was not demonstrably examined, so the verdict \
                 is not trustworthy. / 审查覆盖不足：大部分改动未被可追溯地审查，结果不可信。\n\n",
            );
        } else {
            out.push_str(
                "> ⚠️ **Unverified result**: no expert reported any issue — a zero-finding \
                 outcome may indicate low coverage or a systemic miss, not a clean codebase. \
                 / 全零发现，结果未验证，可能为覆盖率不足或系统性漏报，请谨慎对待。\n\n",
            );
        }
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
    // Hunk-level coverage ledger: changed ranges vs. demonstrably-touched
    // ranges, plus the uncovered ranges (coverage debt). Rendered whenever the
    // consolidator was given a ledger (the full `run_experts` path).
    if let Some(coverage) = &consolidated.coverage {
        out.push_str(&format!(
            "**Hunk Coverage**: {}/{} changed lines demonstrably reviewed ({:.0}%)\n\n",
            coverage.covered_changed_lines,
            coverage.total_changed_lines,
            coverage.ratio * 100.0,
        ));
        if !coverage.debt.is_empty() {
            let debt: Vec<String> = coverage
                .debt
                .iter()
                .map(|u| format!("`{}:{}`", u.file, range_label(u.range)))
                .collect();
            out.push_str(&format!("**未覆盖区域 / uncovered**: {}\n\n", debt.join(", "),));
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

    // Adjudication transparency: high-severity findings the final
    // adjudication pass dropped as false positives, with the adjudicator's
    // reason — recorded, never silent.
    if !consolidated.adjudicated_removed.is_empty() {
        out.push_str("### 🧑‍⚖️ Adjudicated Away (false positives)\n\n");
        for dropped in &consolidated.adjudicated_removed {
            let f = &dropped.finding;
            let line = f.line.map_or(String::new(), |l| format!(":{}", l));
            out.push_str(&format!(
                "- ~~**{}** `{}`{} — {}~~\n  - **Adjudicator**: {}\n",
                f.severity,
                f.file,
                line,
                close_unclosed_code_fences(&f.title),
                close_unclosed_code_fences(&dropped.reason),
            ));
        }
        out.push('\n');
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
mod tests;
