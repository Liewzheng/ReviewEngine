//! Inline-note delivery policy (RENG-63).
//!
//! RENG-60 made the *set* of inline notes correct — consolidated, deduplicated,
//! adjudicated, and anchored inside the reviewed diff — but every Critical/High
//! finding still became a note. The production corpus shows what that costs:
//! 321 eligible findings turned into 214 inline notes, 87 % of them High or
//! above, 57 % of them noise (duplicate, stale, out-of-scope or wrong), a worst
//! case of 2 500 comments per 1 000 changed lines on a two-line MR, and 25
//! out-of-scope notes that were mostly demands about prose
//! (`reports/reng-59-comment-inventory.md` §3.1, §4.7, §5.3).
//!
//! This module is the delivery policy those findings pass through:
//!
//! - **thresholds** — a severity floor, a confidence floor, an actionability
//!   requirement (a non-empty recommendation), plus the existing in-diff anchor
//!   requirement ([`PublishPolicy::admits`], the anchor gate stays in
//!   [`crate::publisher::DiffIndex`]);
//! - **a per-round cap** — at most `max_inline_notes_per_round` notes; the
//!   findings that pass the policy but do not make the cut are *rolled up* into
//!   the board instead of being posted;
//! - **change-type adaptation** — a documentation/CI-only change is published
//!   summary-only (board comment, zero inline notes) unless the policy is
//!   explicitly told otherwise.
//!
//! Nothing the policy refuses is *dropped*: every finding stays in the board,
//! whose per-expert sections are rendered from `output.reports` and are
//! untouched by this module. The policy decides only what deserves a
//! line-anchored comment on top of that.
//!
//! The planning half of the module is pure (no provider, no I/O), so the
//! selection — including the ranking — is unit-testable and deterministic:
//! the same findings, diff and policy always produce the same plan.

use crate::models::{Finding, Severity};

/// Environment variable names that override [`PublishPolicy`] (RENG-63).
///
/// The policy is constructed in code with the defaults below; these four
/// variables are the only override surface, so operators can tune delivery
/// without a new configuration section.
pub const ENV_MIN_SEVERITY: &str = "REVIEW_PUBLISH_MIN_SEVERITY";
/// Integer `0`–`10`; see [`PublishPolicy::min_confidence`].
pub const ENV_MIN_CONFIDENCE: &str = "REVIEW_PUBLISH_MIN_CONFIDENCE";
/// Integer; see [`PublishPolicy::max_inline_notes_per_round`].
pub const ENV_MAX_INLINE_NOTES: &str = "REVIEW_PUBLISH_MAX_INLINE_NOTES";
/// Boolean; see [`PublishPolicy::inline_on_docs_only`].
pub const ENV_INLINE_ON_DOCS_ONLY: &str = "REVIEW_PUBLISH_INLINE_ON_DOCS_ONLY";

/// Documentation file extensions (lower-case, without the dot).
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "rst", "adoc", "asciidoc"];

/// Directory names that mark every file below them as documentation.
const DOC_DIRS: &[&str] = &["docs", "doc", "documentation", "man"];

/// Directory prefixes that mark every file below them as CI configuration.
const CI_DIR_PREFIXES: &[&str] = &[
    ".github/",
    ".gitlab/",
    ".circleci/",
    ".buildkite/",
    ".woodpecker/",
    ".travis/",
    ".ci/",
    "ci/",
];

/// CI configuration file names, recognised wherever they sit in the tree.
const CI_FILE_NAMES: &[&str] = &[
    ".gitlab-ci.yml",
    ".gitlab-ci.yaml",
    ".travis.yml",
    ".travis.yaml",
    "azure-pipelines.yml",
    "azure-pipelines.yaml",
    "appveyor.yml",
    "appveyor.yaml",
    ".drone.yml",
    "buildkite.yml",
    ".woodpecker.yml",
    ".woodpecker.yaml",
    "codecov.yml",
    ".codecov.yml",
    "jenkinsfile",
];

