use super::scoring::repo_risk_level;
use super::types::*;
use crate::output::markdown::{close_unclosed_code_fences, strip_markdown_fences};
use anyhow::Result;

/// Render an expert-score detail line as markdown.
pub(crate) fn render_detail(d: &ScoreItemDetail) -> String {
    let mut buf = String::new();

    if d.message.trim().is_empty() {
        return buf;
    }
    buf.push_str(&format!("\n#### {} — {}\n", d.severity.to_uppercase(), d.message));

    if let Some(ref file) = d.file {
        buf.push_str(&format!("**File**: `{file}`\n"));
    }
    if let Some(ref evidence) = d.evidence {
        let evidence = strip_markdown_fences(evidence);
        if !evidence.is_empty() {
            let evidence = close_unclosed_code_fences(&evidence);
            buf.push_str(&format!("**Evidence**:\n```\n{evidence}\n```\n"));
        }
    }
    if let Some(ref impact) = d.impact {
        if !impact.is_empty() {
            buf.push_str(&format!("**Impact**: {impact}\n"));
        }
    }
    if let Some(ref rec) = d.recommendation {
        if !rec.is_empty() {
            buf.push_str(&format!("**Recommendation**: {rec}\n"));
        }
    }
    if let Some(ref effort) = d.effort {
        if !effort.is_empty() {
            buf.push_str(&format!("**Effort**: {effort}\n"));
        }
    }
    buf
}

