//! Finding-verification pass (false-positive reduction, phase 2).
//!
//! After [`validate_findings`](crate::output::parser::validate_findings) has
//! dropped findings that point outside the diff, this optional pass asks an
//! LLM to re-check each remaining finding against ground-truth context the
//! experts never saw: the file's diff hunks, its current full content from the
//! local checkout, and the complete changed-file list. Findings the evidence
//! disproves are dropped and reported as [`DroppedFinding`]; everything else
//! is kept — the pass is fail-open on any LLM or parsing error.
//!
//! When `files` is empty the pass runs in *no-hunk mode* (used by
//! repo-review): the prompt carries no diff-hunk section, the file list is
//! derived from the findings themselves, and the system-prompt wording is
//! adapted — keep/drop semantics are unchanged.

use crate::llm::client::LLMClient;
use crate::models::{DiffFile, ExpertReport, Finding, LLMConfig};
use crate::prompt::templates::VERIFIER_SYSTEM_TEMPLATE;
use serde::{Deserialize, Serialize};

/// Maximum number of findings sent to the verifier in a single LLM call.
const MAX_FINDINGS_PER_BATCH: usize = 10;

/// A finding removed by the verification pass, together with the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedFinding {
    /// The finding that was dropped.
    pub finding: Finding,
    /// The verifier's reason for dropping it.
    pub reason: String,
}

/// Re-check all findings in `reports` with an LLM verification pass.
///
/// Findings are grouped by referenced file and verified in batches of at most
/// [`MAX_FINDINGS_PER_BATCH`]. Dropped findings are removed from their report
/// (the pre-rendered `markdown` is left untouched) and returned. The pass never
/// fails: on any error the affected batch is kept in full.
pub(crate) async fn verify_findings(
    reports: &mut [ExpertReport],
    files: &[DiffFile],
    project_path: &str,
    llm_configs: &[LLMConfig],
    max_file_bytes: usize,
) -> Vec<DroppedFinding> {
    if llm_configs.is_empty() {
        tracing::warn!("Verification pass enabled but no LLM configs available; skipping");
        return Vec::new();
    }

    let client = LLMClient::new();
    let configs = llm_configs.to_vec();
    verify_with_llm(reports, files, project_path, max_file_bytes, move |system, user| {
        let client = client.clone();
        let configs = configs.clone();
        async move {
            client
                .complete_with_fallback(&configs, &system, &user)
                .await
                .map(|r| r.content)
        }
    })
    .await
}