/// Legal / attribution file names (also matched with a `-suffix` or an
/// extension, e.g. `LICENSE-MIT`, `LICENSE.md`, `COPYING.txt`).
const LEGAL_FILE_NAMES: &[&str] = &["license", "licence", "copying", "notice", "authors", "contributors"];

/// Delivery policy for inline review notes.
///
/// Constructed in code with [`Default`] (the values in the field docs) and
/// optionally overridden from the environment with [`PublishPolicy::from_env`]
/// — there is deliberately no new configuration section for it.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishPolicy {
    /// Lowest severity that may be posted inline. Default: [`Severity::High`]
    /// — the corpus' inline population was 87 % High-or-above while the board's
    /// own mix was 45 % Medium, i.e. the old gate filtered almost nothing.
    pub min_severity: Severity,
    /// Lowest confidence (0–10) that may be posted inline. Default: **8** —
    /// below that the note is a guess, and the corpus' clearest false positive
    /// (`10347`, a Critical/10 SQL-injection claim against a constant string
    /// literal) shows confidence alone does not save a note, which is why the
    /// adjudication gate and the anchor gate stay in place as well.
    pub min_confidence: u8,
    /// Require a non-empty recommendation before a finding may be posted
    /// inline. Default: `true` — the inventory's out-of-scope class was largely
    /// findings with no actionable ask ("update the MR description", "the
    /// submodule bookkeeping is wrong").
    pub require_recommendation: bool,
    /// Maximum number of inline notes a single round may post. Default: **2**
    /// — the corpus' worst offender is a two-line MR with five bot comments
    /// (`github/kernel!1`: 2 500 comments per 1 000 changed lines), and the
    /// median MR carries four. Findings that pass the policy but exceed the cap
    /// are rolled up into the board, never dropped. `0` posts none.
    pub max_inline_notes_per_round: usize,
    /// Publish inline notes even when every changed file is documentation or CI
    /// configuration. Default: `false` — such a round is published summary-only
    /// (board comment, zero inline notes), because "code-quality doctrine on
    /// prose and CI glue" is exactly where the corpus' `AGENTS.md` / `README.md`
    /// / `build.sh` notes came from.
    pub inline_on_docs_only: bool,
}

impl Default for PublishPolicy {
    fn default() -> Self {
        Self {
            min_severity: Severity::High,
            min_confidence: 8,
            require_recommendation: true,
            max_inline_notes_per_round: 2,
            inline_on_docs_only: false,
        }
    }
}

