//! Final adjudication pass (false-positive reduction, phase 3).
//!
//! After lead consolidation, each finding at or above a configured severity
//! (default: High and Critical) is re-examined one last time by the
//! lead-model LLM against the FULL current content of the cited file —
//! fetched through a [`FileSource`] and deliberately NOT subject to the
//! expert-context (`max_context_file_bytes`) or verification-pass
//! (`verification_max_file_bytes`) byte caps, which hid defensive code far
//! from the diff hunk and let confident hallucinations survive. Findings the
//! actual code disproves are dropped with a recorded reason; overstated
//! findings are downgraded in place; everything else is kept.
//!
//! The pass is fail-open by construction: LLM call failures, unparseable
//! verdicts, or unfetchable files keep the finding unchanged — infrastructure
//! problems never silently drop a finding.
//!
//! Before any LLM call, a cheap deterministic pre-filter checks whether the
//! finding's quoted `evidence` actually appears in the real file; the result
//! is attached to the prompt as a PRE-FILTER NOTE (never an auto-drop — the
//! LLM decides with the hint).
//!
//! Ground truth comes from [`crate::team::file_source`]: the local checkout
//! for CLI reviews, the provider API at the reviewed commit SHA for
//! server-side (webhook/API) reviews, which never clone the repository and
//! where `project_path` is a provider slug like `group/project`. Until
//! RENG-31 only the local checkout existed, so the pass skipped wholesale on
//! the deployed path and dropped nothing, while High/10-confidence notes
//! landed on claims the author had already disproved. The diff patch is
//! still never a substitute for full-file content: a unified diff carries
//! only the changed regions ±3 context lines, so adjudicating against it
//! would risk fail-closed drops on code the patch simply does not show. When
//! no source can produce the file, the pass keeps the findings and says so.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::llm::client::LLMClient;
use crate::models::{Finding, LLMConfig, Severity};
use crate::prompt::templates::ADJUDICATOR_SYSTEM_TEMPLATE;
use crate::team::file_source::{FileReadError, FileSource};
use crate::team::verifier::DroppedFinding;

/// Maximum number of findings sent to the adjudicator in a single LLM call.
const MAX_FINDINGS_PER_BATCH: usize = 5;

/// Hard safety cap on file content injected into the adjudication prompt.
/// Files larger than this are represented by the cited region
/// (± [`REGION_CONTEXT_LINES`] lines) plus a function outline of the rest.
const HARD_FILE_CAP_BYTES: usize = 200_000;

/// Lines of context on each side of the cited line when a file exceeds
/// [`HARD_FILE_CAP_BYTES`].
const REGION_CONTEXT_LINES: u32 = 200;

/// Maximum line distance between the cited line and the located evidence
/// before the pre-filter flags a mismatch.
const EVIDENCE_LINE_TOLERANCE: u32 = 50;

/// Maximum number of ground-truth reads in flight while prefetching.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// Per-file read ceiling. A source that never answers counts as an
/// unfetchable file (fail-open) instead of stalling the rest of the pass.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Ceiling for the whole ground-truth prefetch phase. The review itself runs
/// under a caller-imposed budget (600 s for API reviews), which adjudication
/// must not eat: whatever is still unfetched when the budget is gone fails
/// open.
const FETCH_PHASE_TIMEOUT: Duration = Duration::from_secs(120);

/// Adjudication verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Confirmed,
    FalsePositive,
    Downgrade,
}

/// A single adjudication decision parsed from the YAML response.
struct Decision {
    index: usize,
    verdict: Verdict,
    new_severity: Option<Severity>,
    reason: String,
    cited_lines: String,
}

/// Rank order for severity comparison (higher = more severe).
pub(crate) fn severity_rank(s: &Severity) -> u8 {
    match s {
        Severity::Critical => 4,
        Severity::High => 3,
        Severity::Medium => 2,
        Severity::Low => 1,
        Severity::Note => 0,
    }
}

/// Parse the `adjudicate_min_severity` config value. Unrecognized values
/// fall back to `High` with a warning (fail-open toward adjudicating more,
/// never less, than the safe default).
pub(crate) fn parse_min_severity(value: &str) -> Severity {
    match value.trim().to_ascii_lowercase().as_str() {
        "critical" => Severity::Critical,
        "high" => Severity::High,
        "medium" => Severity::Medium,
        "low" => Severity::Low,
        "note" => Severity::Note,
        other => {
            tracing::warn!(
                "Unrecognized adjudicate_min_severity {:?}; falling back to \"high\"",
                other
            );
            Severity::High
        }
    }
}

/// Parse a severity label from the adjudicator's `new_severity` field.
fn parse_severity_label(value: &str) -> Option<Severity> {
    match value.trim().to_ascii_lowercase().as_str() {
        "critical" => Some(Severity::Critical),
        "high" => Some(Severity::High),
        "medium" => Some(Severity::Medium),
        "low" => Some(Severity::Low),
        "note" => Some(Severity::Note),
        _ => None,
    }
}

/// Run the adjudication pass over `findings` (the consolidated list),
/// mutating it in place and returning the findings dropped as false
/// positives together with the adjudicator's reasons.
///
/// Only findings at or above `min_severity` are examined. Ground truth comes
/// from the local checkout when `project_path` is a directory, otherwise from
/// `remote_source` (the provider-API source the caller plumbed in for
/// server-side reviews); when neither exists the pass keeps every candidate
/// and warns. It never fails: on any LLM, fetch or parsing error the affected
/// findings are kept.
pub(crate) async fn adjudicate_findings(
    findings: &mut Vec<Finding>,
    project_path: &str,
    remote_source: Option<Arc<dyn FileSource>>,
    llm_configs: &[LLMConfig],
    min_severity: &Severity,
) -> Vec<DroppedFinding> {
    if llm_configs.is_empty() {
        tracing::warn!("Adjudication pass enabled but no LLM configs available; skipping");
        return Vec::new();
    }

    let source = crate::team::file_source::resolve_ground_truth(project_path, remote_source);
    let client = LLMClient::new();
    let configs = llm_configs.to_vec();
    adjudicate_with_llm(findings, project_path, source.as_deref(), min_severity, move |user| {
        let client = client.clone();
        let configs = configs.clone();
        async move {
            client
                .complete_with_fallback(&configs, ADJUDICATOR_SYSTEM_TEMPLATE, &user)
                .await
                .map(|r| r.content)
        }
    })
    .await
}