/// Render a repo-review output in the requested format.
///
/// `verification_enabled` tells the Markdown renderer whether the finding
/// verification pass ran, so the "Dropped by verification" appendix can show
/// a run summary even when nothing was dropped (mirrors the review
/// pipeline's `format_output`).
pub fn render_repo_review_output(
    output: &RepoReviewOutput,
    format: &str,
    verification_enabled: bool,
) -> Result<String> {
    Ok(match format {
        "json" => serde_json::to_string_pretty(output)?,
        _ => {
            let mut md = String::new();

            // ── Header ──
            md.push_str("# Repository Health Report\n\n");

            // ── Provenance (compact, directly under the title) ──
            // Everything a consumer needs to trace this report back to the
            // exact snapshot that produced it.
            let m = &output.metadata;
            md.push_str("## Provenance\n");
            match m.head_sha.as_deref() {
                Some(sha) => md.push_str(&format!("- **Git HEAD**: `{sha}`\n")),
                None => md.push_str("- **Git HEAD**: (not a git repository)\n"),
            }
            md.push_str(&format!("- **Tree Hash**: `{}`\n", m.tree_hash));
            md.push_str(&format!("- **Reviewed At**: {}\n", m.reviewed_at));
            md.push_str(&format!("- **Model**: {}\n", m.model));
            md.push_str(&format!("- **Score Samples**: {}\n", m.score_samples));
            md.push_str(&format!("- **Scan Source**: {}\n", m.scan_source));
            md.push_str(
                "\n> Scores are a heuristic single-run / sampled assessment of this snapshot; \
                 compare across runs only against the same Git HEAD SHA and tree hash.\n\n",
            );

            // ── Overview (bullet list, no emoji) ──
            md.push_str("## Overview\n");
            md.push_str(&format!(
                "- **Health Score**: {}/100 ({})\n",
                output.overview.health_score, output.overview.risk_level
            ));
            md.push_str(&format!("- **Experts**: {}\n", output.overview.total_experts));
            md.push_str(&format!("- **Files**: {}\n", output.overview.total_files));
            md.push_str(&format!("- **LOC**: {}\n", output.overview.total_loc));
            let lang_str = output.overview.languages.join(", ");
            md.push_str(&format!("- **Languages**: {}\n\n", lang_str));

            // Score breakdown table
            md.push_str("### Score Breakdown\n");
            md.push_str("| Expert | Score | Weight | Contribution | Risk |\n");
            md.push_str("|--------|-------|--------|-------------|------|\n");
            let mut total_weighted = 0.0_f64;
            for row in &output.overview.score_breakdown {
                total_weighted += row.weighted_contrib;
                // A fallback row must not read as a genuine assessment.
                let fb = if output.expert_scores.iter().any(|s| s.name == row.area && s.fallback) {
                    " ⚠"
                } else {
                    ""
                };
                md.push_str(&format!(
                    "| {}{} | {}/100 | {}% | {:.1} | {} |\n",
                    row.area, fb, row.score, row.weight, row.weighted_contrib, row.risk_label
                ));
            }
            let total_risk = repo_risk_level(output.overview.health_score);
            md.push_str(&format!(
                "| **Total** | **{}/100** | **100%** | **{:.1}** | {} |\n\n",
                output.overview.health_score, total_weighted, total_risk
            ));

            if let Some(ref summary) = output.overview.lead_summary {
                md.push_str(&format!("> {}\n\n", summary));
            }

            md.push_str("---\n\n");

            // ── Detailed findings per expert ──
            md.push_str("## Detailed Findings\n");
            for s in &output.expert_scores {
                // Zero-finding experts still render their header + summary:
                // skipping them hid fallback scores (which carry no details)
                // and clean bills of health alike.
                let fb_marker = if s.fallback { " ⚠ fallback" } else { "" };
                md.push_str(&format!(
                    "\n### {} ({}/100){} — {} findings\n",
                    s.name,
                    s.score,
                    fb_marker,
                    s.details.len()
                ));
                if s.fallback {
                    md.push_str(
                        "> ⚠ **Fallback** — this score is a placeholder, not a genuine assessment; \
                         the summary below records why.\n\n",
                    );
                }
                md.push_str(&format!("**Summary**: {}\n\n", s.summary));
                for d in &s.details {
                    md.push_str(&render_detail(d));
                }
            }

            // ── Risk categories ──
            if !output.risk_categories.is_empty() {
                md.push_str("---\n\n## Risk Map\n");
                md.push_str("| Risk Level | Area | Score | Issues |\n");
                md.push_str("|-----------|------|-------|--------|\n");
                for rc in &output.risk_categories {
                    md.push_str(&format!(
                        "| {} | {} | {}/100 | {} |\n",
                        rc.risk_level, rc.area, rc.score, rc.finding_count
                    ));
                }
                md.push('\n');
            }

            // ── Action items ──
            if !output.action_items.is_empty() {
                md.push_str("---\n\n## Action Items\n");
                md.push_str("| # | Area | Severity | Issue | Recommendation | Effort |\n");
                md.push_str("|---|------|----------|-------|---------------|--------|\n");
                for (i, item) in output.action_items.iter().enumerate() {
                    let eff = item.effort.as_deref().unwrap_or("—");
                    md.push_str(&format!(
                        "| {} | {} | {} | {} | {} | {} |\n",
                        i + 1,
                        item.area,
                        item.severity,
                        item.message,
                        item.recommendation,
                        eff,
                    ));
                }
                md.push('\n');
            }

            // ── Conclusion ──
            md.push_str("---\n\n## Conclusion\n");
            md.push_str(&format!(
                "**Aggregated Score**: {}/100 (**{}**)\n\n",
                output.conclusion.aggregated_score, output.conclusion.risk_level
            ));
            md.push_str("**Top Risks**:\n");
            if output.conclusion.top_risks.is_empty() {
                md.push_str("None\n");
            } else {
                for (i, (area, score)) in output.conclusion.top_risks.iter().enumerate() {
                    md.push_str(&format!("{}. **{}** ({}/100)\n", i + 1, area, score));
                }
            }
            md.push('\n');
            md.push_str(&format!("**Recommendation**: {}\n", output.conclusion.recommendation));

            // ── Verification appendix ──
            // checked = surviving code_quality findings + dropped ones, the
            // same "kept + dropped" accounting the review pipeline uses. The
            // explicit ran-state keeps the wording honest: "skipped" when the
            // pass was enabled but had no code_quality findings to verify,
            // "ran" only when `verify_findings` actually executed.
            let checked = output
                .expert_scores
                .iter()
                .filter(|s| s.name == "code_quality")
                .map(|s| s.details.len())
                .sum::<usize>()
                + output.dropped_findings.len();
            let appendix = crate::output::renderer::render_dropped_findings_appendix_with_state(
                &output.dropped_findings,
                verification_enabled,
                output.verification_ran,
                checked,
            );
            if !appendix.is_empty() {
                md.push_str("\n---\n\n");
                md.push_str(&appendix);
            }

            md.push_str("\n---\n*Report generated by Review Engine*\n");
            md = close_unclosed_code_fences(&md);
            md
        }
    })
}
