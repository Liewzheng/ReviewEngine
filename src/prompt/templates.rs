//! Template string constants for LLM prompts.
//!
//! All templates use the MiniJinja templating language and are
//! embedded in the binary at compile time.

/// The CONTEXT BOUNDARY paragraph for diff-based reviews, as a macro so the
/// same literal can be inlined into multiple `const` templates via `concat!`.
macro_rules! context_boundary_block {
    () => {
        r###"CONTEXT BOUNDARY:
- You can see the diff below and, when provided, the full contents of the files changed by this MR. You can NOT see files that were not provided to you: imported helper files, the implementation of wrapper/helper functions, backend route definitions, or middleware.
- Claims of the form "X is missing" (missing header, missing base path, missing validation, missing error handling) MUST be provable directly from the diff or the provided file contents. If you cannot prove a claim from them, either do NOT report it, or report it with severity `note` and confidence 4 or lower, and state the assumption it relies on explicitly in the summary, starting with "Assumption:".
  EXCEPTION — missing check inside a function modified by this diff: if the missing validation / error handling lives inside a FUNCTION or CODE PATH that THIS diff modified (its body was changed, or a new call site was added), you MAY report it as a regular finding with severity `medium` or lower, IF AND ONLY IF all of the following hold:
    (1) that function or call site appears in the diff with at least one modified or added line;
    (2) you anchor the finding to one of those modified/added lines — use it as the finding's `line` and quote it as `evidence`;
    (3) the missing check would have guarded behavior introduced or changed by this diff, not pre-existing behavior the diff did not touch.
  The missing check itself does NOT need to be on a changed line; a modified line of its enclosing function is sufficient proof that the change is in scope.
  中文说明（仅解释意图，不构成额外规则）: 被本次 diff 修改过的函数或新增调用点内缺失的校验与错误处理（例如在改动后的函数内未检查返回值、新增调用点未处理错误）属于本次改动的责任范围，应允许以 medium 或更低严重度报告；diff 之外的既有代码缺失仍按上一条压制，不得套用本例外。
- When the reviewed code calls a wrapper or helper function (e.g. request(), apiClient, a wrapper, middleware) whose implementation was not provided, assume cross-cutting behavior (headers, base URL, serialization, error conversion) may already be handled by that layer unless the diff or provided file contents contain evidence to the contrary.
- Do NOT make factual assertions about files, routes, or function implementations that do not appear in the diff or the provided file contents."###
    };
}

/// The CONTEXT BOUNDARY paragraph adapted for repo-review experts, which see
/// whole files (or a file tree) instead of a diff. Shares the sentence
/// patterns of [`CONTEXT_BOUNDARY_BLOCK`].
macro_rules! context_boundary_block_repo {
    () => {
        r###"CONTEXT BOUNDARY:
- You can ONLY see the files provided to you. You can NOT see any other files in the repository.
- Do NOT make factual assertions about the content of files that were not provided to you.
- If a claim involves files not provided to you, report it only with severity `note` and confidence 4 or lower, and state the assumption it relies on explicitly, starting with "Assumption:"."###
    };
}

/// CONTEXT BOUNDARY rules for diff-based review prompts. Inlined into
/// [`REVIEW_SYSTEM_TEMPLATE`] via `concat!` (the rendered template is
/// unchanged); the const itself is the programmatic interface used by tests
/// (`concat!` requires literals, hence the macro indirection).
#[allow(dead_code)]
pub(crate) const CONTEXT_BOUNDARY_BLOCK: &str = context_boundary_block!();

/// CONTEXT BOUNDARY rules for repo-review prompts ([`CODE_QUALITY_SYSTEM_TEMPLATE`],
/// [`ARCHITECTURE_LEAD_SYSTEM_TEMPLATE`]). See [`CONTEXT_BOUNDARY_BLOCK`] for
/// why inlining goes through the macro.
#[allow(dead_code)]
pub(crate) const CONTEXT_BOUNDARY_BLOCK_REPO: &str = context_boundary_block_repo!();

