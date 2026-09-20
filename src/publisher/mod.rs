//! Publishing helper functions for review results on Git providers.
//!
//! This module provides the [`InlineNote`] struct and helper functions that
//! operate on a [`GitProvider`][crate::git_provider::GitProvider] to format and
//! publish review output. Platform-specific logic lives in the
//! `git_provider` implementations; this module only contains generic helpers
//! such as inline-note publishing and suggestion formatting.
//!
//! Since RENG-63 the decision *which* findings deserve an inline note is the
//! [`PublishPolicy`]'s ([`policy`]): a severity floor, a confidence floor an
//! actionability requirement, a per-round cap, and summary-only publishing for
//! documentation/CI-only changes. [`plan_inline_notes`] turns the policy into
//! an [`InlinePlan`] before anything is posted, so the board can describe the
//! decision ([`board::render_board`]) and the poster only carries it out.

use anyhow::Result;

mod board;
mod policy;

pub use board::render_board;
pub use policy::{
    is_docs_or_ci_only, is_docs_or_ci_path, plan_inline_notes, rank_inline_candidates, InlinePlan, PublishPolicy,
    ENV_INLINE_ON_DOCS_ONLY, ENV_MAX_INLINE_NOTES, ENV_MIN_CONFIDENCE, ENV_MIN_SEVERITY,
};

use crate::git_provider::InlineAnchor;

/// Fixed header of the review report this service posts to the MR
/// (`publish_review`, lib.rs). The Note-hook ingestion path skips notes
/// starting with this prefix — self-echo guard (a) of
/// design/persistence.md §7.1, so our own report never re-enters the
/// discussion history it was published into.
pub const REVIEW_REPORT_PREFIX: &str = "# CodeReview Board\n\n";

/// A note to be posted on a specific line of a file in a merge request.
#[derive(Debug, Clone)]
pub struct InlineNote {
    /// Relative file path where the note should appear.
    pub file: String,
    /// Line number in the new (head) version of the file.
    pub line: u32,
    /// Markdown body of the inline comment.
    pub body: String,
}

/// Maximum number of POST attempts for one inline note (initial try + retries).
const INLINE_POST_MAX_ATTEMPTS: u32 = 3;

/// Base backoff between retry attempts; doubled after every further failure.
const INLINE_POST_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

/// Outcome of one inline-publish pass.
///
/// Returned instead of an error so a batch can finish: individual failures are
/// counted here, logged, and reported by the caller — never propagated out of
/// the per-finding loop. The counters are the accounting the publish path logs
/// and the RENG-59/60 work was measured with; RENG-63 adds `policy_excluded`
/// and `rolled_up` so "why was this finding not posted" has an answer distinct
/// from "the provider refused it", and RENG-99 adds `failures` so the refused
/// notes survive this struct instead of ending as one WARN per note.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PublishSummary {
    /// Findings inspected.
    pub considered: usize,
    /// Notes the provider accepted.
    pub posted: usize,
    /// Findings below a delivery threshold (severity, confidence,
    /// actionability) — board only, never inline. See [`PublishPolicy`].
    pub policy_excluded: usize,
    /// Findings admitted by the policy that were withheld from inline delivery:
    /// dropped behind the per-round cap, or withheld entirely because the round
    /// is summary-only (documentation/CI-only change). Board only.
    pub rolled_up: usize,
    /// Findings that passed the policy but carry no line number, so they can
    /// never carry an anchor.
    pub not_eligible: usize,
    /// Findings dropped by a gate: unsafe path, or a line that is not part of
    /// the reviewed diff.
    pub skipped: usize,
    /// Notes the provider rejected (permanent 4xx verdict, or retries spent).
    /// Equal to `failures.len()`.
    pub failed: usize,
    /// One entry per rejected note, in the order the batch attempted them —
    /// what [`render_inline_failure_section`] puts in front of the user.
    pub failures: Vec<InlineNoteFailure>,
    /// True when the round was published summary-only (documentation/CI-only
    /// change): the board was updated and no inline note was posted at all.
    pub summary_only: bool,
}

/// One inline note the provider refused.
///
/// The `PublishSummary` counters said *how many* notes were lost; this says
/// *which*, and why. Without it a round that posted 3 of 4 candidate notes left
/// one WARN in the log and nothing in anything the user reads — the "有结果但
/// 评论没回写到 GitLab" complaint (RENG-99, and RENG-77 §4 for the same failure
/// one layer further out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineNoteFailure {
    /// `file:line` (or `file:start-end`) of the finding that could not be posted.
    pub anchor: String,
    /// HTTP status of the provider's verdict, when there was one.
    pub status: Option<u16>,
    /// The provider's cause, truncated to one line.
    pub detail: String,
}

/// The changed lines of the reviewed diff, keyed by file path.
///
/// Used to keep inline notes inside the diff: a finding whose line is not in
/// any changed hunk has no anchor the provider will accept, so posting it
/// earns a `400 ... line_code can't be blank` instead of a comment (corpus
/// §4.6 — five such rejections in four hours).
///
/// Since RENG-99 the index also remembers *how* the diff presents each new-side
/// line, because that decides which numbers the anchor must carry: an added line
/// is addressed by `new_line` alone, a line the diff leaves unchanged by
/// `(old_line, new_line)` together. The old-side number is not a detail GitLab
/// can supply on its own — it matches the position's pair against its own diff,
/// so an anchor that omits it is rejected exactly like an anchor outside the
/// diff.
#[derive(Debug, Clone, Default)]
pub struct DiffIndex {
    /// New-side line runs per file that has one, ascending.
    files: std::collections::HashMap<String, Vec<LineRun>>,
    changed_files: Vec<String>,
}

/// One contiguous run of new-side lines that share an anchor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineRun {
    /// First new-side line of the run.
    new_start: u32,
    /// Last new-side line of the run, inclusive.
    new_end: u32,
    /// `old_line - new_line` for every line of the run; `None` for a run of
    /// added lines, whose old side has no number.
    ///
    /// Constant within a context run, so the old-side number of the run's line
    /// `n` is `n as i64 + old_minus_new`.
    old_minus_new: Option<i64>,
}

impl DiffIndex {
    /// Build the index from a unified diff.
    ///
    /// Files whose hunks change nothing on the new side (pure deletions) carry
    /// no anchor line but are still recorded in [`Self::changed_files`].
    pub fn from_diff(diff_text: &str) -> Self {
        let mut files = std::collections::HashMap::new();
        let mut changed_files = std::collections::BTreeSet::new();
        for file in crate::diff::parser::parse_unified_diff(diff_text) {
            changed_files.insert(file.path.clone());
            let runs = line_runs(&file);
            if !runs.is_empty() {
                files.insert(file.path, runs);
            }
        }
        Self {
            files,
            changed_files: changed_files.into_iter().collect(),
        }
    }

    /// True when nothing could be indexed (empty or oversized diff). Callers
    /// read that as "no index available", never as "nothing is in the diff".
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Every file the diff touches, in path order — including pure deletions.
    ///
    /// Feeds the documentation/CI-only detection ([`is_docs_or_ci_only`]): a
    /// round that only deletes lines from `AGENTS.md` is still a docs-only
    /// round even though it has no anchorable line.
    pub fn changed_files(&self) -> &[String] {
        &self.changed_files
    }

    /// The run of new-side lines containing `line`, if any.
    fn run_for(&self, file: &str, line: u32) -> Option<&LineRun> {
        self.files
            .get(file)?
            .iter()
            .find(|run| line >= run.new_start && line <= run.new_end)
    }

    /// Whether `line` (1-based, new side) lies inside a changed hunk of `file`.
    pub fn contains(&self, file: &str, line: u32) -> bool {
        self.run_for(file, line).is_some()
    }

    /// The anchor to submit for `file:line`.
    ///
    /// A line the diff adds becomes `new_line` alone — GitLab's added-line
    /// contract, and the anchor the publisher submitted before RENG-99. A line
    /// the diff leaves unchanged becomes `(old_line, new_line)`, which is the
    /// pair GitLab matches on.
    ///
    /// `None` when the line is outside every hunk of `file`, which is the same
    /// condition [`Self::contains`] gates on.
    pub fn anchor_for(&self, file: &str, line: u32) -> Option<InlineAnchor> {
        let run = self.run_for(file, line)?;
        let anchor = InlineAnchor::new(file, line);
        Some(match run.old_minus_new {
            None => anchor,
            Some(old_minus_new) => match line as i64 + old_minus_new {
                // Defensive: an old-side number below 1 (a malformed hunk
                // header) is not a line number, so the added-line shape — the
                // one shape that needs no old side — is the honest fallback.
                old_line if old_line < 1 => anchor,
                old_line => anchor.with_old_line(old_line as u32),
            },
        })
    }
}