impl PublishPolicy {
    /// The default policy with the `REVIEW_PUBLISH_*` environment overrides
    /// applied. This is what the publish path constructs.
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        policy.apply_overrides(|key| std::env::var(key).ok());
        policy
    }

    /// Apply the `REVIEW_PUBLISH_*` overrides supplied by `lookup`.
    ///
    /// Split out from [`Self::from_env`] so the parsing rules — and the
    /// fail-open behaviour on an unparsable value, which warns and keeps the
    /// default — are testable without mutating the process environment.
    pub fn apply_overrides<F>(&mut self, lookup: F)
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(value) = lookup(ENV_MIN_SEVERITY) {
            match parse_severity(&value) {
                Some(severity) => self.min_severity = severity,
                None => tracing::warn!(
                    "Ignoring {ENV_MIN_SEVERITY}={value:?}: expected critical|high|medium|low|note; \
                     keeping {}",
                    self.min_severity
                ),
            }
        }
        if let Some(value) = lookup(ENV_MIN_CONFIDENCE) {
            match value.trim().parse::<u8>() {
                Ok(confidence) if confidence <= 10 => self.min_confidence = confidence,
                _ => tracing::warn!(
                    "Ignoring {ENV_MIN_CONFIDENCE}={value:?}: expected an integer 0-10; keeping {}",
                    self.min_confidence
                ),
            }
        }
        if let Some(value) = lookup(ENV_MAX_INLINE_NOTES) {
            match value.trim().parse::<usize>() {
                Ok(cap) => self.max_inline_notes_per_round = cap,
                Err(_) => tracing::warn!(
                    "Ignoring {ENV_MAX_INLINE_NOTES}={value:?}: expected a non-negative integer; keeping {}",
                    self.max_inline_notes_per_round
                ),
            }
        }
        if let Some(value) = lookup(ENV_INLINE_ON_DOCS_ONLY) {
            match parse_bool(&value) {
                Some(enabled) => self.inline_on_docs_only = enabled,
                None => tracing::warn!(
                    "Ignoring {ENV_INLINE_ON_DOCS_ONLY}={value:?}: expected a boolean; keeping {}",
                    self.inline_on_docs_only
                ),
            }
        }
    }

    /// Whether `finding` passes the delivery thresholds.
    ///
    /// The anchored-in-the-diff requirement is separate: the caller applies it
    /// with [`crate::publisher::DiffIndex`], because only the reviewed diff
    /// knows where a note can legally land.
    pub fn admits(&self, finding: &Finding) -> bool {
        crate::team::adjudicator::severity_rank(&finding.severity)
            >= crate::team::adjudicator::severity_rank(&self.min_severity)
            && finding.confidence >= self.min_confidence
            && (!self.require_recommendation || !finding.recommendation.trim().is_empty())
    }

    /// Whether an inline note may be posted for a change that is
    /// documentation/CI-only (see [`is_docs_or_ci_only`]).
    pub fn allows_inline_for(&self, docs_only: bool) -> bool {
        !docs_only || self.inline_on_docs_only
    }

    /// One-line description of the policy, used in the board and the logs.
    pub fn describe(&self) -> String {
        format!(
            "severity >= {}{}, {} inline note(s) per round{}{}",
            self.min_severity,
            if self.min_confidence > 0 {
                format!(", confidence >= {}/10", self.min_confidence)
            } else {
                String::new()
            },
            self.max_inline_notes_per_round,
            if self.require_recommendation {
                ", an actionable recommendation"
            } else {
                ""
            },
            if self.inline_on_docs_only {
                ""
            } else {
                ", docs/CI-only changes summary-only"
            },
        )
    }
}

/// Parse a severity label (`critical|high|medium|low|note`, case-insensitive).
fn parse_severity(value: &str) -> Option<Severity> {
    match value.trim().to_ascii_lowercase().as_str() {
        "critical" => Some(Severity::Critical),
        "high" => Some(Severity::High),
        "medium" => Some(Severity::Medium),
        "low" => Some(Severity::Low),
        "note" => Some(Severity::Note),
        _ => None,
    }
}

/// Parse a boolean flag (`1/true/yes/on` and `0/false/no/off`, case-insensitive).
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Whether one changed path is documentation, legal text or CI configuration.
///
/// The predicate is deliberately narrow and readable, because a false positive
/// silences inline notes for a round while a false negative only costs a note:
///
/// - documentation by extension — `.md`, `.mdx`, `.rst`, `.adoc`, `.asciidoc`;
/// - documentation by directory — any file below `docs/`, `doc/`,
///   `documentation/` or `man/` (at any depth);
/// - legal/attribution files — `LICENSE`, `LICENCE`, `COPYING`, `NOTICE`,
///   `AUTHORS`, `CONTRIBUTORS`, also with a suffix or extension
///   (`LICENSE-MIT`, `NOTICE.md`);
/// - CI by directory — anything below `.github/`, `.gitlab/`, `.circleci/`,
///   `.buildkite/`, `.woodpecker/`, `.travis/`, `.ci/` or `ci/` (so
///   `.github/workflows/ci.yml` counts);
/// - CI by file name — `.gitlab-ci.yml`, `.travis.yml`, `azure-pipelines.yml`,
///   `appveyor.yml`, `.drone.yml`, `buildkite.yml`, `.woodpecker.yml`,
///   `codecov.yml`, `Jenkinsfile`.
///
/// `AGENTS.md`, `README.md`, `CHANGELOG.md` and `config.toml` at the root:
/// the first three are documentation (extension), `config.toml` is **not**
/// (it is shipped configuration, and the corpus' config findings were real).
pub fn is_docs_or_ci_path(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    let lower = normalized.trim_start_matches("./").to_ascii_lowercase();
    let segments: Vec<&str> = lower.split('/').filter(|segment| !segment.is_empty()).collect();
    let Some(file_name) = segments.last().copied() else {
        return false;
    };

    if file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| DOC_EXTENSIONS.contains(&extension))
    {
        return true;
    }
    if segments[..segments.len() - 1].iter().any(|dir| DOC_DIRS.contains(dir)) {
        return true;
    }
    if LEGAL_FILE_NAMES.iter().any(|name| {
        file_name == *name || file_name.starts_with(&format!("{name}-")) || file_name.starts_with(&format!("{name}."))
    }) {
        return true;
    }
    if CI_DIR_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return true;
    }
    CI_FILE_NAMES.contains(&file_name)
}