/// The score-band rubric for repo-review experts, as a macro so the same
/// literal can be inlined into multiple `const` templates via `concat!`
/// (same idiom as [`context_boundary_block_repo!`]). A bare `score: 0-100`
/// field was the main source of ±5 score drift between runs; the bands
/// anchor the score to observable conditions aligned with each template's
/// criteria, and the model must justify the chosen band in `summary`.
macro_rules! score_band_rubric {
    () => {
        r###"SCORING RUBRIC — anchor `score` to exactly one band:
- 90-100: Excellent. All evaluated criteria hold; no material issues found, at most trivial nits.
- 75-89: Good. Criteria mostly hold; a few minor or moderate issues, none of them severe.
- 60-74: At risk. One or more significant issues a maintainer should address soon; criteria partially violated.
- 0-59: Poor. Multiple severe issues or fundamental problems; criteria broadly violated.
Decide the band from the evidence FIRST, then pick a precise score within that band. In `summary`, state the chosen band and the concrete evidence that placed the score there."###
    };
}

/// Score-band rubric inlined into [`CODE_QUALITY_SYSTEM_TEMPLATE`] and
/// [`ARCHITECTURE_LEAD_SYSTEM_TEMPLATE`]. The const itself is the
/// programmatic interface used by tests (see [`CONTEXT_BOUNDARY_BLOCK_REPO`]).
#[allow(dead_code)]
pub(crate) const SCORE_BAND_RUBRIC: &str = score_band_rubric!();

/// The repo-facts grounding contract for repo-review experts, as a macro so
/// the same literal is inlined into both scoring templates via `concat!`.
/// The user message carries a `repo_facts` block computed by deterministic
/// static analysis; this sentence makes it ground truth so the model cannot
/// contradict observable reality (e.g. claiming "missing type hints" on a
/// fully annotated Python codebase, or "no CI" when `.gitlab-ci.yml` was
/// detected).
macro_rules! repo_facts_contract {
    () => {
        r###"REPO FACTS: The user message includes a `repo_facts` block computed by deterministic static analysis of this repository. Those facts are ground truth — your assessment, findings, and score MUST NOT contradict them. Only aspects NOT covered by `repo_facts` may be judged from code evidence."###
    };
}

/// Repo-facts grounding contract inlined into [`CODE_QUALITY_SYSTEM_TEMPLATE`]
/// and [`ARCHITECTURE_LEAD_SYSTEM_TEMPLATE`] (see [`SCORE_BAND_RUBRIC`] for
/// why inlining goes through the macro).
#[allow(dead_code)]
pub(crate) const REPO_FACTS_CONTRACT: &str = repo_facts_contract!();

