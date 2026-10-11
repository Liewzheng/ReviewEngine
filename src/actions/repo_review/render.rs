use super::scoring::repo_risk_level;
use super::types::*;
use crate::output::markdown::{close_unclosed_code_fences, strip_markdown_fences};
use anyhow::Result;

/// RENG-113: redact suspected credential values in LLM-derived prose before
/// it lands in the report (the security expert's static `Security` finding
/// is exactly the path that quotes a config line with a real password).
fn redact(text: &str) -> String {
    crate::models::redact_text(text)
}

/// Render an expert-score detail line as markdown.
pub(crate) fn render_detail(d: &ScoreItemDetail) -> String {
    let mut buf = String::new();

    if d.message.trim().is_empty() {
        return buf;
    }
    buf.push_str(&format!(
        "\n#### {} — {}\n",
        d.severity.to_uppercase(),
        redact(&d.message)
    ));

    if let Some(ref file) = d.file {
        buf.push_str(&format!("**File**: `{file}`\n"));
    }
    if let Some(ref evidence) = d.evidence {
        let evidence = strip_markdown_fences(evidence);
        if !evidence.is_empty() {
            let evidence = close_unclosed_code_fences(&redact(&evidence));
            buf.push_str(&format!("**Evidence**:\n```\n{evidence}\n```\n"));
        }
    }
    if let Some(ref impact) = d.impact {
        if !impact.is_empty() {
            buf.push_str(&format!("**Impact**: {}\n", redact(impact)));
        }
    }
    if let Some(ref rec) = d.recommendation {
        if !rec.is_empty() {
            buf.push_str(&format!("**Recommendation**: {}\n", redact(rec)));
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

            // ── Scope note (M61) ──
            // Anchors the report's posture so a reader does not mistake a
            // 72/100 health score for a behaviour-correctness verdict.
            // Behavioural / runtime regressions are the MR review path's
            // job, not this whole-repo health sweep's.
            md.push_str(
                "> **定位 / Scope**: 全仓体检报告（结构、卫生、静态信号为主），不覆盖运行时行为回归——行为正确性请走 MR 评审。\
                 This is a whole-repo health sweep (structure, hygiene, static signals); behavioural regressions are out of scope — use the MR review path.\n\n",
            );

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
                // RENG-113 round 2: lead_summary is the architecture expert's
                // LLM prose — same class as `s.summary` (redacted at line 154),
                // and the same class as Evidence blocks (the team renderer's
                // TL;DR is redacted through `redact()` too).
                md.push_str(&format!("> {}\n\n", redact(summary)));
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
                md.push_str(&format!("**Summary**: {}\n\n", redact(&s.summary)));
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
                    // RENG-113: `message` and `recommendation` are LLM-derived
                    // prose — the same detector that masks Evidence runs here,
                    // so a security finding's quoted config line doesn't
                    // smuggle plaintext into the action-items table.
                    md.push_str(&format!(
                        "| {} | {} | {} | {} | {} | {} |\n",
                        i + 1,
                        item.area,
                        item.severity,
                        redact(&item.message),
                        redact(&item.recommendation),
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
            md.push_str(&format!(
                "**Recommendation**: {}\n",
                // RENG-113 round 2: the aggregator's LLM prose — same class as
                // the per-finding recommendation (redacted inside the action
                // items table at line 191). Without this, the conclusion
                // section is the last unredacted channel in the report.
                redact(&output.conclusion.recommendation)
            ));

            // ── Chunks Reviewed (M26) ──
            // The chunk-level coverage signal the Pass 2 loop collected —
            // total / reviewed / not-reviewed counts plus the per-failure
            // (module + LOC + reason) breakdown. Gated on `total > 0` so the
            // local-only path (no chunks scheduled) and reports with no LLM
            // configs don't render an empty section. Rendered BEFORE the
            // verification appendix so the "this chunk was never reviewed"
            // signal lands earlier than the verification noise.
            //
            // M38: each failure row also inlines the parser reason code
            // (when present) inside the Reason cell, prefixed with the
            // stable `[reason_code=...]` marker the parser itself logs
            // at INFO. The marker is the grep handle a reader uses to ask
            // "how many chunks failed for X reason?" — it survives
            // whitespace collapses inside backticks and is absent on
            // failure classes the parser never classified (LLM transport
            // errors, the M26 empty-completion retry exhausting).
            if output.chunk_summary.total > 0 {
                let cs = &output.chunk_summary;
                md.push_str("---\n\n## Chunks Reviewed\n\n");
                md.push_str(&format!(
                    "- **Total chunks**: {}\n- **Reviewed**: {}\n- **Not reviewed**: {}\n\n",
                    cs.total, cs.reviewed, cs.not_reviewed
                ));
                if !cs.failures.is_empty() {
                    md.push_str(
                        "| Module | LOC | Reason |\n\
                         |--------|-----|--------|\n",
                    );
                    for f in &cs.failures {
                        // Reasons are LLM-error / anyhow-chain prose — keep
                        // them inside backticks so the table cell doesn't
                        // wrap unpredictably. Redaction happens at the error
                        // site (the LLM provider layer) — by the time the
                        // message reaches us it carries no secrets.
                        let reason = f.reason.replace('\n', " ");
                        let cell = match f.reason_code.as_deref() {
                            Some(code) => format!("`[reason_code={code}] {reason}`"),
                            None => format!("`{reason}`"),
                        };
                        md.push_str(&format!("| `{}` | {} | {} |\n", f.module, f.loc, cell));
                    }
                    md.push('\n');
                }
                md.push('\n');
            }

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