/// The new-side line runs of one file's hunks.
///
/// Walks each hunk body the way a unified diff is read: `+` advances the new
/// side only, `-` the old side only, a context line advances both, and the
/// `\ No newline at end of file` marker advances neither (it is glued to the
/// line above it and carries no position of its own).
fn line_runs(file: &crate::models::DiffFile) -> Vec<LineRun> {
    let mut runs: Vec<LineRun> = Vec::new();
    for hunk in &file.hunks {
        let mut old_line = hunk.old_start as i64;
        let mut new_line = hunk.new_start as i64;
        for line in &hunk.lines {
            match line.content.as_bytes().first() {
                Some(b'+') => {
                    push_run(&mut runs, new_line, None);
                    new_line += 1;
                }
                Some(b'-') => old_line += 1,
                Some(b'\\') => {}
                _ => {
                    push_run(&mut runs, new_line, Some(old_line - new_line));
                    old_line += 1;
                    new_line += 1;
                }
            }
        }
    }
    runs
}

/// Append new-side line `new_line` (with its `old_line - new_line` offset) to
/// `runs`, extending the previous run when it is the same shape and contiguous.
fn push_run(runs: &mut Vec<LineRun>, new_line: i64, old_minus_new: Option<i64>) {
    if new_line < 1 {
        return;
    }
    let new_line = new_line as u32;
    if let Some(last) = runs.last_mut() {
        if last.new_end.saturating_add(1) == new_line && last.old_minus_new == old_minus_new {
            last.new_end = new_line;
            return;
        }
    }
    runs.push(LineRun {
        new_start: new_line,
        new_end: new_line,
        old_minus_new,
    });
}

/// `file:line` (or `file:start-end`) anchor of a finding.
pub fn inline_anchor(finding: &crate::models::Finding) -> String {
    match (finding.line, finding.line_end) {
        (Some(start), Some(end)) if end > start => format!("{}:{start}-{end}", finding.file),
        (Some(start), _) => format!("{}:{start}", finding.file),
        (None, _) => finding.file.clone(),
    }
}

/// Whether a finding's file path is safe to embed in a provider API call.
///
/// Defensive: a path outside the repository (`..`, absolute, home-relative) or
/// one carrying a NUL byte is never a valid anchor and must not reach the
/// provider.
pub fn is_safe_inline_path(path: &str) -> bool {
    !(path.contains("..") || path.starts_with('/') || path.starts_with('~') || path.contains('\0'))
}

/// Render the body of an inline note.
///
/// The body carries an explicit file anchor. The pre-0.10.13 body
/// interpolated expert, title, confidence and recommendation only, so two
/// findings that differed just in `file` rendered byte-identical notes (the
/// `10737`/`10738` pair on MR !26, posted 2 s apart) and a reader could not
/// tell which path a note anchored to.
pub fn format_inline_body(finding: &crate::models::Finding) -> String {
    format!(
        "**[{}]** {} (Confidence: {}/10)\n\n`{}`\n\n{}",
        finding.expert_name,
        finding.title,
        finding.confidence,
        inline_anchor(finding),
        finding.recommendation,
    )
}

/// The finding set an inline publish pass draws from.
///
/// The **consolidated** set (post-dedup, post-adjudication) is the source of
/// truth: dedup and adjudication mutate `consolidated.findings` only, so
/// posting the raw per-expert lists is what left both anti-noise layers inert
/// in production — `duplicates_merged` summed to 1 across 140 reviews while 32
/// duplicate notes were posted (corpus §4.1). The raw reports remain a
/// fallback for outputs that were never consolidated (describe/improve/
/// changelog, whose single report carries no findings).
fn inline_publish_set(output: &crate::models::ReviewOutput) -> Vec<&crate::models::Finding> {
    match &output.consolidated {
        Some(consolidated) => consolidated.findings.iter().collect(),
        None => output.reports.iter().flat_map(|r| r.findings.iter()).collect(),
    }
}

/// Whether the output carries any finding the policy would admit inline.
///
/// Lets the caller skip the diff fetch on a publish that has nothing to gate.
pub fn has_inline_candidates(output: &crate::models::ReviewOutput, policy: &PublishPolicy) -> bool {
    inline_publish_set(output)
        .iter()
        .any(|finding| policy.admits(finding) && finding.line.is_some())
}

/// Plan the inline notes of a review output.
///
/// Draws from [`inline_publish_set`] — the consolidated findings — and applies
/// `docs_only` (see [`is_docs_or_ci_only`]) plus the policy's thresholds, cap
/// and summary-only mode. Pure: no provider, no I/O, so the same
/// output/diff/policy always yields the same plan.
pub fn plan_inline_notes_for_output<'a>(
    output: &'a crate::models::ReviewOutput,
    diff: Option<&'a DiffIndex>,
    docs_only: bool,
    policy: &PublishPolicy,
) -> InlinePlan<'a> {
    if output.consolidated.is_none() {
        tracing::warn!(
            "No consolidated report for this output — publishing inline notes from the raw per-expert findings"
        );
    }
    plan_inline_notes(inline_publish_set(output), diff, docs_only, policy)
}

/// Post the inline notes of a plan, counting every outcome.
///
/// The plan has already applied the policy, the cap and the anchor gate, so
/// this only renders and posts — one finding at a time, with the per-finding
/// failure isolation RENG-60 introduced (a rejected anchor or a spent retry is
/// logged and the batch continues) and bounded retries for transient failures.
pub async fn publish_planned_inline_notes(
    provider: &dyn crate::git_provider::GitProvider,
    plan: &InlinePlan<'_>,
) -> PublishSummary {
    let mut summary = PublishSummary {
        considered: plan.considered,
        policy_excluded: plan.policy_excluded,
        rolled_up: plan.rolled_up.len(),
        not_eligible: plan.not_eligible,
        skipped: plan.skipped,
        summary_only: plan.docs_only,
        ..Default::default()
    };

    for finding in &plan.selected {
        // The plan only selects findings the anchor gate admitted; this guard is
        // defensive (and keeps the accounting honest if that invariant ever
        // breaks).
        let Some(anchor) = plan.anchor_for(finding) else {
            tracing::warn!(file = %finding.file, "Inline note selected without a usable anchor — skipping");
            summary.not_eligible += 1;
            continue;
        };
        let body = format_inline_body(finding);
        match post_inline_with_retry(provider, &anchor, &body).await {
            Ok(()) => summary.posted += 1,
            Err(err) => {
                summary.failed += 1;
                let (status, detail) = inline_note_failure_detail(&err);
                // One WARN per failure. The cause goes into the *message*, not
                // only into the fields: the Logs page keeps `fields.message`
                // and drops everything else (log_collector::parse_line), so a
                // fields-only report is invisible where the operator looks.
                //
                // The message keeps its pre-RENG-99 shape for an added-line
                // anchor and only gains the old-side number when there is one,
                // so a rejected context-line anchor is distinguishable from a
                // rejected added-line one in the log.
                let old_line = anchor
                    .old_line
                    .map_or_else(String::new, |old| format!(" old_line={old}"));
                tracing::warn!(
                    file = %finding.file,
                    line = anchor.line,
                    old_line = anchor.old_line,
                    status = status,
                    "Inline note failed — continuing with the remaining findings: \
                     finding={} new_path={} new_line={} status={} error={}{}",
                    inline_anchor(finding),
                    finding.file,
                    anchor.line,
                    status.map_or_else(|| "none".to_string(), |code| code.to_string()),
                    detail,
                    old_line,
                );
                // The same facts, kept where a reader of the review — not a
                // reader of the log — can see them: a log line is not a report
                // (RENG-99, RENG-77 §4).
                summary.failures.push(InlineNoteFailure {
                    anchor: inline_anchor(finding),
                    status,
                    detail,
                });
            }
        }
    }

    tracing::info!(
        "Inline notes: {} posted, {} rolled up (board only), {} policy-excluded, {} anchor-ineligible, \
         {} skipped, {} failed ({} findings considered){}",
        summary.posted,
        summary.rolled_up,
        summary.policy_excluded,
        summary.not_eligible,
        summary.skipped,
        summary.failed,
        summary.considered,
        if summary.summary_only {
            " — documentation/CI-only change: summary only, no inline notes"
        } else {
            ""
        },
    );
    summary
}