/// Core adjudication loop with the LLM call injected for testability.
///
/// `project_path` is used only to name the review in the no-source fail-open
/// warning.
async fn adjudicate_with_llm<F, Fut>(
    findings: &mut Vec<Finding>,
    project_path: &str,
    source: Option<&dyn FileSource>,
    min_severity: &Severity,
    llm: F,
) -> Vec<DroppedFinding>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<String>> + Send,
{
    // Candidate indices: findings at or above the severity threshold,
    // grouped by referenced file in first-seen order.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, finding) in findings.iter().enumerate() {
        if severity_rank(&finding.severity) < severity_rank(min_severity) {
            continue;
        }
        match groups.iter_mut().find(|(path, _)| *path == finding.file) {
            Some((_, idxs)) => idxs.push(i),
            None => groups.push((finding.file.clone(), vec![i])),
        }
    }

    let candidate_count: usize = groups.iter().map(|(_, g)| g.len()).sum();
    if candidate_count == 0 {
        return Vec::new();
    }

    // No ground truth at all: every read below would fail, so skip once,
    // loudly, instead of emitting one INFO per file and a summary that
    // miscounts these findings as examined. Fail-open: all candidates are
    // kept unchanged. The diff patch alone is NOT a substitute ground truth —
    // it covers only changed regions ±3 lines, and adjudicating against it
    // would risk fail-closed drops on code the patch simply doesn't show.
    let Some(source) = source else {
        tracing::warn!(
            "Adjudication: no file source for '{}' — no local checkout exists and no provider-API file \
             source was available for this review (a server-side review needs the reviewed commit SHA and a \
             token with repository read access), so full-file ground truth cannot be obtained — \
             {} candidate finding(s) pass through UNADJUDICATED and are kept unchanged (fail-open).",
            project_path,
            candidate_count
        );
        return Vec::new();
    };

    // Ground truth for the pass, one read per (path, revision): a
    // finding-dense review cites the same file many times, and a repeated
    // failure must not be retried either.
    let revision = source.revision().unwrap_or("").to_string();
    let cache = prefetch_ground_truth(source, &groups, &revision).await;

    let mut drop_marks: HashMap<usize, String> = HashMap::new();
    let mut downgrades: Vec<(usize, Severity)> = Vec::new();
    let mut adjudicated_files = 0usize;
    let mut unauthorized_files = 0usize;

    for (file, group) in &groups {
        let key = (file.clone(), revision.clone());
        let content = match cache.get(&key) {
            Some(Ok(raw)) => format_full_file(raw, group.iter().find_map(|&i| findings[i].line)),
            // 401/403 is a credential-level failure: every other file would
            // fail the same way, so it is reported once for the pass.
            Some(Err(FileReadError::Unauthorized(_))) => {
                unauthorized_files += 1;
                continue;
            }
            Some(Err(note)) => {
                // Fail-open: without the ground-truth file the adjudicator
                // has nothing to judge against — keep every finding in the
                // group untouched rather than inviting speculative drops.
                // WARN, not INFO: inside a real checkout a missing file
                // (e.g. deleted by the MR) means these candidates are not
                // adjudicated, and that must be visible in the logs.
                tracing::warn!(
                    "Adjudication: skipping '{}': {} — {} finding(s) kept unchanged (fail-open)",
                    file,
                    note,
                    group.len()
                );
                continue;
            }
            // Unreachable: the prefetch fills an entry for every cited file.
            None => {
                tracing::warn!(
                    "Adjudication: skipping '{}': no ground-truth content available — {} finding(s) kept unchanged (fail-open)",
                    file,
                    group.len()
                );
                continue;
            }
        };
        adjudicated_files += 1;

        for batch in group.chunks(MAX_FINDINGS_PER_BATCH) {
            let user = build_user_prompt(file, &content, batch, findings);
            let response = match llm(user).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Adjudication LLM call failed for '{}': {:?}; keeping all findings in batch",
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
                        "Failed to parse adjudication verdicts for '{}': {:?}; keeping all findings in batch",
                        file,
                        e
                    );
                    continue;
                }
            };
            for d in decisions {
                let Some(&idx) = batch.get(d.index) else {
                    continue;
                };
                match d.verdict {
                    Verdict::FalsePositive => {
                        let reason = if d.cited_lines.is_empty() {
                            d.reason
                        } else {
                            format!("{} (adjudicator cited {}:{})", d.reason, file, d.cited_lines)
                        };
                        drop_marks.insert(idx, reason);
                    }
                    Verdict::Downgrade => {
                        let current = &findings[idx].severity;
                        match d.new_severity {
                            Some(target) if severity_rank(&target) < severity_rank(current) => {
                                downgrades.push((idx, target));
                            }
                            _ => {
                                tracing::warn!(
                                    "Adjudication: ignoring invalid downgrade for '{}' ({}:{:?}) — \
                                     missing, unparsable, or not-lower new_severity; keeping finding unchanged",
                                    findings[idx].title,
                                    file,
                                    findings[idx].line,
                                );
                            }
                        }
                    }
                    Verdict::Confirmed => {}
                }
            }
        }
    }

    if unauthorized_files > 0 {
        tracing::warn!(
            "Adjudication: the provider API rejected file reads (401/403) for {} cited file(s) — the \
             credential most likely lacks repository read access (GitLab `read_repository` / GitHub \
             contents read); those candidate findings are kept unchanged (fail-open). Grant the token \
             read access to enable adjudication.",
            unauthorized_files
        );
    }

    for (idx, target) in downgrades {
        if let Some(f) = findings.get_mut(idx) {
            tracing::info!(
                "Adjudication: downgraded '{}' ({}:{:?}) from {} to {}",
                f.title,
                f.file,
                f.line,
                f.severity,
                target
            );
            f.severity = target;
        }
    }

    // One line that makes the pass auditable: which source produced the
    // ground truth, how much of it there was, and what the filter did. A
    // production (server-side) pass says `source=provider-api`, not
    // `0 dropped, unadjudicated`.
    tracing::info!(
        "Adjudication: source={} files={} fetches={} dropped={} kept={}",
        source.kind(),
        adjudicated_files,
        cache.len(),
        drop_marks.len(),
        candidate_count.saturating_sub(drop_marks.len())
    );

    if drop_marks.is_empty() {
        return Vec::new();
    }
    let mut dropped = Vec::new();
    let mut kept = Vec::with_capacity(findings.len());
    for (i, finding) in std::mem::take(findings).into_iter().enumerate() {
        match drop_marks.get(&i) {
            Some(reason) => dropped.push(DroppedFinding {
                finding,
                reason: reason.clone(),
            }),
            None => kept.push(finding),
        }
    }
    *findings = kept;
    dropped
}