/// Whether a change set touches documentation/CI **only**.
///
/// Returns `false` for an empty file list: an unknown change set must never be
/// silently downgraded to summary-only (fail-open, same policy as the anchor
/// gate's "no diff available" case).
pub fn is_docs_or_ci_only<I, S>(files: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = false;
    for file in files {
        seen = true;
        if !is_docs_or_ci_path(file.as_ref()) {
            return false;
        }
    }
    seen
}

/// The delivery decision for one round of findings.
///
/// Pure data: [`plan_inline_notes`] computes it before anything is posted, so
/// the board can describe the decision and the publisher can carry it out.
#[derive(Debug, Default)]
pub struct InlinePlan<'a> {
    /// Findings inspected.
    pub considered: usize,
    /// True when the change set was detected as documentation/CI-only.
    pub docs_only: bool,
    /// Findings to post inline, highest-ranked first.
    pub selected: Vec<&'a Finding>,
    /// Findings that passed the policy and have a usable anchor but are not
    /// posted inline — the per-round cap, or summary-only mode. They are
    /// reported in the board section.
    pub rolled_up: Vec<&'a Finding>,
    /// Findings below a threshold (severity, confidence, actionability); board
    /// only.
    pub policy_excluded: usize,
    /// Findings that passed the policy but carry no line, so they can never
    /// carry an anchor.
    pub not_eligible: usize,
    /// Findings whose anchor the provider would reject: an unsafe path, or a
    /// line that is not part of the reviewed diff.
    pub skipped: usize,
}

impl InlinePlan<'_> {
    /// The board section describing this round's inline-note decision.
    ///
    /// Empty when there is nothing to say — a round that posted every finding
    /// it admitted leaves the board exactly as it was. Otherwise it names the
    /// findings that passed the policy but were withheld from inline delivery,
    /// which the board did not carry before RENG-63: without it, a capped
    /// finding existed only inside the per-expert sections, and a
    /// documentation-only round gave no reason for having posted no notes.
    pub fn board_section(&self, policy: &PublishPolicy) -> String {
        if self.rolled_up.is_empty() && !self.docs_only {
            return String::new();
        }

        let mut out = String::from("## Inline notes — delivery policy\n\n");
        out.push_str(&format!("**Policy**: {}.\n\n", policy.describe()));

        if self.docs_only {
            out.push_str(
                "> 📄 **Documentation/CI-only change** — this round is published as a summary \
                 (this board) only; no inline notes were posted.\n\n",
            );
        }

        if !self.rolled_up.is_empty() {
            if self.docs_only {
                out.push_str(&format!(
                    "**{} finding(s) admitted by the policy were withheld from inline delivery**:\n\n",
                    self.rolled_up.len(),
                ));
            } else {
                out.push_str(&format!(
                    "**{} finding(s) admitted by the policy exceeded the {}-note per-round cap** — \
                     reported here instead of inline:\n\n",
                    self.rolled_up.len(),
                    policy.max_inline_notes_per_round,
                ));
            }
            for finding in &self.rolled_up {
                out.push_str(&format!(
                    "- **[{}]** `{}` — {} (Confidence: {}/10)\n",
                    finding.severity,
                    super::inline_anchor(finding),
                    finding.title,
                    finding.confidence,
                ));
            }
            out.push('\n');
        }

        out
    }
}