/// The user-facing statement that inline notes could not be published, for the
/// review report; empty when every admitted note was posted.
///
/// The ticket's requirement (RENG-99) is the first line — "有 N 条行内评论未能
/// 发布" — because the pre-fix behaviour left the loss to one `WARN` that the
/// user never reads. The per-anchor lines are the RENG-71 diagnosis carried
/// where it is useful, and they cover **both** ways a note can be lost: one the
/// provider refused (a verdict, with its status) and one that spent its retries
/// without ever getting an answer (a transport failure, "no HTTP verdict").
/// The lead sentence therefore claims neither of the two on its own.
///
/// Rendered as a board section, so a caller that re-posts the board or a
/// follow-up note can append this verbatim. It is deliberately **not** part of
/// [`crate::publisher::render_board`]: the counting happens while the notes are
/// posted, after the board body has been built.
pub fn render_inline_failure_section(summary: &PublishSummary) -> String {
    if summary.failures.is_empty() {
        return String::new();
    }

    let mut out = format!(
        "## Inline notes — {} could not be published\n\n\
         > ⛔ **{} inline note(s) failed to publish**: each was either refused by the provider or \
         stopped after its retries without an answer, and neither kind is retried again. The \
         status on each line below says which. The round's findings are all in the sections \
         above; only their inline anchors are missing.\n\n",
        summary.failures.len(),
        summary.failures.len(),
    );
    for failure in &summary.failures {
        out.push_str(&format!(
            "- `{}` — {} — {}\n",
            failure.anchor,
            failure
                .status
                .map_or_else(|| "no HTTP verdict".to_string(), |code| format!("HTTP {code}")),
            one_line(&failure.detail),
        ));
    }
    out.push('\n');
    out
}

/// Collapse a provider cause onto one line, so it cannot break out of the
/// bullet it is rendered in.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

///
/// Prefers the provider's own [`InlineNoteError`](crate::git_provider::InlineNoteError)
/// — the status and the response body are then structural rather than parsed
/// out of the rendered message. A provider that reports a plain `anyhow` error
/// (a transport failure, a rejected path, the mocks the publisher's tests use)
/// still yields its status when the message carries one, through the same
/// helper [`is_transient_error`] classifies with.
///
/// The RENG-71 deployment log had neither: the cause was dropped at this call
/// site, so a `400 position is invalid` was indistinguishable from a 403, a 404
/// or a broken connection.
fn inline_note_failure_detail(err: &anyhow::Error) -> (Option<u16>, String) {
    for cause in err.chain() {
        if let Some(verdict) = cause.downcast_ref::<crate::git_provider::InlineNoteError>() {
            // The typed message already renders status, endpoint and body.
            return (verdict.status, crate::llm::sampling::truncate_error(&verdict.message));
        }
        let message = cause.to_string();
        if let Some(status) = http_status_code(&message) {
            return (Some(status), crate::llm::sampling::truncate_error(&message));
        }
    }
    (None, crate::llm::sampling::truncate_error(&format!("{err:#}")))
}

/// POST one inline note, retrying transient failures with a bounded backoff.
///
/// Permanent errors (a 4xx verdict such as GitLab's blank `line_code`, or a
/// rejected path) are returned immediately: retrying cannot change the answer.
async fn post_inline_with_retry(
    provider: &dyn crate::git_provider::GitProvider,
    anchor: &InlineAnchor,
    body: &str,
) -> Result<()> {
    let mut attempt = 1;
    loop {
        match provider.post_inline_comment_at(anchor, body).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= INLINE_POST_MAX_ATTEMPTS || !is_transient_error(&err) {
                    return Err(err);
                }
                let backoff = INLINE_POST_RETRY_BACKOFF * 2u32.pow(attempt - 1);
                tracing::warn!(
                    file = %anchor.file,
                    line = anchor.line,
                    old_line = anchor.old_line,
                    attempt,
                    error = %err,
                    "Transient inline-note failure; retrying"
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
        }
    }
}

/// Transport-layer phrases that mean "the request never got an answer".
///
/// Consulted only when the failure carries no HTTP status at all — see
/// [`is_transient_error`].
const TRANSPORT_FAILURE_MARKERS: [&str; 11] = [
    "error sending request",
    "failed to send",
    "connection refused",
    "connection reset",
    "connection closed",
    "timed out",
    "timeout",
    "dns error",
    "broken pipe",
    "unexpected eof",
    "temporarily unavailable",
];

/// Whether a failed inline post is worth retrying.
///
/// The provider's HTTP verdict decides whenever there is one: 408 / 429 / 5xx
/// are transient, every other status is permanent — including the 400 a
/// rejected anchor earns and the 401/403/404 a wrong token or a moved MR earns.
/// Only a failure with **no** HTTP answer at all (a transport failure) falls
/// back to the message, because the message is then the only evidence there is.
///
/// The order is the whole point (RENG-65). Classifying by substring first made
/// the classification depend on the *response body*: a `404` whose body
/// mentioned `connection closed` — GitLab echoes proxy errors, and a captured
/// body is attacker-adjacent text — was read as a transport failure and retried
/// three times. This is the publisher-side twin of RENG-35, which fixed the
/// same false positive for the LLM path by classifying on the status code.
fn is_transient_error(err: &anyhow::Error) -> bool {
    match http_status_verdict(err) {
        Some(status) => status == 408 || status == 429 || (500..600).contains(&status),
        None => err
            .chain()
            .any(|cause| has_transport_failure_marker(&cause.to_string())),
    }
}

/// The HTTP status a provider error carries, when it carries one.
///
/// Prefers the structural status of a
/// [`InlineNoteError`](crate::git_provider::InlineNoteError) — the shape the
/// real clients produce — over parsing the status back out of a rendered
/// message, which is only there for providers (and mocks) that report a plain
/// `anyhow` error.
fn http_status_verdict(err: &anyhow::Error) -> Option<u16> {
    err.chain()
        .find_map(|cause| {
            cause
                .downcast_ref::<crate::git_provider::InlineNoteError>()
                .and_then(|verdict| verdict.status)
        })
        .or_else(|| err.chain().find_map(|cause| http_status_code(&cause.to_string())))
}

/// Whether a message reads like a transport failure with no HTTP answer.
fn has_transport_failure_marker(message: &str) -> bool {
    let lowered = message.to_lowercase();
    TRANSPORT_FAILURE_MARKERS.iter().any(|needle| lowered.contains(needle))
}

/// Extract the status code from a provider error such as
/// `GitLab API returned 400 Bad Request for POST merge_requests/43/discussions: …`.
fn http_status_code(message: &str) -> Option<u16> {
    const MARKER: &str = "returned ";
    let start = message.find(MARKER)? + MARKER.len();
    let digits: String = message[start..].chars().take_while(char::is_ascii_digit).collect();
    if digits.len() == 3 {
        digits.parse().ok()
    } else {
        None
    }
}