/// Core verification loop with the LLM call injected for testability.
///
/// When `files` is empty the pass runs in no-hunk mode (repo-review): the
/// prompt contains no diff-hunk section, the file list is derived from the
/// findings themselves, and the system prompt wording is adapted. Keep/drop
/// semantics are identical in both modes.
async fn verify_with_llm<F, Fut>(
    reports: &mut [ExpertReport],
    files: &[DiffFile],
    project_path: &str,
    max_file_bytes: usize,
    llm: F,
) -> Vec<DroppedFinding>
where
    F: Fn(String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<String>> + Send,
{
    // Group (report_idx, finding_idx) pairs by referenced file, preserving
    // first-seen order.
    let mut groups: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
    for (r, report) in reports.iter().enumerate() {
        for (f, finding) in report.findings.iter().enumerate() {
            match groups.iter_mut().find(|(path, _)| *path == finding.file) {
                Some((_, idxs)) => idxs.push((r, f)),
                None => groups.push((finding.file.clone(), vec![(r, f)])),
            }
        }
    }

    let has_hunks = !files.is_empty();
    // File list shown to the verifier: the diff's changed files, or — in
    // no-hunk mode — the files referenced by the findings themselves.
    let file_list: Vec<&str> = if has_hunks {
        files.iter().map(|f| f.path.as_str()).collect()
    } else {
        groups.iter().map(|(path, _)| path.as_str()).collect()
    };
    let system = verifier_system_prompt(has_hunks);
    let mut drop_marks: std::collections::HashMap<(usize, usize), String> = std::collections::HashMap::new();

    for (file, group) in &groups {
        let hunks: Option<String> = if has_hunks {
            Some(
                files
                    .iter()
                    .find(|d| d.path == *file)
                    .map(crate::diff::processor::render_file_diff)
                    .unwrap_or_else(|| "(file not present in the diff)".to_string()),
            )
        } else {
            None
        };
        let content = load_file_context(project_path, file, max_file_bytes);

        for batch in group.chunks(MAX_FINDINGS_PER_BATCH) {
            let user = build_user_prompt(file, hunks.as_deref(), &content, &file_list, batch, reports);
            let response = match llm(system.clone(), user).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Verification LLM call failed for '{}': {:?}; keeping all findings in batch",
                        file,
                        e
                    );
                    continue;
                }
            };
            let decisions = match parse_verdicts(&response) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse verification verdicts for '{}': {:?}; keeping all findings in batch",
                        file,
                        e
                    );
                    continue;
                }
            };
            for d in decisions {
                if d.drop {
                    if let Some(&(r, f)) = batch.get(d.index) {
                        drop_marks.insert((r, f), d.reason);
                    }
                }
            }
        }
    }

    if drop_marks.is_empty() {
        return Vec::new();
    }

    let mut dropped = Vec::new();
    for (r, report) in reports.iter_mut().enumerate() {
        let mut kept = Vec::with_capacity(report.findings.len());
        for (f, finding) in std::mem::take(&mut report.findings).into_iter().enumerate() {
            match drop_marks.get(&(r, f)) {
                Some(reason) => dropped.push(DroppedFinding {
                    finding,
                    reason: reason.clone(),
                }),
                None => kept.push(finding),
            }
        }
        report.findings = kept;
    }
    dropped
}

/// A single keep/drop decision parsed from the verifier's YAML response.
struct VerdictDecision {
    index: usize,
    drop: bool,
    reason: String,
}

/// Parse the verifier's YAML verdict list. Tolerates fenced code blocks and
/// surrounding prose; entries with an unusable `index` are skipped. Returns an
/// error when no verdict list can be extracted at all (caller keeps the batch).
fn parse_verdicts(text: &str) -> anyhow::Result<Vec<VerdictDecision>> {
    let cleaned = crate::output::parser::clean_yaml(text);
    let value: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&cleaned) {
        Ok(v) => v,
        Err(e) => {
            let fenced = crate::output::parser::extract_first_fenced_yaml(text)
                .ok_or_else(|| anyhow::anyhow!("verdict YAML parse failed: {}", e))?;
            serde_yaml_ng::from_str(&fenced).map_err(|e2| anyhow::anyhow!("verdict YAML parse failed: {}", e2))?
        }
    };

    let items = value
        .get("verdicts")
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("verdict response has no 'verdicts' list"))?;

    let mut decisions = Vec::with_capacity(items.len());
    for item in items {
        let Some(index) = item.get("index").and_then(|v| v.as_u64()) else {
            continue;
        };
        let verdict = item.get("verdict").and_then(|v| v.as_str()).unwrap_or("keep");
        let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
        decisions.push(VerdictDecision {
            index: index as usize,
            drop: verdict.eq_ignore_ascii_case("drop"),
            reason,
        });
    }
    Ok(decisions)
}

/// Read the current content of the referenced file from the local checkout.
/// Never fails: unreadable, non-UTF-8, or escaping paths yield a note string.
/// Content beyond `max_file_bytes` is truncated with a note.
fn load_file_context(project_path: &str, file: &str, max_file_bytes: usize) -> String {
    let rel = std::path::Path::new(file);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return "(file content unavailable: path escapes the project root)".to_string();
    }
    let full = std::path::Path::new(project_path).join(rel);
    let bytes = match std::fs::read(&full) {
        Ok(b) => b,
        Err(_) => return "(file content unavailable: not readable from the local checkout)".to_string(),
    };
    let mut text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => return "(file content unavailable: not valid UTF-8)".to_string(),
    };
    if text.len() > max_file_bytes {
        let boundary = text.floor_char_boundary(max_file_bytes);
        text.truncate(boundary);
        text.push_str("\n... (file content truncated: exceeded max_file_bytes)");
    }
    text
}