/// Decide which findings of a round are posted inline.
///
/// Order of the gates, cheapest first: the policy thresholds, the line
/// requirement, the unsafe-path guard, then the in-diff anchor check (skipped
/// when no [`crate::publisher::DiffIndex`] is available — fail-open, the
/// provider then has the final word). The survivors are ranked by
/// [`rank_inline_candidates`] and truncated to the per-round cap; a
/// documentation/CI-only change selects none (`policy.inline_on_docs_only`
/// restores them).
///
/// Two orders come out of this, each for its own reason: `selected` is back in
/// the input (consolidated) order, so posting keeps RENG-60's guarantee that
/// the inline set is the admitted subset of the consolidated set in its order;
/// `rolled_up` is in *ranked* order, because it is a "what we would have
/// flagged next" list and the next-best finding belongs at its top.
pub fn plan_inline_notes<'a, I>(
    findings: I,
    diff: Option<&crate::publisher::DiffIndex>,
    docs_only: bool,
    policy: &PublishPolicy,
) -> InlinePlan<'a>
where
    I: IntoIterator<Item = &'a Finding>,
{
    let mut plan = InlinePlan {
        docs_only,
        ..Default::default()
    };
    // The source index travels with the candidate so the selected notes can be
    // restored to the order they were published in before RENG-63.
    let mut candidates: Vec<(usize, &Finding)> = Vec::new();

    for (index, finding) in findings.into_iter().enumerate() {
        plan.considered += 1;
        if !policy.admits(finding) {
            plan.policy_excluded += 1;
            continue;
        }
        let Some(line) = finding.line else {
            plan.not_eligible += 1;
            continue;
        };
        if !super::is_safe_inline_path(&finding.file) {
            tracing::warn!("Skipping inline note for unsafe file path: {}", finding.file);
            plan.skipped += 1;
            continue;
        }
        if let Some(diff) = diff {
            if !diff.contains(&finding.file, line) {
                tracing::info!(
                    file = %finding.file,
                    line,
                    "Skipping inline note: the line is not part of the reviewed diff"
                );
                plan.skipped += 1;
                continue;
            }
        }
        candidates.push((index, finding));
    }

    candidates.sort_by(|(_, a), (_, b)| rank_inline_candidates_cmp(a, b));

    if !policy.allows_inline_for(docs_only) {
        plan.rolled_up = candidates.into_iter().map(|(_, finding)| finding).collect();
        return plan;
    }

    let overflow = candidates.split_off(candidates.len().min(policy.max_inline_notes_per_round));
    plan.rolled_up = overflow.into_iter().map(|(_, finding)| finding).collect();
    // Back to the order the findings were published in before RENG-63.
    candidates.sort_by_key(|(index, _)| *index);
    plan.selected = candidates.into_iter().map(|(_, finding)| finding).collect();
    plan
}

/// Rank inline candidates for the per-round cap, best first.
pub fn rank_inline_candidates(candidates: &mut [&Finding]) {
    candidates.sort_by(|a, b| rank_inline_candidates_cmp(a, b));
}