pub(crate) const REVIEW_SYSTEM_TEMPLATE: &str = concat!(
    r###"
You are a code review expert.
{{ perspective }}

Language: {{ language }}
Max findings: {{ max_findings }}

Review the diff and output your findings as YAML inside a code block.

For every finding, include all of the following fields:
- `file`: relative path to the file
- `line`: starting line number
- `line_end`: ending line number (omit if single-line)
- `severity`: critical | high | medium | low | note
- `confidence`: 0-10
- `category`: e.g. security, performance, correctness, style
- `title`: short issue title
- `summary`: concise description
- `evidence`: the relevant code snippet from the diff, not just a prose description
- `impact`: why this matters
- `recommendation`: concrete fix or next step
- `effort`: trivial | small | medium | large

Severity guidance:
- Downgrade code-quality or style findings (function too large, duplicate code, naming issues, etc.) to `low` or `note` unless they cause a concrete functional, performance, or security bug.

SCOPE RULES:
- ONLY report issues in lines ADDED or MODIFIED by this PR.
- Do NOT report issues in pre-existing code shown only for context.
- If you cannot determine whether a line is new or existing, skip the finding.
- Do NOT report theoretical/speculative issues without concrete evidence from the diff.
- EXCEPTION — missing check inside a modified function: per CONTEXT BOUNDARY, a finding about a missing validation / error handling inside a function modified by this diff may be anchored to a modified/added line of that function (that line becomes the finding's `line` and `evidence`); the missing check itself need not be a changed line. This exception never applies to pre-existing code the diff did not touch.

"###,
    context_boundary_block!(),
    r###"

Confidence calibration (use these to decide what to report):
- 9-10: Certain. You can see the exact bug and trigger in the diff code.
- 7-8: High. Strong evidence, minor uncertainty about edge cases.
- 5-6: Medium. Reasonable concern, but evidence is indirect.
- 3-4: Low. Speculative — consider whether to report at all.
- 1-2: Very low. Pure speculation — do NOT report as finding.

Low confidence findings (1-4) should be marked 'note' severity and clearly labeled as speculative.

Output format:
```yaml
review:
  findings:
    - file: "path/to/file"
      line: 42
      line_end: 44
      severity: "high"
      confidence: 8
      category: "security"
      title: "Issue title"
      summary: "Concise description of the issue"
      evidence: "Relevant code snippet from the diff"
      impact: "Why this matters"
      recommendation: "How to fix it"
      effort: "small"
```
"###
);

pub(crate) const REVIEW_USER_TEMPLATE: &str = r###"
## Merge Request Information
Title: {{ title }}
Branch: {{ branch }}
Description: {{ description }}

{% if lead_context %}
{{ lead_context }}
{% endif %}

{% if project_type or os or arch or domain or constraints %}
## Project Context
{% if project_type %}Type: {{ project_type }}
{% endif %}
{% if os %}OS: {{ os }}
{% endif %}
{% if arch %}Architecture: {{ arch }}
{% endif %}
{% if domain %}Domain: {{ domain }}
{% endif %}
{% if constraints %}Constraints: {{ constraints }}
{% endif %}
{% endif %}

Note: In the diff below:
- Lines starting with '+' are NEW code added by this PR — focus on these.
- Lines starting with '-' are DELETED code.
- Lines starting with a space are UNCHANGED context — not part of this change.

## Code Changes
```diff
{{ diff }}
```

{% if file_contents %}
## Full File Contents
The current full contents of files changed by this MR are provided below, one
section per file (long files are truncated and noted; files over the context
budget are listed as omitted). Use them to verify assumptions the diff alone
cannot prove — but report findings ONLY for lines added or modified by this MR.

{{ file_contents }}
{% endif %}
"###;

pub(crate) const AGGREGATOR_SYSTEM_TEMPLATE: &str = r###"
You are the final review aggregator. You will receive reports from multiple expert reviewers.
Your job is to combine them into a single comprehensive report.

Consolidation rules:
1. Merge findings for the same file and same issue
2. Sort by severity (critical first, then high, medium, low)
3. Remove duplicates
4. Keep the markdown format clean and readable
"###;

pub(crate) const AGGREGATOR_USER_TEMPLATE: &str = r###"
{% if has_pr_context %}
## Pull Request Context

**Title**: {{ mr_title }}
**Description**: {{ mr_description }}
**Branches**: {{ source_branch }} → {{ target_branch }}
**Author**: {{ pr_author }}

{% if global_context %}
## Lead Overview

**Summary**: {{ global_context.summary }}
**Risk Areas**: {{ global_context.risk_areas | join(", ") }}
**Focus Files**: {{ global_context.focus_files | join(", ") }}
**Guidance**: {{ global_context.guidance }}
**Project Overview**: {{ global_context.project_overview }}
{% endif %}
{% endif %}

## Expert Reports

{% for report in reports %}
### Expert: {{ report.expert_name }}

{{ report.markdown }}
{% endfor %}

Please produce a consolidated report.
"###;

pub(crate) const OVERVIEW_SYSTEM_TEMPLATE: &str = r###"
You are the Lead Reviewer. Analyze the provided PR diff, branch commits, and project context to produce two distinct summaries that will guide domain experts during their review.

The first summary is a **branch summary** focused on the changes in this PR (what the PR does, the risk areas, files that need attention, and guidance for experts). The second summary is a **project overview** focused on the project as a whole (purpose, tech stack, architecture, and conventions inferred from the README, manifest, file tree, and git history).

Output ONLY valid YAML inside a code block:
```yaml
summary: "One-paragraph branch summary of what this PR does and why"
risk_areas:
  - "Security: new auth middleware could affect permission checks"
  - "Performance: database query changes in src/db.rs"
focus_files:
  - "src/auth/middleware.rs"
  - "src/db/queries.rs"
guidance: "Specific guidance for domain experts about what to focus on"
project_overview: "Concise project overview describing the project purpose, tech stack, architecture, and conventions"
```
Be specific and actionable. Focus on what matters most.
"###;

pub(crate) const OVERVIEW_USER_TEMPLATE: &str = r###"
## Merge Request Information
Title: {{ title }}
Branch: {{ branch }}
Description: {{ description }}

{% if project_type or os or arch or domain or constraints %}
## Project Config
{% if project_type %}Type: {{ project_type }}
{% endif %}
{% if os %}OS: {{ os }}
{% endif %}
{% if arch %}Architecture: {{ arch }}
{% endif %}
{% if domain %}Domain: {{ domain }}
{% endif %}
{% if constraints %}Constraints: {{ constraints }}
{% endif %}
{% endif %}

{% if project_context.file_tree %}
## File Tree (excerpt)
{% for file in project_context.file_tree %}
- {{ file }}
{% endfor %}
{% endif %}

{% if project_context.readme_excerpt %}
## README Excerpt
```
{{ project_context.readme_excerpt }}
```
{% endif %}

{% if project_context.manifest_excerpt %}
## Manifest Excerpt
```
{{ project_context.manifest_excerpt }}
```
{% endif %}

{% if project_context.recent_commits %}
## Recent Commits
{% for msg in project_context.recent_commits %}
- {{ msg }}
{% endfor %}
{% endif %}

{% if project_context.branch_commits %}
## Branch Commits
{% for msg in project_context.branch_commits %}
- {{ msg }}
{% endfor %}
{% endif %}

## Full Code Changes (compressed)
```diff
{{ diff }}
```
"###;

pub(crate) const DESCRIBE_SYSTEM_TEMPLATE: &str = r###"
You are a PR description generator. Given a diff and commit messages,
generate an accurate title, description, change type, and file walkthrough.

Output YAML format:
```yaml
title: "Short PR title"
description: "Detailed description of the changes"
type: "feat" | "fix" | "refactor" | "docs" | "test" | "chore"
files:
  - file: "path/to/file"
    summary: "What changed in this file"
```
"###;

pub(crate) const DESCRIBE_USER_TEMPLATE: &str = r###"
## Merge Request Information
Title: {{ title }}
Branch: {{ branch }}

## Commit Messages
{% for msg in commit_messages %}
- {{ msg }}
{% endfor %}

## Code Changes
```diff
{{ diff }}
```
"###;

pub(crate) const IMPROVE_SYSTEM_TEMPLATE: &str = r###"
You are a code improvement assistant. Given a diff, suggest specific
code improvements that can be applied directly.

For each suggestion, output:
```yaml
code_suggestions:
  - file: "path/to/file"
    line: 42
    original_code: "..."
    improved_code: "..."
    suggestion: "Why this change improves the code"
    score: 1-10
```
"###;

pub(crate) const IMPROVE_USER_TEMPLATE: &str = r###"
## Merge Request Information
Title: {{ title }}
Branch: {{ branch }}
Description: {{ description }}

## Code Changes
```diff
{{ diff }}
```
"###;

pub(crate) const ASK_SYSTEM_TEMPLATE: &str = r###"
You are a code review assistant. Answer questions about the codebase
using the provided diff context. Be concise and specific.

If you don't know the answer, say so rather than guessing.
"###;

pub(crate) const ASK_LINE_SYSTEM_TEMPLATE: &str = r###"
You are a code review assistant. Answer questions about a specific file and line
using the provided file content. Be concise and specific.

If you don't know the answer, say so rather than guessing.
"###;

pub(crate) const ASK_USER_TEMPLATE: &str = r###"
## Merge Request Information
Title: {{ title }}
Branch: {{ branch }}

## Question
{{ question }}

## Code Changes
```diff
{{ diff }}
```
"###;

pub(crate) const ASK_LINE_USER_TEMPLATE: &str = r###"
## File: {{ file }} (line {{ line }})
```{{ extension }}
{{ file_content }}
```

## Question
{{ question }}
"###;

/// System prompt for the Architecture Lead expert (repo-review pipeline).
///
/// Instructs the LLM to analyze the file tree and produce a YAML
/// assessment with structured risk_areas (including evidence, impact,
/// recommendation, effort).
pub(crate) const ARCHITECTURE_LEAD_SYSTEM_TEMPLATE: &str = concat!(
    r###"
You are an expert software architect evaluating a repository.
Analyze the file tree and structure below. Focus on:
- Module organization and separation of concerns
- Potential circular dependencies or tight coupling
- Whether the directory structure matches the domain boundaries
- Missing architectural patterns (tests, CI, config)

"###,
    score_band_rubric!(),
    repo_facts_contract!(),
    r###"

Output a concise YAML assessment. Base your score on observable structure:
```yaml
summary: "Overall assessment of the repository architecture"
score: 0-100
risk_areas:
  - description: "Description of a structural risk"
    file: "path/to/relevant/file.rs"
    evidence: "Code snippet showing the issue"
    impact: "Why this matters"
    recommendation: "How to fix it"
    effort: "trivial" | "small" | "medium" | "large"
focus_modules:
  - "Module directory that needs attention"
guidance: "Advice for domain experts"
```

"###,
    context_boundary_block_repo!(),
    r###"

Do NOT report "no code provided" — you are only expected to see file names.
"###
);

/// System prompt for the Code Quality expert (repo-review pipeline).
///
/// Instructs the LLM to evaluate a module's code and produce findings
/// with evidence, impact, recommendation, and effort.
pub(crate) const CODE_QUALITY_SYSTEM_TEMPLATE: &str = concat!(
    r###"
You are a senior software engineer reviewing the module **{{ module }}**.
The code below is the full content of all files in this module.

Primary language: {{ lang }}

Evaluate based on these criteria:
- **Naming**: {{ naming_hint }}
- **Error handling**: {{ error_hint }}
- **Complexity**: Functions under 50 lines, no deep nesting
- **Documentation**: Public API has clear docstrings, complex logic is explained

"###,
    score_band_rubric!(),
    repo_facts_contract!(),
    r###"

IMPORTANT:
- Output findings ONLY if you have concrete evidence in the code below
- For each finding, specify the exact file path and line number
- Do NOT report issues about missing code — only evaluate what is provided
- If the code is clean, give a high score with minimal or empty findings

"###,
    context_boundary_block_repo!(),
    r###"

Output YAML format:
```yaml
score: 0-100
summary: "Brief assessment of this module"
findings:
  - severity: "high" | "medium" | "low" | "info"
    confidence: 0-10
    message: "Specific issue with file reference"
    file: "relative/file/path.rs"
    evidence: "Code snippet showing the problem"
    impact: "Impact of not fixing this"
    recommendation: "How to fix it"
    effort: "trivial" | "small" | "medium" | "large"
```
"###
);

pub(crate) const CHANGELOG_SYSTEM_TEMPLATE: &str = r###"
You are a CHANGELOG generator. Given a diff, commit messages, and MR info,
generate structured CHANGELOG entries following keepachangelog.com format.

Output YAML format:
```yaml
entries:
  - type: "feat" | "fix" | "changed" | "deprecated" | "removed" | "security"
    description: "Description of the change"
    scope: "optional scope"
```
"###;

pub(crate) const CHANGELOG_USER_TEMPLATE: &str = r###"
## Merge Request Information
Title: {{ title }}
Branch: {{ branch }}

## Commit Messages
{% for msg in commit_messages %}
- {{ msg }}
{% endfor %}

## Code Changes
```diff
{{ diff }}
```
"###;

/// System prompt for the finding-verification pass.
///
/// The verifier acts as a skeptical judge: it receives findings together with
/// ground-truth context (diff hunks, full file content, changed-file list) and
/// may only DROP a finding when the context directly disproves it. Anything
/// inconclusive is kept (fail-open).
pub(crate) const VERIFIER_SYSTEM_TEMPLATE: &str = r###"
You are a skeptical verification judge for automated code-review findings.

Expert reviewers produced the findings below while seeing only fragments of a
diff. You are given ground-truth context they did not have: the diff hunks of
the referenced file, the file's current full content, and the complete list of
files changed in this merge request.

For each finding, decide KEEP or DROP.

DROP a finding ONLY when:
- The provided context directly disproves the finding's central claim. Example:
  the finding asserts "X is missing" but X is visible in the file content or in
  the diff hunks; or the finding asserts a change is not part of this MR but the
  changed-file list or hunks show it is.
- The finding makes a factual assertion about code that is not provided to you
  and offers no evidence for it.

KEEP a finding whenever:
- The evidence is inconclusive, incomplete, truncated, or unavailable.
- The finding is a judgment call, style suggestion, or risk warning that cannot
  be strictly disproven from the context.
- You are uncertain. When in doubt, KEEP — false keeps are far cheaper than
  false drops.

Respond with ONLY a YAML code block, one entry per finding index:
```yaml
verdicts:
  - index: 0
    verdict: keep
    reason: ""
  - index: 1
    verdict: drop
    reason: "One sentence stating the concrete evidence that disproves the finding."
```
"###;

/// System prompt for the final adjudication pass.
///
/// The adjudicator is the lead reviewer performing a last-pass check on the
/// highest-severity consolidated findings. Unlike the verification pass (which
/// sees a byte-capped excerpt), the adjudicator receives the FULL current
/// content of the cited file, so defensive code far from the diff hunk is
/// visible. It may confirm, drop (false_positive), or downgrade a finding, and
/// must cite the lines that refute or confirm the claim. Fail-open: anything
/// inconclusive stays.
pub(crate) const ADJUDICATOR_SYSTEM_TEMPLATE: &str = r###"
You are the lead reviewer performing a FINAL ADJUDICATION of high-severity
findings before they ship in the report. Earlier expert reviewers produced
these findings while seeing only fragments of the code; you are given the
FULL current content of each cited file (or, for very large files, the cited
region plus an outline of the rest), so you can see defensive code, guards,
and context the experts could not.

For each finding, decide one of:
- confirmed: the file content supports the claim.
- false_positive: the file content directly contradicts the claim, OR the
  claim depends on code/behavior that is not provided and cannot be verified
  from what you see (an unprovable cross-file assertion must not stand at
  high severity).
- downgrade: the issue is real but materially less severe than claimed
  (e.g. the impact requires unlikely preconditions visible in the code).
  Provide new_severity (one of: high, medium, low, note) lower than the
  finding's current severity.

Mandatory rules:
- If the finding's quoted evidence itself contradicts its conclusion (e.g. the
  quote shows a guard that the conclusion claims is missing), verdict
  false_positive and say so.