/// Format a finding's recommendation as a GitLab suggestion block.
///
/// The output uses ````suggestion` fence so that GitLab renders an
/// "Apply suggestion" button.  If `evidence` is non-empty it is used as
/// the "before" code; otherwise only the recommendation is shown
/// (no replace suggestion). Backticks in content are escaped to prevent
/// fence breakage.
pub fn format_suggestion_block(evidence: &str, recommendation: &str) -> String {
    /// Escape backticks in content to prevent fence breakage.
    fn escape_backticks(s: &str) -> String {
        s.replace('`', "\\`")
    }

    if evidence.is_empty() {
        recommendation.to_string()
    } else {
        format!(
            "```suggestion\n{code}\n```\n\n{note}",
            code = escape_backticks(evidence.trim_end()),
            note = escape_backticks(recommendation),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Effort, Finding, ReviewOutput, Severity};

    /// A policy that posts every finding the thresholds admit — used by the
    /// tests that cover gates other than the cap.
    fn unlimited_policy() -> PublishPolicy {
        PublishPolicy {
            max_inline_notes_per_round: usize::MAX,
            ..Default::default()
        }
    }

    fn caps_policy(cap: usize) -> PublishPolicy {
        PublishPolicy {
            max_inline_notes_per_round: cap,
            ..Default::default()
        }
    }

    #[test]
    fn test_inline_note_struct() {
        let note = InlineNote {
            file: "src/main.rs".to_string(),
            line: 42,
            body: "test comment".to_string(),
        };
        assert_eq!(note.file, "src/main.rs");
        assert_eq!(note.line, 42);
        assert_eq!(note.body, "test comment");
    }

    #[tokio::test]
    async fn test_publish_inline_notes_skips_low_severity() {
        use crate::git_provider::GitProvider;
        use crate::models::{Effort, Finding, MRInfo, Severity};
        use async_trait::async_trait;

        // Create a mock provider that records inline-comment calls.
        struct MockGitProvider {
            calls: std::sync::Mutex<Vec<String>>,
        }

        #[async_trait]
        impl GitProvider for MockGitProvider {
            async fn fetch_mr_info(&self) -> anyhow::Result<MRInfo> {
                unimplemented!()
            }
            async fn fetch_diff(&self) -> anyhow::Result<String> {
                unimplemented!()
            }
            async fn post_review_comment(&self, _body: &str) -> anyhow::Result<i64> {
                unimplemented!()
            }
            async fn post_inline_comment(&self, file: &str, _line: u32, _body: &str) -> anyhow::Result<()> {
                self.calls.lock().unwrap().push(file.to_string());
                Ok(())
            }
            async fn fetch_code_audit_toml(&self) -> anyhow::Result<Option<String>> {
                unimplemented!()
            }
            async fn add_reaction(&self, _comment_id: i64, _reaction: &str) -> anyhow::Result<()> {
                unimplemented!()
            }
            async fn update_discussion(&self, _discussion_id: &str, _body: &str) -> anyhow::Result<()> {
                unimplemented!()
            }
        }

        let findings = vec![
            Finding {
                file: "critical.rs".to_string(),
                line: Some(1),
                line_end: None,
                severity: Severity::Critical,
                confidence: 9,
                category: String::new(),
                title: "Critical bug".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Fix it".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
            Finding {
                file: "low.rs".to_string(),
                line: Some(5),
                line_end: None,
                severity: Severity::Low,
                confidence: 3,
                category: String::new(),
                title: "Minor".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Consider".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
        ];

        let provider = MockGitProvider {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let policy = unlimited_policy();
        let plan = plan_inline_notes(&findings, None, false, &policy);
        let summary = publish_planned_inline_notes(&provider, &plan).await;
        let called_files = provider.calls.lock().unwrap().clone();
        assert_eq!(called_files.len(), 1);
        assert!(called_files.contains(&"critical.rs".to_string()));
        assert_eq!(summary.policy_excluded, 1, "the Low finding is policy-excluded");
    }

    #[tokio::test]
    async fn test_publish_inline_notes_skips_unsafe_paths() {
        use crate::git_provider::GitProvider;
        use crate::models::{Effort, Finding, MRInfo, Severity};
        use async_trait::async_trait;

        struct MockGitProvider {
            calls: std::sync::Mutex<Vec<String>>,
        }

        #[async_trait]
        impl GitProvider for MockGitProvider {
            async fn fetch_mr_info(&self) -> anyhow::Result<MRInfo> {
                unimplemented!()
            }
            async fn fetch_diff(&self) -> anyhow::Result<String> {
                unimplemented!()
            }
            async fn post_review_comment(&self, _body: &str) -> anyhow::Result<i64> {
                unimplemented!()
            }
            async fn post_inline_comment(&self, file: &str, _line: u32, _body: &str) -> anyhow::Result<()> {
                self.calls.lock().unwrap().push(file.to_string());
                Ok(())
            }
            async fn fetch_code_audit_toml(&self) -> anyhow::Result<Option<String>> {
                unimplemented!()
            }
            async fn add_reaction(&self, _comment_id: i64, _reaction: &str) -> anyhow::Result<()> {
                unimplemented!()
            }
            async fn update_discussion(&self, _discussion_id: &str, _body: &str) -> anyhow::Result<()> {
                unimplemented!()
            }
        }

        let findings = vec![
            Finding {
                file: "../etc/passwd".to_string(),
                line: Some(1),
                line_end: None,
                severity: Severity::Critical,
                confidence: 9,
                category: String::new(),
                title: "Unsafe path".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Fix it".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
            Finding {
                file: "/etc/passwd".to_string(),
                line: Some(1),
                line_end: None,
                severity: Severity::Critical,
                confidence: 9,
                category: String::new(),
                title: "Absolute path".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Fix it".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
            Finding {
                file: "safe.rs".to_string(),
                line: Some(1),
                line_end: None,
                severity: Severity::Critical,
                confidence: 9,
                category: String::new(),
                title: "Safe path".to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: "Fix it".to_string(),
                effort: Effort::Small,
                expert_name: String::new(),
                expert_role: String::new(),
                agrees_with: Vec::new(),
                references: Vec::new(),
            },
        ];

        let provider = MockGitProvider {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let policy = unlimited_policy();
        let plan = plan_inline_notes(&findings, None, false, &policy);
        let summary = publish_planned_inline_notes(&provider, &plan).await;
        let called_files = provider.calls.lock().unwrap().clone();
        assert_eq!(called_files.len(), 1);
        assert!(called_files.contains(&"safe.rs".to_string()));
        assert_eq!(summary.skipped, 2, "both unsafe paths are gated");
    }

    // ── RENG-60: consolidated publish set, anchors, failure isolation ────────

    /// Recording provider: keeps every `(file, line, body)` POST and can be
    /// told to fail a given file for its first N attempts with a chosen error.
    ///
    /// It also keeps the [`InlineAnchor`] each POST arrived with (RENG-99): the
    /// anchor is what decides whether the provider can address the line at all,
    /// and the default [`crate::git_provider::GitProvider::post_inline_comment_at`]
    /// would hide it.
    struct RecordingProvider {
        posts: std::sync::Mutex<Vec<(String, u32, String)>>,
        anchors: std::sync::Mutex<Vec<InlineAnchor>>,
        attempts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
        failures: std::collections::HashMap<String, (usize, String)>,
    }

    impl RecordingProvider {
        /// `failures` entries are `(file, failing_attempts, error_message)`.
        fn new(failures: &[(&str, usize, &str)]) -> Self {
            Self {
                posts: std::sync::Mutex::new(Vec::new()),
                anchors: std::sync::Mutex::new(Vec::new()),
                attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
                failures: failures
                    .iter()
                    .map(|(file, attempts, message)| ((*file).to_string(), (*attempts, (*message).to_string())))
                    .collect(),
            }
        }

        fn posted(&self) -> Vec<(String, u32, String)> {
            self.posts.lock().unwrap().clone()
        }

        fn anchors(&self) -> Vec<InlineAnchor> {
            self.anchors.lock().unwrap().clone()
        }

        fn attempts_for(&self, file: &str) -> usize {
            *self.attempts.lock().unwrap().get(file).unwrap_or(&0)
        }
    }

    #[async_trait::async_trait]
    impl crate::git_provider::GitProvider for RecordingProvider {
        async fn fetch_mr_info(&self) -> anyhow::Result<crate::models::MRInfo> {
            unimplemented!()
        }
        async fn fetch_diff(&self) -> anyhow::Result<String> {
            unimplemented!()
        }
        async fn post_review_comment(&self, _body: &str) -> anyhow::Result<i64> {
            unimplemented!()
        }
        async fn post_inline_comment(&self, file: &str, line: u32, body: &str) -> anyhow::Result<()> {
            let attempt = {
                let mut counts = self.attempts.lock().unwrap();
                let entry = counts.entry(file.to_string()).or_insert(0);
                *entry += 1;
                *entry
            };
            if let Some((failing_attempts, message)) = self.failures.get(file) {
                if attempt <= *failing_attempts {
                    anyhow::bail!("{}", message);
                }
            }
            self.posts
                .lock()
                .unwrap()
                .push((file.to_string(), line, body.to_string()));
            Ok(())
        }
        async fn post_inline_comment_at(&self, anchor: &InlineAnchor, body: &str) -> anyhow::Result<()> {
            self.anchors.lock().unwrap().push(anchor.clone());
            self.post_inline_comment(&anchor.file, anchor.line, body).await
        }
        async fn fetch_code_audit_toml(&self) -> anyhow::Result<Option<String>> {
            unimplemented!()
        }
        async fn add_reaction(&self, _comment_id: i64, _reaction: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn update_discussion(&self, _discussion_id: &str, _body: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
    }

    fn make_finding(expert: &str, file: &str, line: u32, severity: Severity) -> Finding {
        Finding {
            file: file.to_string(),
            line: Some(line),
            line_end: None,
            severity,
            confidence: 9,
            category: "security".to_string(),
            title: "Audio write errors are silently ignored".to_string(),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: "Propagate the error".to_string(),
            effort: Effort::Small,
            expert_name: expert.to_string(),
            expert_role: String::new(),
            agrees_with: Vec::new(),
            references: Vec::new(),
        }
    }

    fn make_report(expert: &str, findings: Vec<Finding>) -> crate::models::ExpertReport {
        crate::models::ExpertReport {
            expert_name: expert.to_string(),
            findings,
            markdown: String::new(),
            raw_llm_response: String::new(),
            parse_error: None,
            raw_dump_path: None,
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }
    }

    /// Plan the findings under `policy`, post them, and log the summary.
    async fn publish(
        provider: &dyn crate::git_provider::GitProvider,
        findings: &[Finding],
        policy: &PublishPolicy,
    ) -> PublishSummary {
        let plan = plan_inline_notes(findings, None, false, policy);
        publish_planned_inline_notes(provider, &plan).await
    }

    /// The publish set is the consolidated set — same order, same count.
    ///
    /// The fixture reproduces the corpus' same-round multi-expert duplicate
    /// (`sirena!22`: one issue reported by ux, security, database and devops,
    /// posted as four notes). The raw reports carry four findings that
    /// consolidation merges into one; the published set must be the merged one.
    #[tokio::test]
    async fn test_publish_set_equals_consolidated_set() {
        let reports: Vec<crate::models::ExpertReport> = ["ux", "security", "database", "devops"]
            .iter()
            .map(|expert| {
                make_report(
                    expert,
                    vec![make_finding(
                        expert,
                        "crates/sirena-cli/src/session.rs",
                        78,
                        Severity::High,
                    )],
                )
            })
            .collect();
        let raw_count: usize = reports.iter().map(|r| r.findings.len()).sum();

        let consolidated =
            crate::team::lead_consolidator::ConsolidatorConfig::default().consolidate(&reports, Some(80));
        assert_eq!(
            consolidated.duplicates_merged, 3,
            "the fixture must actually exercise dedup"
        );
        assert_eq!(consolidated.findings.len(), 1);

        let output = ReviewOutput {
            reports,
            aggregated: None,
            dropped_findings: Vec::new(),
            consolidated: Some(consolidated.clone()),
            errors: Vec::new(),
        };

        let provider = RecordingProvider::new(&[]);
        let policy = unlimited_policy();
        let plan = plan_inline_notes_for_output(&output, None, false, &policy);
        let summary = publish_planned_inline_notes(&provider, &plan).await;

        let expected: Vec<(String, u32, String)> = consolidated
            .findings
            .iter()
            .map(|f| (f.file.clone(), f.line.unwrap_or(0), format_inline_body(f)))
            .collect();
        assert_eq!(
            provider.posted(),
            expected,
            "the published set must be the consolidated set, in its order"
        );
        assert_eq!(summary.posted, consolidated.findings.len());
        assert_eq!(summary.posted, 1);
        assert!(
            provider.posted().len() < raw_count,
            "the raw duplicates must not be posted"
        );
    }

    /// Bodies carry an unambiguous file anchor: two findings that differ only by
    /// file must never render the same note (corpus notes 10737/10738).
    #[test]
    fn test_inline_body_includes_file_anchor() {
        let gitlink_a = make_finding("docs", "linux-5.15.147", 3, Severity::High);
        let gitlink_b = make_finding("docs", "ubuntu26-linux-5.15.147", 3, Severity::High);

        let body_a = format_inline_body(&gitlink_a);
        let body_b = format_inline_body(&gitlink_b);
        assert_ne!(
            body_a, body_b,
            "findings differing only by file must not render identically"
        );
        assert!(
            body_a.contains("`linux-5.15.147:3`"),
            "body must carry the file anchor: {body_a}"
        );
        assert!(
            body_b.contains("`ubuntu26-linux-5.15.147:3`"),
            "body must carry the file anchor: {body_b}"
        );

        let mut ranged = make_finding("lead", "crates/sirena-cli/src/rt.rs", 120, Severity::Critical);
        ranged.line_end = Some(132);
        assert!(format_inline_body(&ranged).contains("`crates/sirena-cli/src/rt.rs:120-132`"));
    }

    /// One failing post must not abandon the rest of the batch (corpus §4.6).
    #[tokio::test]
    async fn test_publish_inline_notes_isolates_failures() {
        let findings = vec![
            make_finding("ux", "one.rs", 1, Severity::High),
            make_finding("devops", "two.rs", 2, Severity::High),
            make_finding("lead", "three.rs", 3, Severity::Critical),
        ];
        let provider = RecordingProvider::new(&[(
            "two.rs",
            1,
            "GitLab API returned 400 Bad Request for POST merge_requests/43/discussions: \
             {\"message\":\"400 Bad request - Note {:line_code=>[\\\"can't be blank\\\"]}\"}",
        )]);

        let summary = publish(&provider, &findings, &unlimited_policy()).await;

        let posted_files: Vec<String> = provider.posted().into_iter().map(|(file, _, _)| file).collect();
        assert_eq!(posted_files, vec!["one.rs", "three.rs"]);
        assert_eq!(summary.posted, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 0);
        // A 4xx verdict is permanent: exactly one attempt, never retried.
        assert_eq!(provider.attempts_for("two.rs"), 1);
    }

    // ── RENG-71: the cause of a rejected inline note ─────────────────────────

    /// The provider's own verdict is used structurally: the status and the
    /// response body are carried by the error, not parsed back out of the
    /// rendered message.
    #[test]
    fn test_inline_note_failure_detail_uses_the_provider_verdict() {
        let body = r#"{"message":"400 Bad request - Note {:position=>[\"new_line is not part of the diff\"]}}"#;
        let err = anyhow::Error::new(crate::git_provider::InlineNoteError {
            status: Some(400),
            body: body.to_string(),
            message: format!("GitLab API returned 400 Bad Request for POST merge_requests/54/discussions: {body}"),
        });

        let (status, detail) = inline_note_failure_detail(&err);
        assert_eq!(status, Some(400));
        assert!(
            detail.contains("400 Bad Request for POST merge_requests/54/discussions"),
            "the detail names the endpoint: {detail}"
        );
        assert!(
            detail.contains("new_line is not part of the diff"),
            "the detail carries GitLab's response body: {detail}"
        );
    }

    /// A provider that only renders a message still reports its status (the
    /// pre-RENG-71 shape, and every mock in these tests), and a failure with no
    /// HTTP answer is reported as such instead of being guessed at.
    #[test]
    fn test_inline_note_failure_detail_falls_back_to_the_rendered_message() {
        let rendered = anyhow::Error::msg(
            "GitLab API returned 403 Forbidden for POST merge_requests/1/discussions: \
             {\"message\":\"403 Forbidden\"}",
        );
        let (status, detail) = inline_note_failure_detail(&rendered);
        assert_eq!(status, Some(403));
        assert!(detail.contains("403 Forbidden"), "{detail}");

        let transport =
            anyhow::Error::msg("error sending request for url (http://gitlab.islet.space/api/v4): connection refused");
        let (status, detail) = inline_note_failure_detail(&transport);
        assert_eq!(status, None, "a transport failure has no HTTP verdict");
        assert!(detail.contains("connection refused"), "{detail}");
    }

    /// A provider body can be arbitrarily long; the WARN is one line, and the
    /// status at the head of the message survives the cut.
    #[test]
    fn test_inline_note_failure_detail_truncates_a_long_response_body() {
        let long = format!(
            "GitLab API returned 400 Bad Request for POST merge_requests/1/discussions: {{\"message\":\"{}\"}}",
            "x".repeat(crate::llm::sampling::ERROR_MAX_CHARS * 2)
        );
        let (status, detail) = inline_note_failure_detail(&anyhow::Error::msg(long));

        assert_eq!(status, Some(400));
        assert_eq!(
            detail.chars().count(),
            crate::llm::sampling::ERROR_MAX_CHARS + 1,
            "the detail is the LLM-error bound plus the clip marker"
        );
        assert!(detail.ends_with('…'), "a clipped body is marked as clipped: {detail}");
        assert!(
            detail.starts_with("GitLab API returned 400 Bad Request"),
            "the status must not be cut off: {detail}"
        );
    }

    /// The batch survives a rejected position and posts the next note: the
    /// RENG-60 isolation is unchanged by the RENG-71 reporting.
    #[tokio::test]
    async fn test_publish_inline_notes_reports_the_rejected_anchor_and_continues() {
        let findings = vec![
            make_finding("ux", "bad.rs", 7, Severity::High),
            make_finding("lead", "good.rs", 9, Severity::High),
        ];
        let provider = RecordingProvider::new(&[(
            "bad.rs",
            1,
            "GitLab API returned 400 Bad Request for POST merge_requests/54/discussions: \
             {\"message\":\"400 Bad request - Note {:position=>[\\\"new_line is not part of the diff\\\"]}\"}",
        )]);

        let summary = publish(&provider, &findings, &unlimited_policy()).await;

        assert_eq!(summary.failed, 1);
        assert_eq!(summary.posted, 1);
        assert_eq!(
            provider
                .posted()
                .into_iter()
                .map(|(file, _, _)| file)
                .collect::<Vec<_>>(),
            vec!["good.rs"],
            "the note after the rejected one is still posted"
        );
        assert_eq!(provider.attempts_for("bad.rs"), 1, "a 4xx verdict is permanent");

        // What the WARN says about this failure — the status, the anchor and
        // the truncated body — is asserted end to end (real client, real log)
        // by `tests/publish/main.rs::test_rejected_inline_note_logs_its_cause_and_the_batch_continues`.
    }

    /// Transient transport/5xx failures are retried, and a recovered batch is a
    /// success rather than a silent loss.
    #[tokio::test(start_paused = true)]
    async fn test_publish_inline_notes_retries_transient_failures() {
        let findings = vec![make_finding("ux", "flaky.rs", 5, Severity::High)];
        let provider = RecordingProvider::new(&[(
            "flaky.rs",
            2,
            "GitLab API returned 503 Service Unavailable for POST merge_requests/1/discussions: {}",
        )]);

        let summary = publish(&provider, &findings, &unlimited_policy()).await;

        assert_eq!(summary.posted, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(provider.attempts_for("flaky.rs"), 3);
    }

    /// A transient failure that never clears stays bounded (no infinite retry)
    /// and the batch still finishes.
    #[tokio::test(start_paused = true)]
    async fn test_publish_inline_notes_bounds_transient_retries() {
        let findings = vec![
            make_finding("ux", "down.rs", 1, Severity::High),
            make_finding("ux", "up.rs", 2, Severity::High),
        ];
        let provider = RecordingProvider::new(&[(
            "down.rs",
            99,
            "error sending request for url (http://gitlab): connection refused",
        )]);

        let summary = publish(&provider, &findings, &unlimited_policy()).await;

        assert_eq!(provider.attempts_for("down.rs"), INLINE_POST_MAX_ATTEMPTS as usize);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.posted, 1, "the remaining findings are still published");
        assert_eq!(provider.posted()[0].0, "up.rs");
    }

    /// RENG-65 (retry accounting): a permanent verdict costs exactly one
    /// attempt, even when its response body reads like a transport failure;
    /// 429 and 5xx keep retrying to the same bound as before.
    #[tokio::test(start_paused = true)]
    async fn test_publish_inline_notes_attempts_a_permanent_verdict_once() {
        for (file, message) in [
            (
                "unauthorized.rs",
                "GitLab API returned 401 Unauthorized for POST merge_requests/1/discussions: \
                 {\"message\":\"connection closed by remote host\"}",
            ),
            (
                "vanished.rs",
                "GitLab API returned 404 Not Found for POST merge_requests/1/discussions: \
                 {\"message\":\"connection reset while reading the diff\"}",
            ),
        ] {
            let findings = vec![make_finding("ux", file, 5, Severity::High)];
            let provider = RecordingProvider::new(&[(file, 99, message)]);
            let summary = publish(&provider, &findings, &unlimited_policy()).await;

            assert_eq!(
                provider.attempts_for(file),
                1,
                "{file} carries a permanent status — one attempt, no retry"
            );
            assert_eq!(summary.posted, 0);
            assert_eq!(summary.failed, 1);
        }

        // The retryable set is unchanged: 429 still spends every attempt.
        let rate_limited = RecordingProvider::new(&[(
            "limited.rs",
            99,
            "GitLab API returned 429 Too Many Requests for POST merge_requests/1/discussions: {}",
        )]);
        let findings = vec![make_finding("ux", "limited.rs", 5, Severity::High)];
        let summary = publish(&rate_limited, &findings, &unlimited_policy()).await;
        assert_eq!(
            rate_limited.attempts_for("limited.rs"),
            INLINE_POST_MAX_ATTEMPTS as usize,
            "429 remains transient"
        );
        assert_eq!(summary.failed, 1);
    }

    /// Findings whose anchor is outside the reviewed diff are skipped instead of
    /// being sent (the corpus' five `400 line_code can't be blank` rejections).
    #[tokio::test]
    async fn test_publish_inline_notes_skips_findings_outside_diff() {
        let diff = "diff --git a/src/rt.rs b/src/rt.rs\n\
                    index 1111111..2222222 100644\n\
                    --- a/src/rt.rs\n\
                    +++ b/src/rt.rs\n\
                    @@ -10,3 +10,4 @@ fn main() {\n\
                     let a = 1;\n\
                    +let b = 2;\n\
                     let c = 3;\n\
                    \x20let d = 4;\n";
        let index = DiffIndex::from_diff(diff);
        assert!(index.contains("src/rt.rs", 11), "line 11 is inside the changed hunk");
        assert!(!index.contains("src/rt.rs", 99), "line 99 is outside every hunk");
        assert!(!index.contains("other.rs", 11), "the file is not in the diff");

        let findings = vec![
            make_finding("ux", "src/rt.rs", 11, Severity::High),
            make_finding("ux", "src/rt.rs", 99, Severity::High),
            make_finding("ux", "other.rs", 11, Severity::High),
        ];
        let provider = RecordingProvider::new(&[]);
        let policy = unlimited_policy();
        let plan = plan_inline_notes(&findings, Some(&index), false, &policy);
        let summary = publish_planned_inline_notes(&provider, &plan).await;

        assert_eq!(summary.posted, 1);
        assert_eq!(summary.skipped, 2);
        assert_eq!(provider.posted()[0].0, "src/rt.rs");
        assert_eq!(provider.posted()[0].1, 11);
    }

    #[test]
    fn test_diff_index_has_no_anchor_for_pure_deletions() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,0 @@\n-old\n-old\n";
        let index = DiffIndex::from_diff(diff);
        assert!(index.is_empty(), "a pure deletion has no line on the new side");
        assert_eq!(
            index.changed_files(),
            ["x.rs"],
            "a pure deletion is still a changed file (the docs-only check needs it)"
        );

        // An unparsable/empty diff yields an empty index, which callers read as
        // "no index available" (never as "everything is outside the diff").
        assert!(DiffIndex::from_diff("").is_empty());
        assert!(DiffIndex::from_diff("").changed_files().is_empty());
    }

    // ── RENG-99: the anchor GitLab can match ────────────────────────────────

    /// A hunk carrying every line shape GitLab distinguishes: an unchanged line
    /// (needs both numbers), an added line (needs `new_line` alone) and a
    /// removed line (has no new-side number at all).
    const MIXED_HUNK_DIFF: &str = "diff --git a/src/rt.rs b/src/rt.rs\n\
                                   index 1111111..2222222 100644\n\
                                   --- a/src/rt.rs\n\
                                   +++ b/src/rt.rs\n\
                                   @@ -10,5 +10,5 @@ fn main() {\n\
                                   \x20let a = 1;\n\
                                   -let b = 2;\n\
                                   +let b = 3;\n\
                                   \x20let c = 4;\n\
                                   \x20let d = 5;\n";

    /// The old-side number is what GitLab's pair match needs, and the index is
    /// the only place that knows it.
    #[test]
    fn test_diff_index_carries_the_old_line_of_an_unchanged_line() {
        let index = DiffIndex::from_diff(MIXED_HUNK_DIFF);

        assert_eq!(
            index.anchor_for("src/rt.rs", 10),
            Some(InlineAnchor::new("src/rt.rs", 10).with_old_line(10)),
            "line 10 is unchanged: it needs both numbers"
        );
        assert_eq!(
            index.anchor_for("src/rt.rs", 11),
            Some(InlineAnchor::new("src/rt.rs", 11)),
            "line 11 is added: its old side has no number, so none is sent"
        );
        assert_eq!(
            index.anchor_for("src/rt.rs", 13),
            Some(InlineAnchor::new("src/rt.rs", 13).with_old_line(13)),
            "the context run stays anchored across its lines"
        );

        // Outside every hunk, and in a file the diff does not touch: no anchor —
        // the same set the gate admits.
        assert_eq!(index.anchor_for("src/rt.rs", 9), None);
        assert_eq!(index.anchor_for("src/rt.rs", 14), None);
        assert_eq!(index.anchor_for("other.rs", 10), None);
        assert!(!index.contains("src/rt.rs", 14));
        assert!(index.contains("src/rt.rs", 10), "an unchanged line is still anchorable");
    }

    /// The `\ No newline at end of file` marker is not a line: it must not
    /// consume a new-side number, which would shift every later anchor.
    #[test]
    fn test_diff_index_ignores_the_no_newline_marker() {
        let diff = "diff --git a/x.rs b/x.rs\n\
                    --- a/x.rs\n\
                    +++ b/x.rs\n\
                    @@ -1,1 +1,2 @@\n\
                    \x20kept\n\
                    +added\n\
                    \\ No newline at end of file\n";
        let index = DiffIndex::from_diff(diff);

        assert_eq!(
            index.anchor_for("x.rs", 1),
            Some(InlineAnchor::new("x.rs", 1).with_old_line(1))
        );
        assert_eq!(index.anchor_for("x.rs", 2), Some(InlineAnchor::new("x.rs", 2)));
        assert!(
            !index.contains("x.rs", 3),
            "the marker must not be given a new-side line number"
        );
    }

    /// The publisher submits the anchor the diff implies: an unchanged line
    /// travels with its old-side number, an added line without one (RENG-99).
    #[tokio::test]
    async fn test_publish_submits_the_old_line_of_an_unchanged_line() {
        let index = DiffIndex::from_diff(MIXED_HUNK_DIFF);
        let findings = vec![
            make_finding("ux", "src/rt.rs", 10, Severity::High),
            make_finding("ux", "src/rt.rs", 11, Severity::High),
        ];
        let provider = RecordingProvider::new(&[]);
        let plan = plan_inline_notes(&findings, Some(&index), false, &unlimited_policy());
        let summary = publish_planned_inline_notes(&provider, &plan).await;

        assert_eq!(summary.posted, 2);
        assert_eq!(
            provider.anchors(),
            vec![
                InlineAnchor::new("src/rt.rs", 10).with_old_line(10),
                InlineAnchor::new("src/rt.rs", 11),
            ]
        );

        // Without a diff index the gate never ran, so the line alone is all the
        // publisher knows — exactly the pre-RENG-99 anchor.
        let provider = RecordingProvider::new(&[]);
        let plan = plan_inline_notes(&findings, None, false, &unlimited_policy());
        publish_planned_inline_notes(&provider, &plan).await;
        assert_eq!(provider.anchors()[0], InlineAnchor::new("src/rt.rs", 10));
    }

    /// RENG-99's other half: a note that never reached the MR is stated in the
    /// report, with its anchor and the provider's verdict — not only as a WARN.
    #[tokio::test(start_paused = true)]
    async fn test_inline_failure_section_names_what_was_lost() {
        let findings = vec![
            make_finding("ux", "one.rs", 1, Severity::High),
            make_finding("ux", "two.rs", 2, Severity::High),
        ];
        let provider = RecordingProvider::new(&[
            (
                "one.rs",
                1,
                "GitLab API returned 400 Bad Request for POST merge_requests/54/discussions: \
                 {\"message\":\"400 Bad request - Note {:line_code=>[\\\"can't be blank\\\"]}\"}",
            ),
            (
                "two.rs",
                99,
                "error sending request for url (http://gitlab): connection refused",
            ),
        ]);
        let summary = publish(&provider, &findings, &unlimited_policy()).await;

        let section = render_inline_failure_section(&summary);
        assert!(
            section.starts_with("## Inline notes — 2 could not be published"),
            "the count is what the user must see: {section}"
        );
        assert!(section.contains("**2 inline note(s) failed to publish**"), "{section}");
        // The lead sentence has to hold for both kinds of entry this section
        // renders — a provider verdict and a spent transport retry — because the
        // per-line detail distinguishes them (RENG-99 review r1 P2-1).
        assert!(
            section.contains("either refused by the provider or stopped after its retries"),
            "the blanket sentence must not claim a refusal for a transport failure: {section}"
        );
        assert!(
            section.contains("- `one.rs:1` — HTTP 400 — "),
            "the refused anchor and its status are named: {section}"
        );
        assert!(
            section.contains("line_code"),
            "GitLab's verdict is carried into the report: {section}"
        );
        assert!(
            section.contains("- `two.rs:2` — no HTTP verdict — "),
            "a transport failure is reported as one, not as an HTTP status: {section}"
        );
        assert_eq!(
            summary.failed,
            summary.failures.len(),
            "the counter and the list must not drift apart"
        );

        // A round that lost nothing adds nothing to the report.
        let clean = publish(&RecordingProvider::new(&[]), &findings, &unlimited_policy()).await;
        assert!(render_inline_failure_section(&clean).is_empty());
    }

    /// A provider cause is rendered on one line: a multi-line body must not
    /// break out of the bullet it belongs to.
    #[test]
    fn test_inline_failure_section_keeps_one_line_per_failure() {
        let summary = PublishSummary {
            failed: 1,
            failures: vec![InlineNoteFailure {
                anchor: "src/rt.rs:12".to_string(),
                status: Some(400),
                detail: "GitLab said\n  line two of the body".to_string(),
            }],
            ..Default::default()
        };
        let section = render_inline_failure_section(&summary);

        assert!(section.contains("GitLab said line two of the body"), "{section}");
        assert!(!section.contains("said\n"), "the body is flattened: {section}");
    }

    #[test]
    fn test_transient_error_classification() {
        /// Build an error without format-string interpretation (`{}` / `{` appear
        /// literally in provider error bodies).
        fn err(message: &str) -> anyhow::Error {
            anyhow::Error::msg(message.to_string())
        }

        assert!(is_transient_error(&err(
            "GitLab API returned 503 Service Unavailable for POST merge_requests/1/discussions: {}"
        )));
        assert!(
            is_transient_error(&err(
                "GitLab API returned 429 Too Many Requests for POST merge_requests/1/discussions: {}"
            )),
            "429 stays retryable (RENG-65 must not tighten the retryable set)"
        );
        assert!(is_transient_error(&err(
            "error sending request for url (http://gitlab.islet.space/api/v4): connection refused"
        )));
        assert!(!is_transient_error(&err(
            "GitLab API returned 400 Bad Request for POST merge_requests/43/discussions: \
             {\"message\":\"400 Bad request - Note {:line_code=>[\\\"can't be blank\\\"]}\"}"
        )));
        assert!(!is_transient_error(&err(
            "GitLab API returned 404 Not Found for POST merge_requests/1/discussions: {}"
        )));
        assert!(!is_transient_error(&err(
            "Invalid file path for inline comment: ../etc/passwd"
        )));
    }

    /// RENG-65: a permanent status is permanent no matter what the response body
    /// says. Before this, a body mentioning `connection closed` won the
    /// classification and a 401/404 was retried three times.
    #[test]
    fn test_transient_error_classification_ignores_transport_words_in_a_verdict_body() {
        fn err(message: &str) -> anyhow::Error {
            anyhow::Error::msg(message.to_string())
        }

        for (status, phrase) in [
            (401, "connection closed by remote host"),
            (403, "connection reset by peer"),
            (404, "connection refused while fetching the diff"),
            (400, "the connection timed out before the diff was read"),
        ] {
            let message = format!(
                "GitLab API returned {status} for POST merge_requests/1/discussions: {{\"message\":\"{phrase}\"}}"
            );
            assert!(
                !is_transient_error(&err(&message)),
                "status {status} is permanent even though the body says {phrase:?}: {message}"
            );
        }

        // The same holds when the verdict is structural (the shape the real
        // clients produce) rather than parsed out of a rendered message.
        let verdict = anyhow::Error::new(crate::git_provider::InlineNoteError {
            status: Some(404),
            body: "{\"message\":\"connection closed\"}".to_string(),
            message: "GitLab API returned 404 Not Found for POST merge_requests/1/discussions: \
                      {\"message\":\"connection closed\"}"
                .to_string(),
        });
        assert!(!is_transient_error(&verdict));

        // A transport failure that carries no status at all is still retried —
        // the message is then the only evidence there is.
        let transport = anyhow::Error::new(crate::git_provider::InlineNoteError {
            status: None,
            body: String::new(),
            message: "Failed to send POST merge_requests/1/discussions: connection closed before \
                      message completed"
                .to_string(),
        });
        assert!(is_transient_error(&transport));
    }

    // ── RENG-63: policy floors, per-round cap, change-type adaptation ───────

    /// A six-finding round: file `f{i}` at line `i`, all admitted by the
    /// default policy (High, 9/10, with a recommendation).
    fn six_admitted_findings() -> Vec<Finding> {
        (1..=6)
            .map(|i| make_finding("lead", &format!("src/f{i}.rs"), i, Severity::High))
            .collect()
    }

    fn output_with(findings: Vec<Finding>) -> ReviewOutput {
        let mut report = make_report("lead", findings);
        // The board renders the pre-rendered expert markdown; give it the same
        // per-finding lines a real report carries so a test can see what the
        // board does and does not contain.
        report.markdown = report
            .findings
            .iter()
            .map(|f| format!("### {}\n", f.title))
            .collect::<String>();
        ReviewOutput::new(vec![report])
    }

    /// (a) The policy's floors keep low-value findings out of the inline
    /// delivery — and they are still on the board.
    #[tokio::test]
    async fn test_policy_floors_exclude_low_value_findings_but_keep_them_on_the_board() {
        let mut medium = make_finding("lead", "src/medium.rs", 1, Severity::Medium);
        medium.title = "Medium-severity observation".to_string();
        let mut unsure = make_finding("lead", "src/unsure.rs", 2, Severity::High);
        unsure.title = "High severity, low confidence".to_string();
        unsure.confidence = 7;
        let mut no_recommendation = make_finding("lead", "src/vague.rs", 3, Severity::Critical);
        no_recommendation.title = "Critical, but no actionable recommendation".to_string();
        no_recommendation.recommendation = "  ".to_string();
        let admitted = make_finding("lead", "src/real.rs", 4, Severity::High);
        let all = vec![medium, unsure, no_recommendation, admitted.clone()];

        let output = output_with(all);
        let policy = PublishPolicy::default();
        let plan = plan_inline_notes_for_output(&output, None, false, &policy);

        assert_eq!(plan.considered, 4);
        assert_eq!(plan.policy_excluded, 3, "medium / 7-of-10 / non-actionable");
        assert_eq!(plan.selected.len(), 1);
        assert_eq!(plan.selected[0].file, "src/real.rs");
        assert!(plan.rolled_up.is_empty(), "no cap involved");

        let provider = RecordingProvider::new(&[]);
        let summary = publish_planned_inline_notes(&provider, &plan).await;
        assert_eq!(summary.posted, 1);
        assert_eq!(summary.policy_excluded, 3);

        // The board keeps everything: it is rendered from `output.reports`,
        // which the inline policy never touches.
        let board = render_board(&output, &plan, &policy);
        for title in [
            "Medium-severity observation",
            "High severity, low confidence",
            "Critical, but no actionable recommendation",
        ] {
            assert!(board.contains(title), "the board must still carry {title:?}");
        }
    }

    /// (b) The cap posts exactly N findings and reports the remainder.
    #[tokio::test]
    async fn test_per_round_cap_posts_exactly_n_and_reports_the_remainder() {
        let findings = six_admitted_findings();
        let policy = caps_policy(3);
        let plan = plan_inline_notes(&findings[..], None, false, &policy);

        assert_eq!(plan.selected.len(), 3);
        assert_eq!(plan.rolled_up.len(), 3);
        assert_eq!(
            plan.rolled_up[0].file, "src/f4.rs",
            "the ranking decides who is rolled up"
        );

        let provider = RecordingProvider::new(&[]);
        let summary = publish_planned_inline_notes(&provider, &plan).await;
        assert_eq!(summary.posted, 3);
        assert_eq!(summary.rolled_up, 3);
        assert_eq!(summary.considered, 6);
        assert_eq!(provider.posted().len(), 3);

        // A zero cap posts nothing at all and rolls everything up.
        let plan = plan_inline_notes(&findings[..], None, false, &caps_policy(0));
        assert!(plan.selected.is_empty());
        assert_eq!(plan.rolled_up.len(), 6);
    }

    /// (c) A documentation/CI-only change set yields zero inline notes even
    /// with Critical findings; a mixed change set does not.
    #[tokio::test]
    async fn test_docs_only_round_posts_no_inline_notes_but_a_summary() {
        let docs_diff = "diff --git a/AGENTS.md b/AGENTS.md\n\
                         --- a/AGENTS.md\n\
                         +++ b/AGENTS.md\n\
                         @@ -1,1 +1,2 @@\n\
                         +# Workflow\n\
                         diff --git a/docs/design.md b/docs/design.md\n\
                         --- a/docs/design.md\n\
                         +++ b/docs/design.md\n\
                         @@ -1,1 +1,2 @@\n\
                         +# Design\n";
        let index = DiffIndex::from_diff(docs_diff);
        assert!(is_docs_or_ci_only(index.changed_files()));

        let findings = vec![
            make_finding("lead", "AGENTS.md", 1, Severity::Critical),
            make_finding("docs", "docs/design.md", 1, Severity::High),
        ];
        let output = output_with(findings);
        let policy = PublishPolicy::default();

        let plan = plan_inline_notes_for_output(&output, Some(&index), true, &policy);
        assert!(plan.selected.is_empty(), "no inline note for a docs-only round");
        assert_eq!(plan.rolled_up.len(), 2, "both findings stay on the board");
        assert_eq!(
            plan.skipped, 0,
            "nothing was gated per finding — the round is summary-only"
        );

        let provider = RecordingProvider::new(&[]);
        let summary = publish_planned_inline_notes(&provider, &plan).await;
        assert_eq!(summary.posted, 0);
        assert!(summary.summary_only);
        assert!(provider.posted().is_empty(), "zero provider calls");

        let board = render_board(&output, &plan, &policy);
        assert!(board.contains("Documentation/CI-only change"));
        assert!(board.contains("`AGENTS.md:1`"), "the withheld findings are named");

        // The policy escape restores inline delivery for docs-only rounds.
        let escaped = PublishPolicy {
            inline_on_docs_only: true,
            ..Default::default()
        };
        let plan = plan_inline_notes_for_output(&output, Some(&index), true, &escaped);
        assert_eq!(plan.selected.len(), 2);
    }

    /// A change set that touches code as well is *not* docs-only.
    #[test]
    fn test_mixed_change_set_is_not_docs_only() {
        let mixed = "diff --git a/README.md b/README.md\n\
                     --- a/README.md\n\
                     +++ b/README.md\n\
                     @@ -1,1 +1,2 @@\n\
                     +# Hi\n\
                     diff --git a/src/lib.rs b/src/lib.rs\n\
                     --- a/src/lib.rs\n\
                     +++ b/src/lib.rs\n\
                     @@ -1,1 +1,2 @@\n\
                     +pub fn f() {}\n";
        let index = DiffIndex::from_diff(mixed);
        assert!(!is_docs_or_ci_only(index.changed_files()));

        // And the round therefore posts inline notes as usual.
        let findings = [make_finding("lead", "src/lib.rs", 1, Severity::High)];
        let plan = plan_inline_notes(&findings[..], Some(&index), false, &PublishPolicy::default());
        assert_eq!(plan.selected.len(), 1);
    }

    /// (d) The same input always selects the same notes — including when the
    /// findings arrive in a different order.
    #[test]
    fn test_cap_selection_is_deterministic() {
        let mut findings = six_admitted_findings();
        // Make the ranking interesting: a Critical at the end, and a tie.
        findings[5].severity = Severity::Critical;
        let policy = PublishPolicy::default();

        let plan = plan_inline_notes(findings.iter(), None, false, &policy);
        assert!(
            plan.selected.iter().any(|f| f.file == "src/f6.rs"),
            "the Critical finding must survive the cap"
        );

        // Same input, same plan — the selection and the rolled-up list are
        // byte-for-byte identical, not merely the same size.
        let again = plan_inline_notes(findings.iter(), None, false, &policy);
        let anchors = |plan: &InlinePlan<'_>| {
            plan.selected
                .iter()
                .map(|f| (f.file.clone(), f.line))
                .collect::<Vec<_>>()
        };
        assert_eq!(anchors(&plan), anchors(&again));
        assert_eq!(
            plan.rolled_up.iter().map(|f| f.file.clone()).collect::<Vec<_>>(),
            again.rolled_up.iter().map(|f| f.file.clone()).collect::<Vec<_>>()
        );

        // A shuffled input selects the *same set* — the ranking decides, not
        // the arrival order (the posting order follows the source order).
        let reversed: Vec<Finding> = findings.iter().rev().cloned().collect();
        let plan_reversed = plan_inline_notes(reversed.iter(), None, false, &policy);
        let mut selected: Vec<String> = plan.selected.iter().map(|f| f.file.clone()).collect();
        let mut selected_reversed: Vec<String> = plan_reversed.selected.iter().map(|f| f.file.clone()).collect();
        selected.sort();
        selected_reversed.sort();
        assert_eq!(selected, selected_reversed);
        assert_eq!(selected, vec!["src/f1.rs", "src/f6.rs"]);
    }

    /// (e) End to end on a fixture: six admitted findings → two inline notes and
    /// four rolled into the board.
    #[tokio::test]
    async fn test_end_to_end_two_inline_four_rolled_into_the_board() {
        let output = output_with(six_admitted_findings());
        let policy = PublishPolicy::default();
        let provider = RecordingProvider::new(&[]);

        let plan = plan_inline_notes_for_output(&output, None, false, &policy);
        let summary = publish_planned_inline_notes(&provider, &plan).await;

        assert_eq!(summary.considered, 6);
        assert_eq!(summary.posted, 2, "the default cap is two notes per round");
        assert_eq!(summary.rolled_up, 4);
        assert_eq!(summary.policy_excluded, 0);
        assert_eq!(summary.failed, 0);
        assert!(!summary.summary_only);

        let posted: Vec<String> = provider.posted().into_iter().map(|(file, _, _)| file).collect();
        assert_eq!(posted, vec!["src/f1.rs", "src/f2.rs"]);

        let board = render_board(&output, &plan, &policy);
        assert!(board.contains("# CodeReview Board"));
        assert!(
            board.contains("`src/f3.rs:3`"),
            "the four rolled-up findings are on the board"
        );
        assert!(board.contains("`src/f4.rs:4`"));
        assert!(board.contains("`src/f5.rs:5`"));
        assert!(board.contains("`src/f6.rs:6`"));
        assert!(
            board.contains("exceeded the 2-note per-round cap"),
            "the board explains why they were not posted inline"
        );
        assert!(
            !board.contains("`src/f1.rs:1`"),
            "the two posted findings are not duplicated in the policy section"
        );
    }
}
