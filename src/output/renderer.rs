use crate::models::*;
use crate::output::markdown::close_unclosed_code_fences;

const NO_ISSUES_FOUND: &str = "No issues found.\n";

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "CRITICAL",
        Severity::High => "HIGH",
        Severity::Medium => "MEDIUM",
        Severity::Low => "LOW",
        Severity::Note => "NOTE",
    }
}

/// Render a single expert's findings as a Markdown section.
///
/// Each finding is formatted with a severity label, the file path and
/// line number, and a summary description. If there are no findings, a
/// "no issues found" message is returned.
pub fn render_expert_markdown(expert_name: &str, findings: &[Finding]) -> String {
    let header = format!("## {} Review\n\n", capitalize(expert_name));
    if findings.is_empty() {
        return format!("{}{}", header, NO_ISSUES_FOUND);
    }

    let mut body = String::new();
    for f in findings {
        let line_info = f.line.map_or(String::new(), |l| format!(":{}", l));
        body.push_str(&format!("### [{}] {}\n\n", severity_label(&f.severity), f.title,));
        body.push_str(&format!(
            "**File**: `{file}{line}`\n\n{summary}\n\n",
            file = f.file,
            line = line_info,
            summary = close_unclosed_code_fences(&f.summary),
        ));
    }

    format!("{}{}", header, body)
}

/// Render all findings as a consolidated Markdown report.
///
/// Findings are sorted by severity (critical first) and formatted in
/// a compact list with severity labels, file paths, and titles. If
/// there are no findings, a "no issues found" message is returned.
pub fn render_aggregated_markdown(findings: &[Finding]) -> String {
    let severity_order = |s: &Severity| -> usize {
        match s {
            Severity::Critical => 0,
            Severity::High => 1,
            Severity::Medium => 2,
            _ => 3,
        }
    };

    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| severity_order(&f.severity));

    let mut out = String::from("# PR Review Report\n\n");
    for f in &sorted {
        let line = f.line.map_or(String::new(), |l| format!(":{}", l));
        out.push_str(&format!(
            "- **[{severity}]** `{file}{line}` — {title}\n",
            severity = severity_label(&f.severity),
            file = f.file,
            line = line,
            title = f.title,
        ));
    }

    if findings.is_empty() {
        out.push_str(NO_ISSUES_FOUND);
    }

    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Render the "Dropped by verification" appendix for the verification pass.