/// Read the full content of every cited file, once per (path, revision).
///
/// Reads run concurrently, capped at [`MAX_CONCURRENT_FETCHES`], each under
/// [`FETCH_TIMEOUT`], with the phase as a whole under [`FETCH_PHASE_TIMEOUT`].
/// Every requested key gets an entry — a file that was never read (the phase
/// budget ran out) is recorded as a failure, so the caller fails open on it
/// rather than reading a stale or empty file.
async fn prefetch_ground_truth(
    source: &dyn FileSource,
    groups: &[(String, Vec<usize>)],
    revision: &str,
) -> HashMap<(String, String), Result<String, FileReadError>> {
    use futures::StreamExt;

    let mut keys: Vec<(String, String)> = Vec::new();
    for (file, _) in groups {
        let key = (file.clone(), revision.to_string());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    let mut cache: HashMap<(String, String), Result<String, FileReadError>> = HashMap::new();
    let mut stream = futures::stream::iter(keys.iter().cloned())
        .map(|key| async move {
            let fetched = match tokio::time::timeout(FETCH_TIMEOUT, source.read_file(&key.0)).await {
                Ok(result) => result,
                Err(_) => Err(FileReadError::Other(format!(
                    "no response from the file source within {}s",
                    FETCH_TIMEOUT.as_secs()
                ))),
            };
            (key, fetched)
        })
        .buffer_unordered(MAX_CONCURRENT_FETCHES);

    let deadline = tokio::time::Instant::now() + FETCH_PHASE_TIMEOUT;
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(entry)) => {
                cache.insert(entry.0, entry.1);
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    for key in &keys {
        cache.entry(key.clone()).or_insert_with(|| {
            Err(FileReadError::Other(format!(
                "the ground-truth fetch phase exceeded its {}s budget",
                FETCH_PHASE_TIMEOUT.as_secs()
            )))
        });
    }
    cache
}

/// Render fetched full-file content for the adjudication prompt — bypassing
/// the expert-context and verification byte caps. Lines are numbered
/// (` 1234| code`) so verdicts can cite exact lines. Files larger than
/// [`HARD_FILE_CAP_BYTES`] are represented by the cited region
/// (± [`REGION_CONTEXT_LINES`] lines) plus a function-level outline of the
/// rest.
///
/// Formatting is separate from fetching so the raw text can be cached per
/// file and re-rendered per group (the cited line decides how an oversized
/// file is windowed).
fn format_full_file(text: &str, cited_line: Option<u32>) -> String {
    if text.len() <= HARD_FILE_CAP_BYTES {
        return number_lines(text, 1);
    }

    // Oversized file: cited region ± context, plus an outline of the rest.
    let lines: Vec<&str> = text.lines().collect();
    let center = cited_line.unwrap_or(((lines.len() / 2) as u32).max(1)).max(1);
    let start = center.saturating_sub(REGION_CONTEXT_LINES).max(1);
    let end = center.saturating_add(REGION_CONTEXT_LINES).min(lines.len() as u32);
    let mut out = format!(
        "(file exceeds {} bytes; showing lines {}-{} around the cited line, plus an outline of the rest)\n",
        HARD_FILE_CAP_BYTES, start, end,
    );
    let region: String = lines[(start - 1) as usize..end as usize]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>5}| {}", start as usize + i, l))
        .collect::<Vec<_>>()
        .join("\n");
    out.push_str(&region);
    out.push_str("\n\n### Outline of the remaining file (line: item)\n");
    out.push_str(&function_outline(&lines));
    out
}