/// The documented ranking rule: **severity descending** (critical → note),
/// then **confidence descending**, then **file path ascending**, **line
/// ascending**, **title ascending** and finally **expert name ascending**.
///
/// The last four are tie-breakers that make the order a total order
/// independent of the input order, so the same findings always select the same
/// notes under the per-round cap.
fn rank_inline_candidates_cmp(a: &Finding, b: &Finding) -> std::cmp::Ordering {
    use crate::team::adjudicator::severity_rank;
    severity_rank(&b.severity)
        .cmp(&severity_rank(&a.severity))
        .then_with(|| b.confidence.cmp(&a.confidence))
        .then_with(|| a.file.cmp(&b.file))
        .then_with(|| a.line.unwrap_or(u32::MAX).cmp(&b.line.unwrap_or(u32::MAX)))
        .then_with(|| a.title.cmp(&b.title))
        .then_with(|| a.expert_name.cmp(&b.expert_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Effort, Finding};

    fn finding(file: &str, line: Option<u32>, severity: Severity, confidence: u8, recommendation: &str) -> Finding {
        Finding {
            file: file.to_string(),
            line,
            line_end: None,
            severity,
            confidence,
            category: "security".to_string(),
            title: format!("issue in {file}"),
            summary: String::new(),
            evidence: String::new(),
            impact: String::new(),
            recommendation: recommendation.to_string(),
            effort: Effort::Small,
            expert_name: "lead".to_string(),
            expert_role: String::new(),
            agrees_with: Vec::new(),
            references: Vec::new(),
        }
    }

    // ── thresholds ─────────────────────────────────────────────────────────

    #[test]
    fn default_policy_documents_its_floors() {
        let policy = PublishPolicy::default();
        assert_eq!(policy.min_severity, Severity::High);
        assert_eq!(policy.min_confidence, 8);
        assert!(policy.require_recommendation);
        assert_eq!(policy.max_inline_notes_per_round, 2);
        assert!(!policy.inline_on_docs_only, "docs/CI-only rounds are summary-only");
    }

    #[test]
    fn admits_applies_every_floor() {
        let policy = PublishPolicy::default();
        assert!(policy.admits(&finding("a.rs", Some(1), Severity::Critical, 10, "fix it")));
        assert!(policy.admits(&finding("a.rs", Some(1), Severity::High, 8, "fix it")));

        assert!(
            !policy.admits(&finding("a.rs", Some(1), Severity::Medium, 10, "fix it")),
            "below the severity floor"
        );
        assert!(
            !policy.admits(&finding("a.rs", Some(1), Severity::High, 7, "fix it")),
            "below the confidence floor"
        );
        assert!(
            !policy.admits(&finding("a.rs", Some(1), Severity::Critical, 10, "   ")),
            "no actionable recommendation"
        );
    }

    #[test]
    fn recommendation_requirement_can_be_switched_off() {
        let policy = PublishPolicy {
            require_recommendation: false,
            ..Default::default()
        };
        assert!(policy.admits(&finding("a.rs", Some(1), Severity::High, 9, "")));
    }

    // ── env overrides ──────────────────────────────────────────────────────

    #[test]
    fn apply_overrides_reads_every_knob() {
        let mut policy = PublishPolicy::default();
        policy.apply_overrides(|key| match key {
            ENV_MIN_SEVERITY => Some("critical".to_string()),
            ENV_MIN_CONFIDENCE => Some("9".to_string()),
            ENV_MAX_INLINE_NOTES => Some("5".to_string()),
            ENV_INLINE_ON_DOCS_ONLY => Some("true".to_string()),
            _ => None,
        });

        assert_eq!(policy.min_severity, Severity::Critical);
        assert_eq!(policy.min_confidence, 9);
        assert_eq!(policy.max_inline_notes_per_round, 5);
        assert!(policy.inline_on_docs_only);
        assert!(policy.admits(&finding("a.rs", Some(1), Severity::Critical, 9, "fix")));
        assert!(!policy.admits(&finding("a.rs", Some(1), Severity::High, 10, "fix")));
    }

    #[test]
    fn apply_overrides_fails_open_on_a_malformed_value() {
        let mut policy = PublishPolicy::default();
        policy.apply_overrides(|key| match key {
            ENV_MIN_SEVERITY => Some("urgent".to_string()),
            ENV_MIN_CONFIDENCE => Some("11".to_string()),
            ENV_MAX_INLINE_NOTES => Some("two".to_string()),
            ENV_INLINE_ON_DOCS_ONLY => Some("maybe".to_string()),
            _ => None,
        });
        assert_eq!(policy, PublishPolicy::default(), "a bad value never disables delivery");

        // Zero is a legal (if drastic) cap and a legal confidence floor.
        let mut zero = PublishPolicy::default();
        zero.apply_overrides(|key| match key {
            ENV_MAX_INLINE_NOTES => Some("0".to_string()),
            ENV_MIN_CONFIDENCE => Some("0".to_string()),
            _ => None,
        });
        assert_eq!(zero.max_inline_notes_per_round, 0);
        assert_eq!(zero.min_confidence, 0);
    }

    // ── docs/CI-only detection ─────────────────────────────────────────────

    #[test]
    fn docs_or_ci_paths_are_recognised() {
        for path in [
            "AGENTS.md",
            "README.md",
            "CHANGELOG.md",
            "docs/configuration.md",
            "doc/api.rst",
            "src/docs/design.adoc",
            "LICENSE",
            "LICENSE-MIT",
            "NOTICE.md",
            ".github/workflows/ci.yml",
            ".github/pull_request_template.md",
            ".gitlab/issue_templates/bug.md",
            ".gitlab-ci.yml",
            ".travis.yml",
            "ci/build.sh",
            ".circleci/config.yml",
            "Jenkinsfile",
        ] {
            assert!(is_docs_or_ci_path(path), "{path} should count as docs/CI");
        }
    }

    #[test]
    fn code_and_config_paths_are_not_docs_or_ci() {
        for path in [
            "src/lib.rs",
            "src/main.rs",
            "config.toml",
            ".code-audit-config.toml",
            "crates/sirena-cli/build.rs",
            "scripts/deploy.sh",
            "Cargo.toml",
            "src/rt.rs",
            // A directory that merely starts with "doc" is not `docs/`.
            "docker/compose.yml",
        ] {
            assert!(!is_docs_or_ci_path(path), "{path} must not count as docs/CI");
        }
    }

    #[test]
    fn docs_or_ci_only_requires_every_path_to_qualify() {
        assert!(is_docs_or_ci_only([
            "AGENTS.md",
            "docs/x.md",
            ".github/workflows/ci.yml"
        ]));
        assert!(!is_docs_or_ci_only(["AGENTS.md", "src/lib.rs"]));
        assert!(
            !is_docs_or_ci_only(Vec::<String>::new()),
            "an unknown change set is never downgraded"
        );
    }

    // ── cap, ranking, summary-only ─────────────────────────────────────────

    #[test]
    fn plan_caps_the_round_and_rolls_the_rest_up() {
        let policy = PublishPolicy::default();
        let findings: Vec<Finding> = (1..=6)
            .map(|i| finding(&format!("src/f{i}.rs"), Some(i), Severity::High, 9, "fix it"))
            .collect();

        let plan = plan_inline_notes(&findings, None, false, &policy);
        assert_eq!(plan.considered, 6);
        assert_eq!(plan.selected.len(), 2);
        assert_eq!(plan.rolled_up.len(), 4);
        assert_eq!(plan.policy_excluded, 0);
        assert_eq!(plan.selected[0].file, "src/f1.rs");
        assert_eq!(plan.selected[1].file, "src/f2.rs");
        assert_eq!(plan.rolled_up[0].file, "src/f3.rs");
    }

    #[test]
    fn plan_ranks_by_severity_then_confidence_then_position() {
        let policy = PublishPolicy {
            max_inline_notes_per_round: 1,
            ..Default::default()
        };
        let findings = vec![
            finding("src/z.rs", Some(9), Severity::High, 10, "fix"),
            finding("src/a.rs", Some(2), Severity::Critical, 8, "fix"),
            finding("src/b.rs", Some(3), Severity::Critical, 9, "fix"),
        ];
        let plan = plan_inline_notes(&findings, None, false, &policy);
        assert_eq!(plan.selected[0].file, "src/b.rs", "critical + 9/10 wins");

        // Ties break on file, then line: independent of the input order.
        let reversed: Vec<Finding> = findings.into_iter().rev().collect();
        let plan = plan_inline_notes(&reversed, None, false, &policy);
        assert_eq!(plan.selected[0].file, "src/b.rs");
    }

    #[test]
    fn ranking_is_deterministic_for_equal_keys() {
        let a = finding("src/a.rs", Some(1), Severity::High, 9, "fix");
        let b = finding("src/a.rs", Some(2), Severity::High, 9, "fix");
        let c = finding("src/a.rs", Some(2), Severity::High, 9, "fix");

        let mut one = vec![&a, &b, &c];
        let mut two = vec![&c, &a, &b];
        rank_inline_candidates(&mut one);
        rank_inline_candidates(&mut two);

        let anchors = |list: &[&Finding]| {
            list.iter()
                .map(|f| (f.file.clone(), f.line, f.title.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(anchors(&one), anchors(&two));
        assert_eq!(anchors(&one).len(), 3);
    }

    #[test]
    fn docs_only_round_selects_no_inline_note_even_for_critical_findings() {
        let policy = PublishPolicy::default();
        let findings = vec![
            finding("docs/design.md", Some(4), Severity::Critical, 10, "fix the doc"),
            finding("AGENTS.md", Some(9), Severity::High, 9, "fix the doc"),
        ];
        let docs_only = is_docs_or_ci_only(["docs/design.md", "AGENTS.md"]);
        assert!(docs_only);

        let plan = plan_inline_notes(&findings, None, docs_only, &policy);
        assert!(plan.selected.is_empty(), "zero inline notes for a docs-only round");
        assert_eq!(plan.rolled_up.len(), 2, "they are reported on the board instead");

        // The policy escape restores inline delivery.
        let escaped = PublishPolicy {
            inline_on_docs_only: true,
            ..Default::default()
        };
        let plan = plan_inline_notes(&findings, None, docs_only, &escaped);
        assert_eq!(plan.selected.len(), 2);
        assert!(plan.rolled_up.is_empty());

        // A mixed change set is not summary-only.
        assert!(!is_docs_or_ci_only(["README.md", "src/lib.rs"]));
    }

    #[test]
    fn summary_only_board_section_names_the_withheld_findings() {
        let policy = PublishPolicy::default();
        let findings = vec![
            finding("docs/design.md", Some(4), Severity::Critical, 10, "fix the doc"),
            finding("AGENTS.md", Some(9), Severity::High, 9, "fix the doc"),
        ];
        let plan = plan_inline_notes(&findings, None, true, &policy);
        let section = plan.board_section(&policy);
        assert!(section.contains("Documentation/CI-only change"));
        assert!(
            section.contains("`docs/design.md:4`"),
            "the withheld findings are named: {section}"
        );
        assert!(section.contains("`AGENTS.md:9`"));
        assert!(section.contains("Confidence: 9/10"));
    }

    #[test]
    fn board_section_is_empty_when_nothing_was_withheld() {
        let policy = PublishPolicy::default();
        let findings = vec![finding("src/a.rs", Some(1), Severity::High, 9, "fix")];
        let plan = plan_inline_notes(&findings, None, false, &policy);
        assert_eq!(plan.selected.len(), 1);
        assert!(
            plan.board_section(&policy).is_empty(),
            "an unaffected board stays unchanged"
        );
    }

    #[test]
    fn board_section_lists_the_capped_findings() {
        let policy = PublishPolicy::default();
        let findings: Vec<Finding> = (1..=4)
            .map(|i| finding(&format!("src/f{i}.rs"), Some(i), Severity::High, 9, "fix it"))
            .collect();
        let plan = plan_inline_notes(&findings, None, false, &policy);
        let section = plan.board_section(&policy);
        assert!(section.contains("exceeded the 2-note per-round cap"));
        assert!(section.contains("`src/f3.rs:3`") && section.contains("`src/f4.rs:4`"));
        assert!(!section.contains("`src/f1.rs:1`"), "posted findings are not re-listed");
    }
}