///
/// `checked` is the total number of findings the pass examined (kept plus
/// dropped). When `verification_enabled` is false the run-summary lines are
/// omitted, so an empty `dropped` list yields an empty string (no appendix);
/// callers that don't know whether the pass ran (e.g. publish) pass `false`
/// to keep the historical list-only rendering. When the pass ran but dropped
/// nothing, a one-line note is rendered so users can tell the pass ran;
/// otherwise each dropped finding is listed with file, title, expert, and the
/// verifier's reason, followed by the run summary.
pub fn render_dropped_findings_appendix(
    dropped: &[crate::team::verifier::DroppedFinding],
    verification_enabled: bool,
    checked: usize,
) -> String {
    if dropped.is_empty() {
        if !verification_enabled {
            return String::new();
        }
        return format!(
            "## Dropped by verification\n\n_Verification pass ran: no findings were dropped ({} checked)._\n",
            checked
        );
    }

    let mut out = String::from("## Dropped by verification\n\n");
    for d in dropped {
        let f = &d.finding;
        let line = f.line.map_or(String::new(), |l| format!(":{}", l));
        out.push_str(&format!(
            "- `{file}{line}` — **{title}** ({expert}): {reason}\n",
            file = f.file,
            line = line,
            title = f.title,
            expert = f.expert_name,
            reason = d.reason,
        ));
    }
    if verification_enabled {
        out.push_str(&format!(
            "\n_Verification pass ran: {} findings checked, {} dropped._\n",
            checked,
            dropped.len()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_finding(severity: Severity, title: &str, file: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line: Some(42),
            line_end: None,
            severity,
            confidence: 8,
            category: String::new(),
            title: title.to_string(),
            summary: "Detail".to_string(),
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

    // ── render_expert_markdown ──

    #[test]
    fn test_render_expert_markdown_shows_severity_label() {
        let findings = vec![make_test_finding(Severity::High, "Test issue", "src/main.rs")];
        let md = render_expert_markdown("security", &findings);
        assert!(md.contains("Security Review"));
        assert!(md.contains("[HIGH]"));
        assert!(md.contains("Test issue"));
        assert!(md.contains("src/main.rs:42"));
    }

    #[test]
    fn test_render_expert_markdown_empty() {
        let md = render_expert_markdown("security", &[]);
        assert!(md.contains("No issues found"));
    }

    #[test]
    fn test_render_expert_markdown_critical_label() {
        let md = render_expert_markdown("test", &[make_test_finding(Severity::Critical, "X", "f.rs")]);
        assert!(md.contains("CRITICAL"));
    }

    #[test]
    fn test_render_expert_markdown_high_label() {
        let md = render_expert_markdown("test", &[make_test_finding(Severity::High, "X", "f.rs")]);
        assert!(md.contains("HIGH"));
    }

    #[test]
    fn test_render_expert_markdown_medium_label() {
        let md = render_expert_markdown("test", &[make_test_finding(Severity::Medium, "X", "f.rs")]);
        assert!(md.contains("MEDIUM"));
    }

    #[test]
    fn test_render_expert_markdown_low_label() {
        let md = render_expert_markdown("test", &[make_test_finding(Severity::Low, "X", "f.rs")]);
        assert!(md.contains("LOW"));
    }

    #[test]
    fn test_render_expert_markdown_note_label() {
        let md = render_expert_markdown("test", &[make_test_finding(Severity::Note, "X", "f.rs")]);
        assert!(md.contains("NOTE"));
    }

    // ── render_aggregated_markdown ──

    #[test]
    fn test_render_aggregated_markdown_sorted_by_severity() {
        let findings = vec![
            make_test_finding(Severity::Low, "Low issue", "a.rs"),
            make_test_finding(Severity::Critical, "Critical issue", "b.rs"),
            make_test_finding(Severity::Medium, "Medium issue", "c.rs"),
        ];
        let md = render_aggregated_markdown(&findings);
        // Critical should appear before Medium which appears before Low
        let crit_pos = md.find("[CRITICAL]").expect("critical should be present");
        let med_pos = md.find("[MEDIUM]").expect("medium should be present");
        let low_pos = md.find("[LOW]").expect("low should be present");
        assert!(crit_pos < med_pos, "critical should sort before medium");
        assert!(med_pos < low_pos, "medium should sort before low");
    }

    #[test]
    fn test_render_aggregated_markdown_empty() {
        let md = render_aggregated_markdown(&[]);
        assert!(md.contains("No issues found"));
    }

    #[test]
    fn test_render_aggregated_markdown_labels() {
        let findings = vec![make_test_finding(Severity::High, "Bug", "src/main.rs")];
        let md = render_aggregated_markdown(&findings);
        assert!(md.contains("[HIGH]"));
        assert!(md.contains("src/main.rs"));
        assert!(md.contains("Bug"));
    }

    // ── render_dropped_findings_appendix ──

    #[test]
    fn test_render_dropped_findings_appendix_disabled_is_empty() {
        // Pass not enabled (or caller doesn't know): no appendix at all.
        assert!(render_dropped_findings_appendix(&[], false, 0).is_empty());
    }

    #[test]
    fn test_render_dropped_findings_appendix_enabled_no_drops() {
        let md = render_dropped_findings_appendix(&[], true, 7);
        assert!(md.contains("## Dropped by verification"));
        assert!(md.contains("no findings were dropped (7 checked)"));
    }

    #[test]
    fn test_render_dropped_findings_appendix_lists_drops_with_count() {
        let dropped = vec![crate::team::verifier::DroppedFinding {
            finding: make_test_finding(Severity::High, "False alarm", "src/lib.rs"),
            reason: "Claim disproven by file content".to_string(),
        }];
        let md = render_dropped_findings_appendix(&dropped, true, 5);
        assert!(md.contains("## Dropped by verification"));
        assert!(md.contains("src/lib.rs:42"));
        assert!(md.contains("False alarm"));
        assert!(md.contains("test")); // expert name
        assert!(md.contains("Claim disproven by file content"));
        assert!(md.contains("5 findings checked, 1 dropped"));
    }

    #[test]
    fn test_render_dropped_findings_appendix_disabled_lists_drops_without_summary() {
        // Legacy rendering (publish path): list only, no run-summary lines.
        let dropped = vec![crate::team::verifier::DroppedFinding {
            finding: make_test_finding(Severity::High, "False alarm", "src/lib.rs"),
            reason: "Claim disproven by file content".to_string(),
        }];
        let md = render_dropped_findings_appendix(&dropped, false, 0);
        assert!(md.contains("## Dropped by verification"));
        assert!(md.contains("False alarm"));
        assert!(!md.contains("Verification pass ran"));
    }
}
