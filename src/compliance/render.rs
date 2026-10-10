//! Markdown rendering for the `## Ticket compliance check` section (M40 / RENG).
//!
//! The shape mirrors pr-agent's `ticket_compliance_check` output (per-ticket
//! 4-bullet list: requirements / satisfied / not-satisfied / needs-verification),
//! but with the field names the M39 report standardised.
//!
//! The renderer is pure: it takes a [`super::ComplianceOutput`] and returns
//! a Markdown string. The `format_output` (CLI) and `render_board`
//! (publisher) integration points append this section to their reports;
//! callers that have `compliance = None` get an empty string back.

use crate::models::redact_text;
use crate::output::markdown::close_unclosed_code_fences;

use super::{ComplianceOutput, FetchedTicket, TicketCheck, TicketProvider};

/// The heading used in both the CLI report and the published board.
pub const SECTION_HEADING: &str = "## Ticket compliance check";

/// Render `output` as Markdown. Returns an empty string when there is
/// nothing to render — never prints a heading without a body, so the
/// downstream renderers can decide whether to skip the section.
pub fn render_compliance_section(output: &ComplianceOutput) -> String {
    if output.checks.is_empty() && output.fetch_failures.is_empty() && output.notes.is_empty() {
        return String::new();
    }
    let mut out = String::from(SECTION_HEADING);
    out.push_str("\n\n");

    // Per-ticket: failed fetches first (the section promises per-ticket
    // status), then successful checks.
    for failed in &output.fetch_failures {
        out.push_str(&render_failed_fetch(failed));
        out.push_str("\n");
    }

    if !output.checks.is_empty() {
        out.push_str(&format!("_{} ticket(s) analysed._\n\n", output.checks.len()));
        for check in &output.checks {
            out.push_str(&render_check(check));
            out.push_str("\n");
        }
    }

    if !output.notes.is_empty() {
        out.push_str("\n**Notes**\n\n");
        for note in &output.notes {
            // Defence-in-depth: notes are produced by us, not the LLM, so
            // redaction is mostly a no-op here — but a note that quotes an
            // upstream error message (e.g. "fetch failed: HTTP 401: token
            // glpat-…") should be masked before it lands in the report.
            out.push_str(&format!("- {}\n", redact_text(note)));
        }
        out.push('\n');
    }

    if let (Some(provider), Some(model)) = (&output.llm_provider, &output.llm_model) {
        out.push_str(&format!(
            "\n_compliance LLM: {}/{} · {} check(s) · {} fetch failure(s)_\n",
            provider,
            model,
            output.checks.len(),
            output.fetch_failures.len(),
        ));
    }

    out
}

fn render_failed_fetch(failed: &FetchedTicket) -> String {
    let r = &failed.ticket_ref;
    let provider_label = match r.provider {
        TicketProvider::GitLab => "GitLab",
        TicketProvider::GitHub => "GitHub",
    };
    let reason = failed.error.as_deref().unwrap_or("unknown");
    format!(
        "### ⚠️ {} fetch failed — {} ({} #{})\n\n- URL: {}\n- Reason: {}\n",
        provider_label,
        r.source,
        r.project.as_deref().unwrap_or("?"),
        r.iid,
        r.url,
        redact_text(reason),
    )
}

fn render_check(check: &TicketCheck) -> String {
    let provider_label = match check.ticket_ref.provider {
        TicketProvider::GitLab => "GitLab",
        TicketProvider::GitHub => "GitHub",
    };
    let mut out = format!(
        "### {} {}\n\n- URL: {}\n- Ticket: {} (#{})\n",
        provider_label,
        check.ticket_ref.source,
        check.ticket_ref.url,
        check.ticket_ref.project.as_deref().unwrap_or("?"),
        check.ticket_ref.iid,
    );
    if !check.ticket_title.is_empty() {
        out.push_str(&format!("- Title: {}\n", redact_text(&check.ticket_title)));
    }
    if let Some(err) = &check.parse_error {
        // Surface the LLM-side parse failure the same way the per-expert
        // sections do, so the reader can correlate parser error with raw
        // dump. The raw response is captured on the TicketCheck for the
        // verbose-dump path; here we just name it.
        out.push_str(&format!("\n> ⚠️ **parse failure** / {}\n", redact_text(err)));
        if !check.raw_response.is_empty() {
            let excerpt: String = check.raw_response.chars().take(500).collect();
            let truncated = check.raw_response.chars().count() > 500;
            out.push_str(&format!(
                "\n**Raw LLM response** (verbatim)\n\n```text\n{}\n```\n",
                close_unclosed_code_fences(&redact_text(&excerpt))
            ));
            if truncated {
                out.push_str("… (truncated)\n");
            }
        }
        return out;
    }

    out.push_str("\n**Requirements** (from the ticket)\n\n");
    out.push_str(&bullets(&check.requirement));
    out.push_str("\n**Satisfied**\n\n");
    out.push_str(&bullets(&check.satisfied));
    out.push_str("\n**Not satisfied**\n\n");
    out.push_str(&bullets(&check.not_satisfied));
    out.push_str("\n**Needs verification**\n\n");
    out.push_str(&bullets(&check.need_verification));
    out
}