/// Number every line of `text` starting at `first` (`    1| code`).
fn number_lines(text: &str, first: u32) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| format!("{:>5}| {}", first as usize + i, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Heuristic function/type outline: numbered lines whose trimmed text starts
/// with a definition keyword across common languages.
fn function_outline(lines: &[&str]) -> String {
    const KEYWORDS: &[&str] = &[
        "fn ",
        "pub fn",
        "pub(crate) fn",
        "pub async fn",
        "async fn",
        "impl ",
        "struct ",
        "enum ",
        "trait ",
        "mod ",
        "def ",
        "class ",
        "func ",
        "function ",
        "public ",
        "private ",
        "protected ",
    ];
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            KEYWORDS.iter().any(|k| t.starts_with(k))
        })
        .map(|(i, l)| format!("{}: {}", i + 1, l.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Collapse all whitespace runs so quoted snippets match across rewrapping.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Deterministic pre-filter: locate the finding's quoted evidence in the
/// real file content. Returns a PRE-FILTER NOTE for the prompt when the
/// evidence is absent entirely (fabrication signal) or located far from the
/// cited line; `None` when the evidence checks out or is too short to test.
fn evidence_hint(finding: &Finding, numbered_content: &str) -> Option<String> {
    let evidence = finding.evidence.trim();
    // Strip markdown fences experts tend to wrap around quotes.
    let evidence = evidence.trim_start_matches("```").trim_end_matches("```").trim();
    let normalized = normalize_ws(evidence);
    if normalized.len() < 8 {
        return None;
    }
    // The content carries "   42| " line-number prefixes; normalizing both
    // sides keeps containment checks reliable despite them.
    let content_norm = normalize_ws(numbered_content);
    if !content_norm.contains(&normalized) {
        return Some(format!(
            "PRE-FILTER NOTE: the finding's quoted evidence does NOT appear anywhere in the \
             actual file `{}` — the quote is likely fabricated or from a different file.",
            finding.file
        ));
    }
    // Evidence present: check proximity to the cited line via its first
    // substantive line.
    if let Some(cited) = finding.line {
        if let Some(first_line) = evidence.lines().map(str::trim).find(|l| l.len() >= 8) {
            if let Some(found_line) = numbered_content.lines().find_map(|l| {
                let (num, code) = l.split_once('|')?;
                if code.contains(first_line) {
                    num.trim().parse::<u32>().ok()
                } else {
                    None
                }
            }) {
                if found_line.abs_diff(cited) > EVIDENCE_LINE_TOLERANCE {
                    return Some(format!(
                        "PRE-FILTER NOTE: the quoted evidence appears at line {} of `{}`, far from \
                         the cited line {} — the finding may be anchored to the wrong location.",
                        found_line, finding.file, cited
                    ));
                }
            }
        }
    }
    None
}

/// Parse the adjudicator's YAML verdict list. Tolerates fenced code blocks
/// and surrounding prose; entries with an unusable `index` are skipped and
/// unknown verdict strings fail open to `confirmed`. Returns an error when
/// no verdict list can be extracted at all (caller keeps the batch).
fn parse_verdicts(text: &str) -> anyhow::Result<Vec<Decision>> {
    let cleaned = crate::output::parser::clean_yaml(text);
    let value: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&cleaned) {
        Ok(v) => v,
        Err(e) => {
            let fenced = crate::output::parser::extract_first_fenced_yaml(text)
                .ok_or_else(|| anyhow::anyhow!("adjudication YAML parse failed: {}", e))?;
            serde_yaml_ng::from_str(&fenced).map_err(|e2| anyhow::anyhow!("adjudication YAML parse failed: {}", e2))?
        }
    };

    let items = value
        .get("verdicts")
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("adjudication response has no 'verdicts' list"))?;

    let mut decisions = Vec::with_capacity(items.len());
    for item in items {
        let Some(index) = item.get("index").and_then(|v| v.as_u64()) else {
            continue;
        };
        let verdict = match item
            .get("verdict")
            .and_then(|v| v.as_str())
            .unwrap_or("confirmed")
            .to_ascii_lowercase()
            .as_str()
        {
            "false_positive" | "false-positive" | "drop" => Verdict::FalsePositive,
            "downgrade" => Verdict::Downgrade,
            _ => Verdict::Confirmed,
        };
        let new_severity = item
            .get("new_severity")
            .and_then(|v| v.as_str())
            .and_then(parse_severity_label);
        let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let cited_lines = item
            .get("cited_lines")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        decisions.push(Decision {
            index: index as usize,
            verdict,
            new_severity,
            reason,
            cited_lines,
        });
    }
    Ok(decisions)
}