/// Phrase in [`VERIFIER_SYSTEM_TEMPLATE`] describing what the experts saw;
/// only accurate for diff-based reviews. (Spans a line wrap in the template.)
const HUNK_EXPERT_PHRASE: &str = "while seeing only fragments of a\ndiff";
/// No-hunk replacement for [`HUNK_EXPERT_PHRASE`] (repo-review experts saw
/// whole files, not diff fragments).
const NO_HUNK_EXPERT_PHRASE: &str = "while seeing only individual source files, not the whole repository";
/// Phrase in [`VERIFIER_SYSTEM_TEMPLATE`] describing the ground-truth
/// context; only accurate when diff hunks are available. (Spans line wraps.)
const HUNK_CONTEXT_PHRASE: &str = "the diff hunks of\nthe referenced file, the file's current full content, and the complete list of\nfiles changed in this merge request";
/// No-hunk replacement for [`HUNK_CONTEXT_PHRASE`].
const NO_HUNK_CONTEXT_PHRASE: &str = "the referenced file's current full content and the complete list of files referenced by the findings (no diff hunks are available)";

/// System prompt for the verifier, with the context description adapted to
/// whether diff hunks are available. The keep/drop rules are identical in
/// both modes; only the framing of what was seen and what is provided
/// changes.
fn verifier_system_prompt(has_hunks: bool) -> String {
    if has_hunks {
        VERIFIER_SYSTEM_TEMPLATE.to_string()
    } else {
        VERIFIER_SYSTEM_TEMPLATE
            .replace(HUNK_EXPERT_PHRASE, NO_HUNK_EXPERT_PHRASE)
            .replace(HUNK_CONTEXT_PHRASE, NO_HUNK_CONTEXT_PHRASE)
    }
}