/// Normalise the field body into a bullet list: lines that already start
/// with `- ` pass through, blank lines collapse, non-empty lines without a
/// leading dash get a dash, and a single missing bullet becomes "- none".
fn bullets(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "- none\n".to_string();
    }
    let mut out = String::new();
    let mut wrote_any = false;
    for line in trimmed.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("- ") {
            out.push_str(line);
        } else {
            out.push_str("- ");
            out.push_str(line);
        }
        out.push('\n');
        wrote_any = true;
    }
    if !wrote_any {
        return "- none\n".to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compliance::{FetchedTicket, TicketCheck, TicketProvider, TicketRef};

    fn gitlab_ref(iid: u32) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitLab,
            project: Some("g/p".into()),
            iid,
            url: format!("https://gitlab.example.com/g/p/-/work_items/{iid}"),
            source: format!("!{iid}"),
        }
    }

    fn ok_check(iid: u32) -> TicketCheck {
        TicketCheck {
            ticket_ref: gitlab_ref(iid),
            ticket_title: "Test ticket".into(),
            requirement: "- one\n- two".into(),
            satisfied: "- done".into(),
            not_satisfied: "- gap".into(),
            need_verification: "- none".into(),
            parse_error: None,
            raw_response: String::new(),
        }
    }

    fn failed_fetch(iid: u32) -> FetchedTicket {
        FetchedTicket {
            ticket_ref: gitlab_ref(iid),
            title: String::new(),
            body: String::new(),
            web_url: None,
            error: Some("HTTP 404".into()),
        }
    }

    #[test]
    fn renders_section_heading() {
        let mut out = ComplianceOutput::empty();
        out.checks.push(ok_check(73));
        let rendered = render_compliance_section(&out);
        assert!(rendered.starts_with(SECTION_HEADING));
        assert!(rendered.contains("!73"));
        assert!(rendered.contains("https://gitlab.example.com/g/p/-/work_items/73"));
    }

    #[test]
    fn renders_failed_fetch_block() {
        let mut out = ComplianceOutput::empty();
        out.fetch_failures.push(failed_fetch(75));
        let rendered = render_compliance_section(&out);
        assert!(rendered.contains("fetch failed"));
        assert!(rendered.contains("HTTP 404"));
        assert!(rendered.contains("!75"));
    }

    #[test]
    fn renders_parse_failure_block() {
        let mut check = ok_check(73);
        check.parse_error = Some("missing field".into());
        check.raw_response = r#"{"not": "well-formed"}"#.into();
        let mut out = ComplianceOutput::empty();
        out.checks.push(check);
        let rendered = render_compliance_section(&out);
        assert!(rendered.contains("parse failure"));
        assert!(rendered.contains("missing field"));
        assert!(rendered.contains("Raw LLM response"));
    }

    #[test]
    fn empty_output_renders_nothing() {
        let out = ComplianceOutput::empty();
        assert!(render_compliance_section(&out).is_empty());
    }

    #[test]
    fn notes_section_renders() {
        let mut out = ComplianceOutput::empty();
        out.checks.push(ok_check(73));
        out.notes.push("description references 5 tickets, capped at 3".into());
        let rendered = render_compliance_section(&out);
        assert!(rendered.contains("**Notes**"));
        assert!(rendered.contains("capped at 3"));
    }

    #[test]
    fn bullets_normalises_text_without_leading_dash() {
        let out = bullets("first\nsecond");
        assert!(out.contains("- first"), "first line is bulleted: {out}");
        assert!(out.contains("- second"), "second line is bulleted: {out}");
    }

    #[test]
    fn bullets_handles_empty_input() {
        assert_eq!(bullets(""), "- none\n");
        assert_eq!(bullets("   "), "- none\n");
    }

    #[test]
    fn bullets_keeps_existing_dashes() {
        let out = bullets("- one\n- two");
        assert!(out.contains("- one"));
        assert!(out.contains("- two"));
    }

    #[test]
    fn section_carries_provider_label() {
        let mut out = ComplianceOutput::empty();
        out.checks.push(ok_check(73));
        let rendered = render_compliance_section(&out);
        assert!(rendered.contains("GitLab !73"));
        let mut gh_ref = gitlab_ref(73);
        gh_ref.provider = TicketProvider::GitHub;
        gh_ref.url = "https://github.com/o/r/issues/73".into();
        gh_ref.source = "#73".into();
        let mut check = ok_check(73);
        check.ticket_ref = gh_ref;
        let mut out = ComplianceOutput::empty();
        out.checks.push(check);
        let rendered = render_compliance_section(&out);
        assert!(rendered.contains("GitHub #73"));
    }

    #[test]
    fn section_carries_llm_attribution_when_present() {
        let mut out = ComplianceOutput::empty();
        out.checks.push(ok_check(73));
        out.llm_provider = Some("xiaomi".into());
        out.llm_model = Some("mimo-v2.5-pro".into());
        let rendered = render_compliance_section(&out);
        assert!(rendered.contains("compliance LLM: xiaomi/mimo-v2.5-pro"));
    }
}
