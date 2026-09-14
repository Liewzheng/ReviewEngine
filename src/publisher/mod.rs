//! Publishing helper functions for review results on Git providers.
//!
//! This module provides the [`InlineNote`] struct and helper functions that
//! operate on a [`GitProvider`][crate::git_provider::GitProvider] to format and
//! publish review output. Platform-specific logic lives in the
//! `git_provider` implementations; this module only contains generic helpers
//! such as inline-note publishing and suggestion formatting.

use anyhow::Result;

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
/// the per-finding loop.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublishSummary {
    /// Findings inspected.
    pub considered: usize,
    /// Notes the provider accepted.
    pub posted: usize,
    /// Findings that can never carry an inline note (severity below High, or
    /// no line number).
    pub not_eligible: usize,
    /// Findings dropped by a gate: unsafe path, or a line that is not part of
    /// the reviewed diff.
    pub skipped: usize,
    /// Notes the provider rejected (permanent 4xx verdict, or retries spent).
    pub failed: usize,
}

/// The changed lines of the reviewed diff, keyed by file path.
///
/// Used to keep inline notes inside the diff: a finding whose line is not in
/// any changed hunk has no anchor the provider will accept, so posting it
/// earns a `400 ... line_code can't be blank` instead of a comment (corpus
/// §4.6 — five such rejections in four hours).
#[derive(Debug, Clone, Default)]
pub struct DiffIndex {
    files: std::collections::HashMap<String, Vec<(u32, u32)>>,
}

impl DiffIndex {
    /// Build the index from a unified diff.
    ///
    /// Files whose hunks change nothing on the new side (pure deletions) are
    /// absent: they have no valid anchor line.
    pub fn from_diff(diff_text: &str) -> Self {
        let mut files = std::collections::HashMap::new();
        for file in crate::diff::parser::parse_unified_diff(diff_text) {
            let ranges: Vec<(u32, u32)> = file
                .hunks
                .iter()
                .filter(|h| h.new_lines > 0)
                .map(|h| (h.new_start, h.new_start.saturating_add(h.new_lines.saturating_sub(1))))
                .collect();
            if !ranges.is_empty() {
                files.insert(file.path, ranges);
            }
        }
        Self { files }
    }

    /// True when nothing could be indexed (empty or oversized diff). Callers
    /// read that as "no index available", never as "nothing is in the diff".
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether `line` (1-based, new side) lies inside a changed hunk of `file`.
    pub fn contains(&self, file: &str, line: u32) -> bool {
        self.files
            .get(file)
            .is_some_and(|ranges| ranges.iter().any(|(start, end)| line >= *start && line <= *end))
    }
}