/// Build the user prompt for one verification batch.
///
/// In no-hunk mode (`hunks` is `None`) the diff-hunk section is replaced by
/// a note and the file list is introduced as "files referenced by the
/// findings" instead of "changed files in this merge request".
fn build_user_prompt(
    file: &str,
    hunks: Option<&str>,
    content: &str,
    file_list: &[&str],
    batch: &[(usize, usize)],
    reports: &[ExpertReport],
) -> String {
    let mut out = String::new();
    if hunks.is_some() {
        out.push_str("## Changed files in this merge request\n");
    } else {
        out.push_str("## Files referenced by the findings\n");
    }
    for p in file_list {
        out.push_str(&format!("- {}\n", p));
    }
    out.push_str(&format!("\n## File under verification: `{}`\n\n", file));
    match hunks {
        Some(hunks) => out.push_str(&format!(
            "### Diff hunks for this file\n```diff\n{}\n```\n\n",
            hunks.trim_end()
        )),
        None => out.push_str(
            "No diff hunks are available for this review; verify each finding against the full file content below.\n\n",
        ),
    }
    out.push_str(&format!(
        "### Current content of `{}`\n```\n{}\n```\n\n",
        file,
        content.trim_end()
    ));

    out.push_str("## Findings to verify\n");
    for (i, &(r, f)) in batch.iter().enumerate() {
        let finding = &reports[r].findings[f];
        let line = match (finding.line, finding.line_end) {
            (Some(l), Some(le)) if le != l => format!("{}-{}", l, le),
            (Some(l), _) => l.to_string(),
            (None, _) => "n/a".to_string(),
        };
        out.push_str(&format!(
            "\n### Finding [{}]\n- expert: {} ({})\n- severity: {}, confidence: {}/10, category: {}\n- location: `{}:{}`\n- title: {}\n- summary: {}\n- evidence: {}\n- impact: {}\n- recommendation: {}\n- effort: {}\n",
            i,
            finding.expert_name,
            finding.expert_role,
            finding.severity,
            finding.confidence,
            finding.category,
            finding.file,
            line,
            finding.title,
            finding.summary,
            finding.evidence,
            finding.impact,
            finding.recommendation,
            finding.effort,
        ));
        if !finding.agrees_with.is_empty() {
            out.push_str(&format!("- agrees_with: {}\n", finding.agrees_with.join(", ")));
        }
        if !finding.references.is_empty() {
            out.push_str(&format!("- references: {}\n", finding.references.join(", ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DiffHunk, Effort, Severity};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn make_finding(file: &str, title: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line: Some(10),
            line_end: None,
            severity: Severity::High,
            confidence: 8,
            category: "test".to_string(),
            title: title.to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    fn make_report(expert: &str, findings: Vec<Finding>) -> ExpertReport {
        ExpertReport {
            expert_name: expert.to_string(),
            findings,
            markdown: String::new(),
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
        }
    }

    fn make_diff_file(path: &str) -> DiffFile {
        DiffFile {
            path: path.to_string(),
            old_path: path.to_string(),
            new_path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: vec![DiffHunk {
                header: "@@ -10,2 +10,2 @@".to_string(),
                old_start: 10,
                old_lines: 2,
                new_start: 10,
                new_lines: 2,
                lines: vec![],
            }],
        }
    }

    // ─── parse_verdicts ──────────────────────────

    #[test]
    fn test_parse_verdicts_fenced_yaml() {
        let text = "```yaml\nverdicts:\n  - index: 0\n    verdict: keep\n    reason: \"\"\n  - index: 1\n    verdict: drop\n    reason: \"X is present in the file\"\n```";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(!decisions[0].drop);
        assert!(decisions[1].drop);
        assert_eq!(decisions[1].index, 1);
        assert_eq!(decisions[1].reason, "X is present in the file");
    }

    #[test]
    fn test_parse_verdicts_plain_yaml_without_fence() {
        let text = "verdicts:\n  - index: 2\n    verdict: drop\n    reason: \"disproven\"\n";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].drop);
        assert_eq!(decisions[0].index, 2);
    }

    #[test]
    fn test_parse_verdicts_missing_fields_default_to_keep() {
        // Missing `reason` defaults to ""; missing `verdict` defaults to keep;
        // an entry without a usable `index` is skipped.
        let text = "verdicts:\n  - index: 0\n    verdict: drop\n  - index: 1\n  - verdict: drop\n";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(decisions[0].drop);
        assert_eq!(decisions[0].reason, "");
        assert!(!decisions[1].drop);
        assert_eq!(decisions[1].index, 1);
    }

    #[test]
    fn test_parse_verdicts_tolerates_surrounding_text() {
        let text = "Here are my verdicts.\n```yaml\nverdicts:\n  - index: 0\n    verdict: keep\n    reason: \"\"\n```\nHope this helps.";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions.len(), 1);
        assert!(!decisions[0].drop);
    }

    #[test]
    fn test_parse_verdicts_garbage_errors() {
        assert!(parse_verdicts("!!! not yaml at all !!!").is_err());
    }

    #[test]
    fn test_parse_verdicts_missing_verdicts_key_errors() {
        assert!(parse_verdicts("summary: \"no verdicts here\"\n").is_err());
    }

    // ─── load_file_context ───────────────────────

    #[test]
    fn test_load_file_context_missing_file() {
        let ctx = load_file_context("/nonexistent/path", "src/main.rs", 20000);
        assert!(ctx.contains("unavailable"));
    }

    #[test]
    fn test_load_file_context_rejects_escaping_paths() {
        assert!(load_file_context("/tmp", "../secret", 20000).contains("unavailable"));
        assert!(load_file_context("/tmp", "/etc/passwd", 20000).contains("unavailable"));
    }

    #[test]
    fn test_load_file_context_reads_and_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("a.rs");
        std::fs::write(&file_path, "0123456789abcdef").unwrap();

        let full = load_file_context(dir.path().to_str().unwrap(), "a.rs", 20000);
        assert_eq!(full, "0123456789abcdef");

        let truncated = load_file_context(dir.path().to_str().unwrap(), "a.rs", 10);
        assert!(truncated.starts_with("0123456789"));
        assert!(truncated.contains("truncated"));
    }

    // ─── verify_with_llm ─────────────────────────

    #[tokio::test]
    async fn test_verify_drops_disproved_finding() {
        let mut reports = vec![make_report(
            "security",
            vec![
                make_finding("src/a.rs", "False alarm"),
                make_finding("src/a.rs", "Real bug"),
            ],
        )];
        let files = vec![make_diff_file("src/a.rs")];
        let llm = |_s: String, _u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: drop\n    reason: \"Claim disproven by file content\"\n  - index: 1\n    verdict: keep\n    reason: \"\"\n".to_string())
        };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert_eq!(reports[0].findings.len(), 1);
        assert_eq!(reports[0].findings[0].title, "Real bug");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].finding.title, "False alarm");
        assert_eq!(dropped[0].reason, "Claim disproven by file content");
    }

    #[tokio::test]
    async fn test_verify_fail_open_on_llm_error() {
        let mut reports = vec![make_report("security", vec![make_finding("src/a.rs", "Bug")])];
        let files = vec![make_diff_file("src/a.rs")];
        let llm = |_s: String, _u: String| async { anyhow::bail!("network down") };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(reports[0].findings.len(), 1);
    }

    #[tokio::test]
    async fn test_verify_fail_open_on_parse_failure() {
        let mut reports = vec![make_report("security", vec![make_finding("src/a.rs", "Bug")])];
        let files = vec![make_diff_file("src/a.rs")];
        let llm = |_s: String, _u: String| async { Ok("total garbage, no yaml".to_string()) };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(reports[0].findings.len(), 1);
    }

    #[tokio::test]
    async fn test_verify_ignores_out_of_range_index() {
        let mut reports = vec![make_report("security", vec![make_finding("src/a.rs", "Bug")])];
        let files = vec![make_diff_file("src/a.rs")];
        let llm = |_s: String, _u: String| async {
            Ok("verdicts:\n  - index: 7\n    verdict: drop\n    reason: \"no such finding\"\n".to_string())
        };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(reports[0].findings.len(), 1);
    }

    #[tokio::test]
    async fn test_verify_batches_large_groups() {
        let findings: Vec<Finding> = (0..11)
            .map(|i| make_finding("src/a.rs", &format!("Issue {}", i)))
            .collect();
        let mut reports = vec![make_report("security", findings)];
        let files = vec![make_diff_file("src/a.rs")];

        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_s: String, _u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(reports[0].findings.len(), 11);
        assert_eq!(calls.load(Ordering::SeqCst), 2); // 11 findings → 2 batches
    }

    #[tokio::test]
    async fn test_verify_groups_by_file_and_prompt_contains_context() {
        let mut reports = vec![
            make_report("security", vec![make_finding("src/a.rs", "A")]),
            make_report("quality", vec![make_finding("src/b.rs", "B")]),
        ];
        let files = vec![make_diff_file("src/a.rs"), make_diff_file("src/b.rs")];

        let prompts = Arc::new(Mutex::new(Vec::new()));
        let prompts2 = prompts.clone();
        let llm = move |_s: String, user: String| {
            prompts2.lock().unwrap().push(user);
            async { Ok("verdicts: []".to_string()) }
        };

        // project_path does not exist → file content unavailable, must not abort.
        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent/dir", 20000, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(reports[0].findings.len(), 1);
        assert_eq!(reports[1].findings.len(), 1);

        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2); // one LLM call per file group
        for p in prompts.iter() {
            assert!(p.contains("## Changed files in this merge request"));
            assert!(p.contains("- src/a.rs"));
            assert!(p.contains("- src/b.rs"));
            assert!(p.contains("file content unavailable"));
        }
        assert!(prompts.iter().any(|p| p.contains("`src/a.rs`")));
        assert!(prompts.iter().any(|p| p.contains("`src/b.rs`")));
    }

    // ─── no-hunk mode (repo-review) ─────────────

    #[test]
    fn test_verifier_system_prompt_no_hunk_adaptation() {
        // Guard the replace-based adaptation against template edits.
        assert!(VERIFIER_SYSTEM_TEMPLATE.contains(HUNK_EXPERT_PHRASE));
        assert!(VERIFIER_SYSTEM_TEMPLATE.contains(HUNK_CONTEXT_PHRASE));

        let hunk = verifier_system_prompt(true);
        assert_eq!(hunk, VERIFIER_SYSTEM_TEMPLATE);

        let no_hunk = verifier_system_prompt(false);
        assert!(no_hunk.contains(NO_HUNK_EXPERT_PHRASE));
        assert!(no_hunk.contains(NO_HUNK_CONTEXT_PHRASE));
        assert!(!no_hunk.contains(HUNK_EXPERT_PHRASE));
        assert!(!no_hunk.contains(HUNK_CONTEXT_PHRASE));
        assert!(!no_hunk.contains("merge request"));
    }

    #[tokio::test]
    async fn test_verify_no_hunk_mode_prompt_omits_hunk_section() {
        let mut reports = vec![make_report("code_quality", vec![make_finding("src/a.rs", "Bug")])];
        let files: Vec<DiffFile> = vec![];

        let prompts = Arc::new(Mutex::new(Vec::new()));
        let prompts2 = prompts.clone();
        let llm = move |_s: String, user: String| {
            prompts2.lock().unwrap().push(user);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent/dir", 20000, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(reports[0].findings.len(), 1);
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        let p = &prompts[0];
        assert!(!p.contains("### Diff hunks"));
        assert!(!p.contains("Changed files in this merge request"));
        assert!(p.contains("## Files referenced by the findings"));
        assert!(p.contains("- src/a.rs"));
        assert!(p.contains("No diff hunks are available"));
        assert!(p.contains("### Current content of `src/a.rs`"));
    }

    #[tokio::test]
    async fn test_verify_no_hunk_mode_drop_logic_unchanged() {
        let mut reports = vec![make_report(
            "code_quality",
            vec![
                make_finding("src/a.rs", "False alarm"),
                make_finding("src/a.rs", "Real bug"),
            ],
        )];
        let files: Vec<DiffFile> = vec![];
        let llm = |_s: String, _u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: drop\n    reason: \"Claim disproven by file content\"\n  - index: 1\n    verdict: keep\n    reason: \"\"\n".to_string())
        };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert_eq!(reports[0].findings.len(), 1);
        assert_eq!(reports[0].findings[0].title, "Real bug");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].finding.title, "False alarm");
    }

    #[tokio::test]
    async fn test_verify_no_hunk_mode_file_list_comes_from_findings() {
        // With no diff files, the prompt's file list is derived from the
        // findings' referenced files (one entry per unique file).
        let mut reports = vec![
            make_report("code_quality", vec![make_finding("src/a.rs", "A")]),
            make_report("code_quality", vec![make_finding("src/b.rs", "B")]),
        ];
        let files: Vec<DiffFile> = vec![];

        let prompts = Arc::new(Mutex::new(Vec::new()));
        let prompts2 = prompts.clone();
        let llm = move |_s: String, user: String| {
            prompts2.lock().unwrap().push(user);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = verify_with_llm(&mut reports, &files, "/nonexistent", 20000, llm).await;

        assert!(dropped.is_empty());
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        for p in prompts.iter() {
            assert!(p.contains("## Files referenced by the findings"));
            assert!(p.contains("- src/a.rs"));
            assert!(p.contains("- src/b.rs"));
        }
    }
}