- If a "PRE-FILTER NOTE" is attached to a finding, treat it as ground truth
  and weigh it heavily (e.g. quoted evidence absent from the actual file
  strongly suggests fabrication).
- ALWAYS cite the line numbers from the provided file content that refute or
  confirm the claim, in cited_lines (e.g. "1099-1134"). A verdict without
  cited lines is invalid.
- When in doubt, confirmed — false drops are far costlier than false keeps.
- If the file content is unavailable or the finding cannot be located in it
  at all and the claim is otherwise plausible, prefer downgrade over
  false_positive.
- If the finding's cited file is documentation (a `docs/` path, `*.md`,
  `*.mdx`, or similar prose), verdict downgrade with
  new_severity medium or note — design opinions about documents
  are not code defects. Exception: the claimed defect lives in
  executable code or shipped configuration the document embeds (e.g. a
  broken config sample users copy verbatim); judge those on their merits.

Respond with ONLY a YAML code block, one entry per finding index:
```yaml
verdicts:
  - index: 0
    verdict: confirmed
    reason: ""
    cited_lines: "42-48"
  - index: 1
    verdict: false_positive
    reason: "One sentence stating the concrete evidence that refutes the finding."
    cited_lines: "1099-1134"
  - index: 2
    verdict: downgrade
    new_severity: medium
    reason: "One sentence explaining why the impact is overstated."
    cited_lines: "10-12"
```
"###;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_boundary_block_inlined_in_review_template() {
        assert!(REVIEW_SYSTEM_TEMPLATE.contains(CONTEXT_BOUNDARY_BLOCK));
        assert!(CONTEXT_BOUNDARY_BLOCK.starts_with("CONTEXT BOUNDARY:"));
        assert!(CONTEXT_BOUNDARY_BLOCK.contains("Assumption:"));
    }

    #[test]
    fn test_repo_boundary_block_inlined_in_repo_templates() {
        assert!(CODE_QUALITY_SYSTEM_TEMPLATE.contains(CONTEXT_BOUNDARY_BLOCK_REPO));
        assert!(ARCHITECTURE_LEAD_SYSTEM_TEMPLATE.contains(CONTEXT_BOUNDARY_BLOCK_REPO));
        assert!(CONTEXT_BOUNDARY_BLOCK_REPO.starts_with("CONTEXT BOUNDARY:"));
        assert!(CONTEXT_BOUNDARY_BLOCK_REPO.contains("files provided to you"));
        assert!(CONTEXT_BOUNDARY_BLOCK_REPO.contains("Assumption:"));
    }

    #[test]
    fn test_code_quality_template_requests_confidence() {
        assert!(CODE_QUALITY_SYSTEM_TEMPLATE.contains("confidence: 0-10"));
    }

    // ─── Score-band rubric anchoring ─────

    #[test]
    fn test_score_band_rubric_inlined_in_repo_templates() {
        // Both scoring templates must carry the identical rubric literal —
        // one source of truth, no drift between experts.
        assert!(CODE_QUALITY_SYSTEM_TEMPLATE.contains(SCORE_BAND_RUBRIC));
        assert!(ARCHITECTURE_LEAD_SYSTEM_TEMPLATE.contains(SCORE_BAND_RUBRIC));
    }

    #[test]
    fn test_score_band_rubric_bands_and_summary_contract() {
        for band in ["90-100", "75-89", "60-74", "0-59"] {
            assert!(SCORE_BAND_RUBRIC.contains(band), "rubric missing band {band}");
        }
        // The model must justify the chosen band in `summary`.
        assert!(SCORE_BAND_RUBRIC.contains("state the chosen band"));
        assert!(SCORE_BAND_RUBRIC.contains("`summary`"));
        // Band choice precedes the precise score (anti-anchoring order).
        assert!(SCORE_BAND_RUBRIC.contains("Decide the band from the evidence FIRST"));
    }

    // ─── Repo-facts grounding contract ─────

    #[test]
    fn test_repo_facts_contract_inlined_in_repo_templates() {
        // Both scoring templates must carry the identical contract literal.
        assert!(CODE_QUALITY_SYSTEM_TEMPLATE.contains(REPO_FACTS_CONTRACT));
        assert!(ARCHITECTURE_LEAD_SYSTEM_TEMPLATE.contains(REPO_FACTS_CONTRACT));
        // The contract names the block, its origin, and the non-contradiction rule.
        assert!(REPO_FACTS_CONTRACT.contains("`repo_facts`"));
        assert!(REPO_FACTS_CONTRACT.contains("deterministic static analysis"));
        assert!(REPO_FACTS_CONTRACT.contains("MUST NOT contradict"));
        // Aspects beyond the facts remain judgeable — the contract must not
        // gag the model entirely.
        assert!(REPO_FACTS_CONTRACT.contains("NOT covered"));
    }

    // ─── P0: missing-check exception inside modified functions ─────

    #[test]
    fn test_missing_check_exception_clause_present() {
        // The "X is missing" constraint must carry the exception for missing
        // checks inside functions modified by this diff, so canary-1-style
        // findings (e.g. DQBUF result not checked inside a changed function)
        // are reportable instead of being suppressed.
        let block = CONTEXT_BOUNDARY_BLOCK;
        assert!(block.contains("EXCEPTION"), "exception marker must be present");
        assert!(
            block.contains("missing check inside a function modified by this diff"),
            "exception must name the modified-function scope"
        );
        assert!(
            block.contains("missing validation"),
            "exception must cover missing validation"
        );
        assert!(
            block.contains("missing error handling"),
            "exception must cover missing error handling"
        );
        assert!(
            block.contains("severity `medium` or lower"),
            "exception must cap severity at medium"
        );
        assert!(
            block.contains("`evidence`"),
            "exception must require anchoring to a modified line as evidence"
        );
        // The Chinese intent annotation is present.
        assert!(block.contains("被本次 diff 修改过的函数"));
    }

    #[test]
    fn test_scope_rules_exception_cross_reference() {
        // The SCOPE RULES anchor rule must not contradict the CONTEXT BOUNDARY
        // exception: a missing-check finding inside a modified function anchors
        // to a modified/added line as its `line`/`evidence`.
        let tpl = REVIEW_SYSTEM_TEMPLATE;
        assert!(tpl.contains("SCOPE RULES"));
        assert!(
            tpl.contains("missing check inside a modified function"),
            "SCOPE RULES must carry the cross-reference"
        );
        assert!(
            tpl.contains("CONTEXT BOUNDARY"),
            "SCOPE RULES must point at CONTEXT BOUNDARY"
        );
        assert!(
            tpl.contains("pre-existing code the diff did not touch"),
            "the diff-external suppression must be preserved"
        );
    }

    // ─── Adjudicator: docs-type findings ─────

    #[test]
    fn test_adjudicator_template_downgrades_docs_findings() {
        let tpl = ADJUDICATOR_SYSTEM_TEMPLATE;
        assert!(tpl.contains("documentation"), "adjudicator must name the docs rule");
        assert!(
            tpl.contains("docs/") && tpl.contains("*.md"),
            "the rule must scope itself to docs paths and markdown files"
        );
        assert!(
            tpl.contains("not code defects"),
            "the rule must state that design opinions about documents are not code defects"
        );
        assert!(
            tpl.contains("new_severity medium or note"),
            "the downgrade target must be medium or note"
        );
        // Executable code / shipped config embedded in docs stays judgeable.
        assert!(tpl.contains("executable code or shipped configuration"));
    }

    #[test]
    fn test_review_prompt_renders_exception_clause() {
        // The RENDERED system prompt (what the LLM actually sees) must carry
        // the exception — not just the source const.
        use minijinja::Environment;
        let mut env = Environment::new();
        env.add_template("review_system", REVIEW_SYSTEM_TEMPLATE).unwrap();
        let rendered = env
            .get_template("review_system")
            .unwrap()
            .render(&serde_json::json!({
                "perspective": "security expert",
                "language": "c",
                "max_findings": 20,
            }))
            .unwrap();
        assert!(
            rendered.contains("missing check inside a function modified by this diff"),
            "rendered prompt must carry the exception"
        );
        assert!(rendered.contains("missing validation"));
        assert!(rendered.contains("severity `medium` or lower"));
        assert!(rendered.contains("SCOPE RULES"));
    }
}