/// `file:line` (or `file:start-end`) anchor of a finding.
pub fn inline_anchor(finding: &crate::models::Finding) -> String {
    match (finding.line, finding.line_end) {
        (Some(start), Some(end)) if end > start => format!("{}:{start}-{end}", finding.file),
        (Some(start), _) => format!("{}:{start}", finding.file),
        (None, _) => finding.file.clone(),
    }
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

/// Whether a finding can carry an inline note at all (Critical/High + a line).
fn is_inline_eligible(finding: &crate::models::Finding) -> bool {
    use crate::models::Severity;
    (finding.severity == Severity::Critical || finding.severity == Severity::High) && finding.line.is_some()
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

/// Whether the output carries any finding that could become an inline note.
///
/// Lets the caller skip the diff fetch on a publish that has nothing to gate.
pub fn has_inline_candidates(output: &crate::models::ReviewOutput) -> bool {
    inline_publish_set(output).iter().any(|f| is_inline_eligible(f))
}

/// Publish inline notes for the Critical/High findings in `findings`.
///
/// Lower-severity findings and findings without a line are included in the
/// discussion board but never posted inline. Individual failures are isolated:
/// a rejected anchor or a spent retry is logged and the batch continues, so one
/// bad finding can no longer abort every later note of a review (corpus §4.6:
/// 25 inline attempts produced 20 notes because the first rejection returned
/// early). Pass `diff` to also require the anchor to be inside the reviewed
/// diff.
pub async fn publish_inline_notes(
    provider: &dyn crate::git_provider::GitProvider,
    findings: &[crate::models::Finding],
    diff: Option<&DiffIndex>,
) -> PublishSummary {
    let mut summary = PublishSummary::default();
    for finding in findings {
        publish_one(provider, finding, diff, &mut summary).await;
    }
    summary
}

/// Publish the inline notes of a review output.
///
/// Draws from [`inline_publish_set`] — the consolidated findings.
pub async fn publish_inline_notes_for_output(
    provider: &dyn crate::git_provider::GitProvider,
    output: &crate::models::ReviewOutput,
    diff: Option<&DiffIndex>,
) -> PublishSummary {
    let mut summary = PublishSummary::default();
    if output.consolidated.is_none() {
        tracing::warn!(
            "No consolidated report for this output — publishing inline notes from the raw per-expert findings"
        );
    }
    for finding in inline_publish_set(output) {
        publish_one(provider, finding, diff, &mut summary).await;
    }
    tracing::info!(
        "Inline notes: {} posted, {} skipped, {} failed, {} ineligible ({} findings considered){}",
        summary.posted,
        summary.skipped,
        summary.failed,
        summary.not_eligible,
        summary.considered,
        if diff.is_none() {
            " — no diff anchor check (diff unavailable)"
        } else {
            ""
        },
    );
    summary
}

/// Gate, render and post one finding; every outcome is recorded on `summary`.
async fn publish_one(
    provider: &dyn crate::git_provider::GitProvider,
    finding: &crate::models::Finding,
    diff: Option<&DiffIndex>,
    summary: &mut PublishSummary,
) {
    use crate::models::Severity;

    summary.considered += 1;
    let eligible = finding.severity == Severity::Critical || finding.severity == Severity::High;
    let Some(line) = finding.line.filter(|_| eligible) else {
        summary.not_eligible += 1;
        return;
    };

    // Defensive: validate file path before posting to prevent API abuse
    let file = finding.file.as_str();
    if file.contains("..") || file.starts_with('/') || file.starts_with('~') || file.contains('\0') {
        tracing::warn!("Skipping inline note for unsafe file path: {}", file);
        summary.skipped += 1;
        return;
    }
    if let Some(diff) = diff {
        if !diff.contains(file, line) {
            tracing::info!(
                file = %file,
                line,
                "Skipping inline note: the line is not part of the reviewed diff"
            );
            summary.skipped += 1;
            return;
        }
    }

    let body = format_inline_body(finding);
    match post_inline_with_retry(provider, file, line, &body).await {
        Ok(()) => summary.posted += 1,
        Err(err) => {
            summary.failed += 1;
            tracing::warn!(
                file = %file,
                line,
                error = %err,
                "Inline note failed — continuing with the remaining findings"
            );
        }
    }
}

/// POST one inline note, retrying transient failures with a bounded backoff.
///
/// Permanent errors (a 4xx verdict such as GitLab's blank `line_code`, or a
/// rejected path) are returned immediately: retrying cannot change the answer.
async fn post_inline_with_retry(
    provider: &dyn crate::git_provider::GitProvider,
    file: &str,
    line: u32,
    body: &str,
) -> Result<()> {
    let mut attempt = 1;
    loop {
        match provider.post_inline_comment(file, line, body).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt >= INLINE_POST_MAX_ATTEMPTS || !is_transient_error(&err) {
                    return Err(err);
                }
                let backoff = INLINE_POST_RETRY_BACKOFF * 2u32.pow(attempt - 1);
                tracing::warn!(
                    file = %file,
                    line,
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

/// Whether a failed inline post is worth retrying.
///
/// Transient means no HTTP verdict was produced (transport failure) or the
/// provider answered with a retryable status (408 / 429 / 5xx). Anything with a
/// 4xx verdict is permanent — the corpus' five `400 line_code can't be blank`
/// rejections are logged and skipped, never retried.
fn is_transient_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        let message = cause.to_string();
        let lowered = message.to_lowercase();
        if [
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
        ]
        .iter()
        .any(|needle| lowered.contains(needle))
        {
            return true;
        }
        if let Some(status) = http_status_code(&message) {
            return status == 408 || status == 429 || (500..600).contains(&status);
        }
    }
    false
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
    use crate::models::{Effort, Finding, Severity};

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
        publish_inline_notes(&provider, &findings, None).await;
        let called_files = provider.calls.lock().unwrap().clone();
        assert_eq!(called_files.len(), 1);
        assert!(called_files.contains(&"critical.rs".to_string()));
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
        publish_inline_notes(&provider, &findings, None).await;
        let called_files = provider.calls.lock().unwrap().clone();
        assert_eq!(called_files.len(), 1);
        assert!(called_files.contains(&"safe.rs".to_string()));
    }

    // ── RENG-60: consolidated publish set, anchors, failure isolation ────────

    /// Recording provider: keeps every `(file, line, body)` POST and can be
    /// told to fail a given file for its first N attempts with a chosen error.
    struct RecordingProvider {
        posts: std::sync::Mutex<Vec<(String, u32, String)>>,
        attempts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
        failures: std::collections::HashMap<String, (usize, String)>,
    }

    impl RecordingProvider {
        /// `failures` entries are `(file, failing_attempts, error_message)`.
        fn new(failures: &[(&str, usize, &str)]) -> Self {
            Self {
                posts: std::sync::Mutex::new(Vec::new()),
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
        }
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

        let output = crate::models::ReviewOutput {
            reports,
            aggregated: None,
            dropped_findings: Vec::new(),
            consolidated: Some(consolidated.clone()),
        };

        let provider = RecordingProvider::new(&[]);
        let summary = publish_inline_notes_for_output(&provider, &output, None).await;

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

        let summary = publish_inline_notes(&provider, &findings, None).await;

        let posted_files: Vec<String> = provider.posted().into_iter().map(|(file, _, _)| file).collect();
        assert_eq!(posted_files, vec!["one.rs", "three.rs"]);
        assert_eq!(summary.posted, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 0);
        // A 4xx verdict is permanent: exactly one attempt, never retried.
        assert_eq!(provider.attempts_for("two.rs"), 1);
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

        let summary = publish_inline_notes(&provider, &findings, None).await;

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

        let summary = publish_inline_notes(&provider, &findings, None).await;

        assert_eq!(provider.attempts_for("down.rs"), INLINE_POST_MAX_ATTEMPTS as usize);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.posted, 1, "the remaining findings are still published");
        assert_eq!(provider.posted()[0].0, "up.rs");
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
        let summary = publish_inline_notes(&provider, &findings, Some(&index)).await;

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

        // An unparsable/empty diff yields an empty index, which callers read as
        // "no index available" (never as "everything is outside the diff").
        assert!(DiffIndex::from_diff("").is_empty());
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
}