/// Build the user prompt for one adjudication batch over a single file.
fn build_user_prompt(file: &str, content: &str, batch: &[usize], findings: &[Finding]) -> String {
    let mut out = format!("## File under adjudication: `{}`\n\n", file);
    out.push_str(&format!(
        "### Full current content of `{}` (line-numbered)\n```\n{}\n```\n",
        file,
        content.trim_end()
    ));
    out.push_str("\n## Findings to adjudicate\n");
    for (i, &idx) in batch.iter().enumerate() {
        let finding = &findings[idx];
        let line = match (finding.line, finding.line_end) {
            (Some(l), Some(le)) if le != l => format!("{}-{}", l, le),
            (Some(l), _) => l.to_string(),
            (None, _) => "n/a".to_string(),
        };
        out.push_str(&format!(
            "\n### Finding [{}]\n- expert: {} ({})\n- severity: {}, confidence: {}/10, category: {}\n- location: `{}:{}`\n- title: {}\n- summary: {}\n- quoted evidence: {}\n- impact: {}\n- recommendation: {}\n",
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
        ));
        if let Some(hint) = evidence_hint(finding, content) {
            out.push_str(&format!("- {}\n", hint));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Effort;
    use crate::team::file_source::LocalFileSource;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn make_finding(file: &str, line: Option<u32>, severity: Severity, title: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line,
            line_end: None,
            severity,
            confidence: 8,
            category: "test".to_string(),
            title: title.to_string(),
            summary: "summary".to_string(),
            evidence: String::new(),
            impact: "impact".to_string(),
            recommendation: "rec".to_string(),
            effort: Effort::Small,
            expert_name: "expert".to_string(),
            expert_role: "role".to_string(),
            agrees_with: vec![],
            references: vec![],
        }
    }

    /// Adjudicate against a local checkout, with the LLM call injected.
    async fn adjudicate_local<F, Fut>(
        findings: &mut Vec<Finding>,
        dir: &std::path::Path,
        min_severity: &Severity,
        llm: F,
    ) -> Vec<DroppedFinding>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send,
    {
        let source = LocalFileSource::new(dir);
        adjudicate_with_llm(findings, dir.to_str().unwrap(), Some(&source), min_severity, llm).await
    }

    /// A stand-in for the provider-API source: fixed content per path, a
    /// preset failure per path, and a read counter.
    struct MockProviderSource {
        files: Mutex<HashMap<String, String>>,
        failures: HashMap<String, FileReadError>,
        reads: AtomicUsize,
    }

    impl MockProviderSource {
        fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: Mutex::new(
                    files
                        .iter()
                        .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
                        .collect(),
                ),
                failures: HashMap::new(),
                reads: AtomicUsize::new(0),
            }
        }

        fn failing(files: &[(&str, &str)], failures: &[(&str, FileReadError)]) -> Self {
            let mut source = Self::new(files);
            source.failures = failures.iter().map(|(p, e)| ((*p).to_string(), e.clone())).collect();
            source
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl FileSource for MockProviderSource {
        async fn read_file(&self, path: &str) -> Result<String, FileReadError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = self.failures.get(path) {
                return Err(e.clone());
            }
            match self.files.lock().unwrap().get(path) {
                Some(content) => Ok(content.clone()),
                None => Err(FileReadError::NotFound),
            }
        }

        fn kind(&self) -> &'static str {
            "provider-api"
        }

        fn revision(&self) -> Option<&str> {
            Some("deadbeef")
        }
    }

    /// In-memory `tracing` sink for asserting the pass's own warnings.
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogs;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
    }

    /// Capture this thread's `tracing` output (the tests run on a
    /// current-thread runtime, so the pass's logs land here).
    fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
        let logs = CapturedLogs::default();
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::INFO)
                .finish(),
        );
        (logs, guard)
    }

    // ─── severity helpers ────────────────────────

    #[test]
    fn test_severity_rank_ordering() {
        assert!(severity_rank(&Severity::Critical) > severity_rank(&Severity::High));
        assert!(severity_rank(&Severity::High) > severity_rank(&Severity::Medium));
        assert!(severity_rank(&Severity::Medium) > severity_rank(&Severity::Low));
        assert!(severity_rank(&Severity::Low) > severity_rank(&Severity::Note));
    }

    #[test]
    fn test_parse_min_severity_known_values() {
        assert_eq!(parse_min_severity("critical"), Severity::Critical);
        assert_eq!(parse_min_severity("High"), Severity::High);
        assert_eq!(parse_min_severity(" medium "), Severity::Medium);
        assert_eq!(parse_min_severity("low"), Severity::Low);
        assert_eq!(parse_min_severity("note"), Severity::Note);
    }

    #[test]
    fn test_parse_min_severity_unknown_falls_back_to_high() {
        assert_eq!(parse_min_severity("bogus"), Severity::High);
        assert_eq!(parse_min_severity(""), Severity::High);
    }

    // ─── parse_verdicts ──────────────────────────

    #[test]
    fn test_parse_verdicts_all_kinds() {
        let text = "```yaml\nverdicts:\n  - index: 0\n    verdict: confirmed\n    reason: \"\"\n    cited_lines: \"10-12\"\n  - index: 1\n    verdict: false_positive\n    reason: \"guard exists\"\n    cited_lines: \"1099-1134\"\n  - index: 2\n    verdict: downgrade\n    new_severity: medium\n    reason: \"impact overstated\"\n    cited_lines: \"42\"\n```";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions.len(), 3);
        assert_eq!(decisions[0].verdict, Verdict::Confirmed);
        assert_eq!(decisions[1].verdict, Verdict::FalsePositive);
        assert_eq!(decisions[1].reason, "guard exists");
        assert_eq!(decisions[1].cited_lines, "1099-1134");
        assert_eq!(decisions[2].verdict, Verdict::Downgrade);
        assert_eq!(decisions[2].new_severity, Some(Severity::Medium));
    }

    #[test]
    fn test_parse_verdicts_unknown_verdict_defaults_to_confirmed() {
        let text = "verdicts:\n  - index: 0\n    verdict: maybe\n";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions[0].verdict, Verdict::Confirmed);
        assert!(decisions[0].new_severity.is_none());
    }

    #[test]
    fn test_parse_verdicts_malformed_errors() {
        assert!(parse_verdicts("!!! not yaml at all !!!").is_err());
        assert!(parse_verdicts("summary: \"no verdicts\"\n").is_err());
    }

    #[test]
    fn test_parse_verdicts_invalid_new_severity_is_none() {
        let text = "verdicts:\n  - index: 0\n    verdict: downgrade\n    new_severity: catastrophic\n";
        let decisions = parse_verdicts(text).unwrap();
        assert_eq!(decisions[0].verdict, Verdict::Downgrade);
        assert!(decisions[0].new_severity.is_none());
    }

    // ─── ground-truth fetching + formatting ──────

    /// Reproduce what the pass does for one local file: read through the
    /// source, then render for the prompt.
    async fn load_full_file(project_path: &str, file: &str, cited_line: Option<u32>) -> Result<String, String> {
        let source = LocalFileSource::new(project_path);
        source
            .read_file(file)
            .await
            .map(|text| format_full_file(&text, cited_line))
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn test_load_full_file_bypasses_twenty_kb_cap() {
        // A file well above the 20KB expert-context cap must be delivered in
        // full — this is the core fix for hidden defensive code. 1500 lines
        // at ~20 bytes each ≈ 30KB on disk, comfortably over the cap.
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=1500)
            .map(|i| format!("let line_{} = {};", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.len() > 20_000);
        std::fs::write(dir.path().join("big.rs"), &body).unwrap();

        let content = load_full_file(dir.path().to_str().unwrap(), "big.rs", None)
            .await
            .unwrap();
        assert!(content.contains("let line_1 = 1;"));
        assert!(content.contains("let line_1500 = 1500;"));
        assert!(!content.contains("Outline of the remaining"));
    }

    #[tokio::test]
    async fn test_load_full_file_hard_cap_gives_region_plus_outline() {
        let dir = tempfile::tempdir().unwrap();
        // Build a >200KB file: ~9000 numbered lines with some `fn` markers.
        let mut lines = Vec::new();
        for i in 1..=9000u32 {
            if i % 500 == 0 {
                lines.push(format!("fn handler_{}() {{", i));
            } else {
                lines.push(format!("    let value_{} = {}; // padding padding padding", i, i));
            }
        }
        let body = lines.join("\n");
        assert!(body.len() > HARD_FILE_CAP_BYTES);
        std::fs::write(dir.path().join("huge.rs"), &body).unwrap();

        let content = load_full_file(dir.path().to_str().unwrap(), "huge.rs", Some(4500))
            .await
            .unwrap();
        // Region around the cited line, numbered.
        assert!(content.contains("4500| fn handler_4500()"));
        assert!(content.contains("Outline of the remaining"));
        assert!(content.contains("fn handler_500()"));
        assert!(content.contains("fn handler_9000()"));
        // Far-away body lines are NOT inlined in full.
        assert!(!content.contains("let value_100 = 100;"));
    }

    #[tokio::test]
    async fn test_load_full_file_unreadable_or_escaping() {
        assert!(load_full_file("/nonexistent", "a.rs", None).await.is_err());
        assert!(load_full_file("/tmp", "../secret", None).await.is_err());
        assert!(load_full_file("/tmp", "/etc/passwd", None).await.is_err());
    }

    // ─── evidence_hint ───────────────────────────

    #[test]
    fn test_evidence_hint_absent_evidence_flagged() {
        let mut f = make_finding("src/a.rs", Some(3), Severity::High, "x");
        f.evidence = "if (freshItems.length !== remaining.length) throw".to_string();
        let content = number_lines("fn main() {}\nfn other() {}", 1);
        let hint = evidence_hint(&f, &content).unwrap();
        assert!(hint.contains("PRE-FILTER NOTE"));
        assert!(hint.contains("does NOT appear"));
    }

    #[test]
    fn test_evidence_hint_present_near_cited_line_is_quiet() {
        let mut f = make_finding("src/a.rs", Some(2), Severity::High, "x");
        f.evidence = "let guard = check();".to_string();
        let content = number_lines("fn main() {\n    let guard = check();\n}", 1);
        assert!(evidence_hint(&f, &content).is_none());
    }

    #[test]
    fn test_evidence_hint_present_but_far_flagged() {
        let mut f = make_finding("src/a.rs", Some(1), Severity::High, "x");
        f.evidence = "let defensive_guard = true;".to_string();
        let mut lines = vec!["fn top() {".to_string()];
        for i in 0..120 {
            lines.push(format!("    let pad_{} = {};", i, i));
        }
        lines.push("    let defensive_guard = true;".to_string());
        let content = number_lines(&lines.join("\n"), 1);
        let hint = evidence_hint(&f, &content).unwrap();
        assert!(hint.contains("far from the cited line"));
    }

    #[test]
    fn test_evidence_hint_trivial_evidence_ignored() {
        let mut f = make_finding("src/a.rs", Some(1), Severity::High, "x");
        f.evidence = "}".to_string();
        let content = number_lines("fn main() {}", 1);
        assert!(evidence_hint(&f, &content).is_none());
    }

    // ─── adjudicate_with_llm ─────────────────────

    #[tokio::test]
    async fn test_adjudicate_drops_false_positive_with_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn guard() { assert!(x); }").unwrap();
        let mut findings = vec![
            make_finding("a.rs", Some(1), Severity::Critical, "hallucinated claim"),
            make_finding("a.rs", Some(1), Severity::High, "real bug"),
        ];
        let llm = |_u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: false_positive\n    reason: \"guard is present\"\n    cited_lines: \"1\"\n  - index: 1\n    verdict: confirmed\n    reason: \"\"\n    cited_lines: \"1\"\n".to_string())
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].title, "real bug");
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].finding.title, "hallucinated claim");
        assert!(dropped[0].reason.contains("guard is present"));
        assert!(dropped[0].reason.contains("a.rs:1"));
    }

    #[tokio::test]
    async fn test_adjudicate_downgrade_updates_severity_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {}").unwrap();
        let mut findings = vec![make_finding("a.rs", Some(1), Severity::Critical, "overstated")];
        let llm = |_u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: downgrade\n    new_severity: low\n    reason: \"unlikely\"\n    cited_lines: \"1\"\n".to_string())
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Low);
    }

    #[tokio::test]
    async fn test_adjudicate_invalid_downgrade_keeps_finding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {}").unwrap();
        // Downgrade to a HIGHER severity must be rejected.
        let mut findings = vec![make_finding("a.rs", Some(1), Severity::High, "bug")];
        let llm = |_u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: downgrade\n    new_severity: critical\n    reason: \"worse\"\n    cited_lines: \"1\"\n".to_string())
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings[0].severity, Severity::High);
    }

    #[tokio::test]
    async fn test_adjudicate_fail_open_on_llm_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {}").unwrap();
        let mut findings = vec![make_finding("a.rs", Some(1), Severity::Critical, "bug")];
        let llm = |_u: String| async { anyhow::bail!("network down") };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Critical);
    }

    #[tokio::test]
    async fn test_adjudicate_fail_open_on_parse_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {}").unwrap();
        let mut findings = vec![make_finding("a.rs", Some(1), Severity::Critical, "bug")];
        let llm = |_u: String| async { Ok("total garbage, no yaml".to_string()) };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 1);
    }

    #[tokio::test]
    async fn test_adjudicate_skips_unreadable_file_keeps_all() {
        let mut findings = vec![make_finding("missing.rs", Some(1), Severity::Critical, "bug")];
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = adjudicate_with_llm(&mut findings, "/nonexistent", None, &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no LLM call without ground truth");
    }

    /// Regression (RENG-25, still true after RENG-31): when the review has
    /// neither a local checkout nor a provider file source, the pass must
    /// skip explicitly — no LLM calls, every candidate kept unchanged, one
    /// WARN that says so — instead of per-file "not readable" noise followed
    /// by a summary claiming the findings were examined.
    #[tokio::test]
    async fn test_adjudicate_no_local_checkout_slug_skips_all_candidates() {
        // Hermetic stand-in for a provider slug: a path that is not a
        // directory, as `group/project` is not on the server's filesystem.
        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("group/project");
        let slug = slug.to_str().unwrap();
        assert!(!std::path::Path::new(slug).is_dir());

        let mut findings = vec![
            make_finding("README.md", Some(1), Severity::Critical, "doc claim"),
            make_finding("src/a.rs", Some(10), Severity::High, "bug A"),
            make_finding("src/b.rs", Some(20), Severity::High, "bug B"),
            make_finding("src/c.rs", Some(30), Severity::Medium, "below threshold"),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Ok("verdicts: []".to_string()) }
        };

        let (logs, _guard) = capture_logs();
        let dropped = adjudicate_with_llm(&mut findings, slug, None, &Severity::High, llm).await;

        assert!(dropped.is_empty(), "fail-open: nothing dropped without ground truth");
        assert_eq!(findings.len(), 4, "all findings kept unchanged");
        assert_eq!(findings[0].severity, Severity::Critical);
        assert_eq!(findings[1].severity, Severity::High);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no LLM call when no local checkout exists"
        );
        let logged = logs.text();
        assert!(logged.contains("no file source"), "{logged}");
        assert!(logged.contains("3 candidate finding(s)"), "{logged}");
        assert!(logged.contains("UNADJUDICATED"), "{logged}");
    }

    /// A real checkout where one cited file is missing (e.g. deleted by the
    /// MR): that file's group is skipped fail-open, but files that DO exist
    /// on disk are still adjudicated normally.
    #[tokio::test]
    async fn test_adjudicate_real_checkout_missing_file_skips_only_that_group() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("present.rs"), "fn f() {}").unwrap();
        let mut findings = vec![
            make_finding("deleted.rs", Some(1), Severity::Critical, "on deleted file"),
            make_finding("present.rs", Some(1), Severity::High, "on present file"),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async {
                Ok("verdicts:\n  - index: 0\n    verdict: downgrade\n    new_severity: low\n    reason: \"minor\"\n    cited_lines: \"1\"\n".to_string())
            }
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 2);
        // Missing file: kept untouched.
        assert_eq!(findings[0].severity, Severity::Critical);
        // Present file: adjudicated normally (downgrade applied).
        assert_eq!(findings[1].severity, Severity::Low);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the readable file's batch calls the LLM"
        );
    }

    #[tokio::test]
    async fn test_adjudicate_below_threshold_untouched_no_llm_call() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {}").unwrap();
        let mut findings = vec![
            make_finding("a.rs", Some(1), Severity::Medium, "minor"),
            make_finding("a.rs", Some(1), Severity::Low, "nit"),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_adjudicate_prompt_contains_full_content_and_hint() {
        let dir = tempfile::tempdir().unwrap();
        // 30KB file: over the expert-context cap, delivered in full here.
        let body: String = (1..=1200)
            .map(|i| format!("let line_{} = {};", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.len() > 20_000);
        std::fs::write(dir.path().join("big.rs"), &body).unwrap();

        let mut f = make_finding("big.rs", Some(600), Severity::High, "claim");
        f.evidence = "definitely_not_in_the_file()".to_string();
        let mut findings = vec![f];

        let prompts = Arc::new(Mutex::new(Vec::new()));
        let prompts2 = prompts.clone();
        let llm = move |user: String| {
            prompts2.lock().unwrap().push(user);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        let prompts = prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        let p = &prompts[0];
        // Full content beyond the 20KB cap is present, line-numbered.
        assert!(p.contains("let line_1200 = 1200;"));
        assert!(p.contains("Full current content of `big.rs`"));
        // Pre-filter note about the absent evidence is attached.
        assert!(p.contains("PRE-FILTER NOTE"));
        assert!(p.contains("does NOT appear"));
    }

    #[tokio::test]
    async fn test_adjudicate_batches_per_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn f() {}").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn g() {}").unwrap();
        let mut findings: Vec<Finding> = (0..6)
            .map(|i| make_finding("a.rs", Some(1), Severity::High, &format!("A{}", i)))
            .collect();
        findings.push(make_finding("b.rs", Some(1), Severity::Critical, "B0"));

        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Ok("verdicts: []".to_string()) }
        };

        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 7);
        // 6 findings in a.rs → 2 batches; 1 in b.rs → 1 batch.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    // ─── RENG-31: server-side ground truth ───────

    /// The point of the ticket: a review with NO checkout at all — a provider
    /// slug for `project_path`, the file read through the provider source —
    /// now adjudicates. The claim the guard disproves is dropped; the valid
    /// one is kept, and the pass says which source produced the ground truth.
    #[tokio::test]
    async fn test_adjudicate_with_provider_source_drops_false_positive_without_checkout() {
        let dir = tempfile::tempdir().unwrap();
        // A provider slug is not a directory: without the provider source this
        // pass used to bail out and drop nothing.
        let slug = dir.path().join("group/project");
        let slug = slug.to_str().unwrap().to_string();
        assert!(!std::path::Path::new(&slug).is_dir());

        let source = MockProviderSource::new(&[("src/a.rs", "fn guard() {\n    assert!(x);\n}\n")]);
        let mut findings = vec![
            make_finding("src/a.rs", Some(1), Severity::Critical, "hallucinated: no guard"),
            make_finding("src/a.rs", Some(2), Severity::High, "real bug"),
        ];
        let llm = |_u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: false_positive\n    reason: \"the guard is present\"\n    cited_lines: \"2\"\n  - index: 1\n    verdict: confirmed\n    reason: \"\"\n    cited_lines: \"2\"\n".to_string())
        };

        let (logs, _guard) = capture_logs();
        let dropped = adjudicate_with_llm(&mut findings, &slug, Some(&source), &Severity::High, llm).await;

        assert_eq!(dropped.len(), 1, "the disproved claim is dropped");
        assert_eq!(dropped[0].finding.title, "hallucinated: no guard");
        assert!(dropped[0].reason.contains("guard is present"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].title, "real bug");

        let logged = logs.text();
        assert!(logged.contains("Adjudication: source=provider-api"), "{logged}");
        assert!(logged.contains("files=1"), "{logged}");
        assert!(logged.contains("dropped=1"), "{logged}");
    }

    /// The same pass driven through the real provider client against a mock
    /// GitLab: the fetch goes to the reviewed SHA and the filter drops the
    /// finding the fetched file disproves.
    #[tokio::test]
    async fn test_adjudicate_over_the_gitlab_api_without_a_checkout() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v4/projects/group%2Fproject/repository/files/src%2Fguard.rs/raw",
            ))
            .and(query_param("ref", "headsha"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fn guard() {\n    assert!(x);\n}\n"))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("group/project").to_str().unwrap().to_string();
        let mr_url = format!("{}/group/project/-/merge_requests/26", server.uri());
        let source = crate::team::file_source::ProviderFileSource::gitlab("token", &mr_url, "headsha").unwrap();

        let mut findings = vec![
            make_finding("src/guard.rs", Some(1), Severity::High, "CRITICAL 10/10: no guard"),
            make_finding("src/guard.rs", Some(3), Severity::High, "unreachable branch"),
        ];
        let llm = |_u: String| async {
            Ok("verdicts:\n  - index: 0\n    verdict: false_positive\n    reason: \"`assert!(x)` is right there\"\n    cited_lines: \"2\"\n  - index: 1\n    verdict: confirmed\n    reason: \"\"\n    cited_lines: \"3\"\n".to_string())
        };

        let dropped = adjudicate_with_llm(&mut findings, &slug, Some(&source), &Severity::High, llm).await;

        assert_eq!(dropped.len(), 1);
        assert_eq!(findings.len(), 1);
        // `expect(1)` above fails the test if the file was read twice.
    }

    #[tokio::test]
    async fn test_adjudicate_provider_failure_fails_open_and_warns() {
        for (failure, expected) in [
            (FileReadError::Other("boom".to_string()), "skipping 'src/a.rs': boom"),
            (FileReadError::NotFound, "not present at the reviewed revision (404)"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let slug = dir.path().join("group/project").to_str().unwrap().to_string();
            let source = MockProviderSource::failing(&[], &[("src/a.rs", failure)]);
            let mut findings = vec![make_finding("src/a.rs", Some(1), Severity::Critical, "bug")];
            let calls = Arc::new(AtomicUsize::new(0));
            let calls2 = calls.clone();
            let llm = move |_u: String| {
                calls2.fetch_add(1, Ordering::SeqCst);
                async { Ok("verdicts: []".to_string()) }
            };

            let (logs, _guard) = capture_logs();
            let dropped = adjudicate_with_llm(&mut findings, &slug, Some(&source), &Severity::High, llm).await;

            assert!(dropped.is_empty(), "fail-open");
            assert_eq!(findings.len(), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "no LLM call without ground truth");
            assert!(findings[0].severity == Severity::Critical);
            let logged = logs.text();
            assert!(logged.contains(expected), "{logged}");
            assert!(logged.contains("fail-open"), "{logged}");
        }
    }

    /// 401/403 is a credential-level failure: one WARN for the pass, naming
    /// the read permission, never one per cited file.
    #[tokio::test]
    async fn test_adjudicate_unauthorized_source_warns_once_and_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("group/project").to_str().unwrap().to_string();
        let denied = FileReadError::Unauthorized("GitLab API returned 401 Unauthorized: denied".to_string());
        let source = MockProviderSource::failing(&[], &[("src/a.rs", denied.clone()), ("src/b.rs", denied)]);

        let mut findings = vec![
            make_finding("src/a.rs", Some(1), Severity::Critical, "A"),
            make_finding("src/b.rs", Some(2), Severity::High, "B"),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let llm = move |_u: String| {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Ok("verdicts: []".to_string()) }
        };

        let (logs, _guard) = capture_logs();
        let dropped = adjudicate_with_llm(&mut findings, &slug, Some(&source), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(findings.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let logged = logs.text();
        assert_eq!(
            logged.matches("rejected file reads (401/403)").count(),
            1,
            "one credential warning for the whole pass: {logged}"
        );
        assert!(logged.contains("read_repository"), "{logged}");
        assert_eq!(
            logged.matches("Adjudication: skipping").count(),
            0,
            "no per-file noise for a credential problem: {logged}"
        );
    }

    /// A finding-dense review cites the same file from many findings and
    /// batches; the pass reads each (path, revision) once. A failed read is
    /// not retried either.
    #[tokio::test]
    async fn test_adjudicate_fetches_each_file_once_per_pass() {
        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("group/project").to_str().unwrap().to_string();
        let source = MockProviderSource::failing(
            &[("a.rs", "fn a() {}"), ("b.rs", "fn b() {}")],
            &[("gone.rs", FileReadError::NotFound)],
        );

        let mut findings: Vec<Finding> = (0..7)
            .map(|i| make_finding("a.rs", Some(1), Severity::High, &format!("A{i}")))
            .collect();
        findings.push(make_finding("b.rs", Some(1), Severity::High, "B0"));
        findings.push(make_finding("gone.rs", Some(1), Severity::High, "G0"));
        findings.push(make_finding("gone.rs", Some(2), Severity::High, "G1"));
        let llm = |_u: String| async { Ok("verdicts: []".to_string()) };

        let dropped = adjudicate_with_llm(&mut findings, &slug, Some(&source), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        assert_eq!(
            source.reads(),
            3,
            "one read per cited file (7+1 findings on a.rs span 2 batches; gone.rs is cited twice)"
        );
    }

    /// The local path is unchanged in kind and behaviour — the cache just
    /// makes the per-file count explicit.
    #[tokio::test]
    async fn test_adjudicate_local_source_reads_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() {}").unwrap();
        let mut findings: Vec<Finding> = (0..7)
            .map(|i| make_finding("a.rs", Some(1), Severity::High, &format!("A{i}")))
            .collect();
        findings.push(make_finding("b.rs", Some(1), Severity::High, "B0"));
        let llm = |_u: String| async { Ok("verdicts: []".to_string()) };

        let (logs, _guard) = capture_logs();
        let dropped = adjudicate_local(&mut findings, dir.path(), &Severity::High, llm).await;

        assert!(dropped.is_empty());
        let logged = logs.text();
        assert!(
            logged.contains("Adjudication: source=local files=2 fetches=2 dropped=0 kept=8"),
            "{logged}"
        );
    }
}
