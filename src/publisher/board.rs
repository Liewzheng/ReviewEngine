//! Assembly of the `# CodeReview Board` note body.
//!
//! The board is the **complete** record of a round: the per-expert sections,
//! the lead's consolidation summary, the aggregator's report and the
//! verification appendix are all rendered here, and none of them is affected by
//! the inline-note delivery policy ([`crate::publisher::PublishPolicy`]). A
//! finding the policy keeps out of the inline delivery — below a floor, or
//! rolled up behind the per-round cap — is part of the board for exactly the
//! same reason as before: the per-expert sections come from
//! `output.reports`, which nothing in the publish path rewrites.
//!
//! The one thing the policy adds is its own section
//! ([`crate::publisher::InlinePlan::board_section`]), placed just before the
//! verification appendix: it names the findings that were admitted by the
//! policy but withheld from inline delivery, and states that a
//! documentation/CI-only round is summary-only. Without it, those decisions
//! were invisible — a capped finding existed only inside the per-expert
//! sections and an all-docs round gave no reason for having posted no notes.

use crate::models::ReviewOutput;
use crate::publisher::{InlinePlan, PublishPolicy};

/// Render the board note for `output` under the round's inline-note `plan`.
pub fn render_board(output: &ReviewOutput, plan: &InlinePlan<'_>, policy: &PublishPolicy) -> String {
    let mut md = String::from(crate::publisher::REVIEW_REPORT_PREFIX);
    for report in &output.reports {
        // render_expert_section appends the parse-failure / raw-response
        // annotations that the pre-rendered `markdown` does not carry, so a
        // silent zero-finding run is never mistaken for a clean review.
        md.push_str(&crate::output::team_renderer::render_expert_section(report));
        md.push_str("\n\n---\n\n");
    }
    // Lead consolidation summary (score / TL;DR / conflicts), rendered after
    // the per-expert reports and before the verification appendix.
    if let Some(ref consolidated) = output.consolidated {
        md.push_str(&crate::output::team_renderer::render_lead_summary(consolidated));
        md.push_str("\n\n---\n\n");
    }
    // Aggregator expert's LLM-aggregated report: rendered after the lead
    // summary (if any) so it can build on the same context, then before the
    // inline-note policy section. The markdown is already pre-rendered by the
    // aggregator; we emit it verbatim and skip empty fragments.
    if let Some(ref aggregated) = output.aggregated {
        if !aggregated.markdown.trim().is_empty() {
            md.push_str(&aggregated.markdown);
            md.push_str("\n\n---\n\n");
        }
    }
    let inline_section = plan.board_section(policy);
    if !inline_section.is_empty() {
        md.push_str(&inline_section);
        md.push_str("\n\n---\n\n");
    }
    // `false` keeps the historical list-only rendering here; the run-summary
    // lines are only added to the CLI Markdown report.
    md.push_str(&crate::output::renderer::render_dropped_findings_appendix(
        &output.dropped_findings,
        false,
        0,
    ));
    md
}
