//! M40 / RENG ticket compliance check (M39 §7 Option A).
//!
//! When [`crate::models::ComplianceConfig::enabled`] is true and an MR/PR
//! description references a recognised ticket (`Closes #N`, `Closes !N`,
//! `<work_item_URL>`, `owner/repo#N`), the review fetches each referenced
//! ticket and runs a single LLM call that compares the diff to the ticket's
//! requirements. The result is rendered as a `## Ticket compliance check`
//! section alongside the per-expert findings.
//!
//! Gating (verified by the M40 tests):
//! * `enabled = false` → zero extra LLM calls, zero extra HTTP calls.
//! * `enabled = true` and no refs found → zero extra calls.
//! * `enabled = true` and refs found → exactly one LLM call (one ticket-body
//!   fetch per ref, fail-soft: a fetch failure becomes a section note, not a
//!   review-blocking error).
//!
//! The reference implementation we mirror is pr-agent's
//! `ticket_pr_compliance_check.py` (GITLAB_TICKET_PATTERN / extract_* and
//! the four-bullet `TicketCompliance` schema). See
//! `reports/ask-ticket-compliance-experiment/REPORT.md` for the full design
//! rationale.

pub mod fetch;
pub mod parser;
pub mod prompt;
pub mod render;

pub use crate::models::ComplianceConfig;

use serde::{Deserialize, Serialize};

use crate::models::{LLMConfig, MRInfo};

/// One ticket reference extracted from the MR/PR description.
///
/// `project` is the GitLab project path or GitHub `owner/repo` for cross-
/// project references; `None` for the local `#N` / `!N` shorthand that
/// resolves to the MR's own project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRef {
    /// Which provider the ticket lives on.
    pub provider: TicketProvider,
    /// Project path / owner-repo. `None` only for the local-shorthand refs
    /// (`!N`, `#N`) which inherit the MR's own project.
    pub project: Option<String>,
    /// Ticket internal id (iid for GitLab, number for GitHub).
    pub iid: u32,
    /// Canonical URL identifying this ticket, after host gating.
    pub url: String,
    /// Original surface form: `!42`, `#42`, `org/repo#42`, the work-item URL,
    /// or the GitHub issue URL. Used verbatim in error messages and the
    /// rendered section so a reader can match the parser's hit against the
    /// MR description.
    pub source: String,
}

/// Git provider for a [`TicketRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TicketProvider {
    GitLab,
    GitHub,
}

/// A fetched ticket body — either successfully fetched or carrying a
/// failure reason. Failures are surfaced as a section note, not as a
/// review-blocking error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedTicket {
    pub ticket_ref: TicketRef,
    pub title: String,
    pub body: String,
    /// Provider-returned canonical URL, when the fetch succeeded.
    pub web_url: Option<String>,
    /// Failure reason when the fetch did not succeed; `None` on success.
    pub error: Option<String>,
}

impl FetchedTicket {
    pub fn is_fetched(&self) -> bool {
        self.error.is_none()
    }

    /// Truncate the body to the budget the prompt template enforces, so a
    /// ticket that returns a multi-megabyte description cannot blow up the
    /// LLM context. Mirrors pr-agent's `MAX_TICKET_CHARACTERS = 10000`.
    pub fn body_clipped(&self, max_chars: usize) -> String {
        truncate_chars(&self.body, max_chars)
    }
}

/// One ticket's LLM-produced compliance check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketCheck {
    pub ticket_ref: TicketRef,
    pub ticket_title: String,
    /// Bullet list of requirements the LLM read from the ticket body.
    pub requirement: String,
    /// Bullet list of requirements the LLM judged satisfied by the diff.
    pub satisfied: String,
    /// Bullet list of requirements the LLM judged NOT satisfied.
    pub not_satisfied: String,
    /// Bullet list of items needing human verification (out-of-diff evidence,
    /// spec interpretation, etc.).
    pub need_verification: String,
    /// Set when the LLM response could not be parsed into the four fields.
    /// `raw_response` carries the verbatim body so the section can surface
    /// "parse failure" the same way the per-expert sections do.
    pub parse_error: Option<String>,
    pub raw_response: String,
}

/// The full compliance output for one review run.
///
/// `None` when the gate is off, when no ticket refs were found, or when
/// every fetch failed AND the LLM call did not happen. Otherwise carries
/// one [`TicketCheck`] per successfully-fetched ticket plus notes for any
/// fetch failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceOutput {
    pub checks: Vec<TicketCheck>,
    /// Ticket references we could not fetch, kept here so the section can
    /// surface "X referenced, fetch failed: HTTP 404" instead of silently
    /// dropping them.
    pub fetch_failures: Vec<FetchedTicket>,
    /// Free-form notes about the run — e.g. "the LLM call failed: …", or
    /// "description references 4 tickets, capped at 3".
    pub notes: Vec<String>,
    /// RENG-38 / RENG-75: provider + model snapshot from the call that
    /// actually produced the compliance section. `None` when no LLM call
    /// happened (gate off, no refs, or every fetch failed).
    pub llm_provider: Option<String>,
    pub llm_model: Option<String>,
    /// RENG-75: entry fingerprint of the serving card. IN-MEMORY ONLY —
    /// hashes the API key and must never appear in any persisted or
    /// serialized form.
    #[serde(default, skip_serializing)]
    pub llm_fp: Option<String>,
}

impl ComplianceOutput {
    pub fn empty() -> Self {
        Self {
            checks: Vec::new(),
            fetch_failures: Vec::new(),
            notes: Vec::new(),
            llm_provider: None,
            llm_model: None,
            llm_fp: None,
        }
    }
}

/// The gate decision: should this review run the compliance check?
///
/// Three inputs: the user-level gate ([`ComplianceConfig::enabled`]), the
/// presence of ticket references in the MR description, and (silently) the
/// availability of a usable LLM. A gate-on + no-tickets run is a no-op —
/// the section is simply absent — exactly the `0 extra calls` guarantee.
pub fn gate(config: &ComplianceConfig, ticket_refs: &[TicketRef]) -> bool {
    config.enabled && !ticket_refs.is_empty()
}

/// Truncate `body` to at most `max_chars` Unicode characters, appending an
/// ellipsis marker. Mirrors pr-agent's `MAX_TICKET_CHARACTERS` cap (10_000)
/// to keep the compliance prompt bounded.
fn truncate_chars(body: &str, max_chars: usize) -> String {
    let count = body.chars().count();
    if count <= max_chars {
        return body.to_string();
    }
    let mut out: String = body.chars().take(max_chars).collect();
    out.push_str("\n\n[…truncated]");
    out
}

/// Decide whether the compliance sub-step should run, and if so, build the
/// orchestrator inputs (the ticket-refs list, the fetch plan and the
/// prompt inputs).
///
/// This is the only entry point the [`crate::lib::run_review`] integration
/// calls — keeping the rest of the wiring in test-only paths and isolating
/// the gating arithmetic in one place makes the four-gate contract (off,
/// on+no-refs, on+refs, fetch-fail) auditable from a single test surface.
pub struct CompliancePlan {
    pub ticket_refs: Vec<TicketRef>,
    /// Whether the gate decided this run should call the LLM.
    pub should_run: bool,
    /// Notes gathered before the LLM call (e.g. "capped at 3 tickets").
    pub pre_notes: Vec<String>,
}

/// Inspect the MR description, apply the cap, and decide whether to run.
///
/// `provider_host` is the host the MR was opened on (so cross-host refs are
/// dropped — pr-agent's `extract_gitlab_ticket_references` host gate).
pub fn plan(
    config: &ComplianceConfig,
    description: &str,
    provider: TicketProvider,
    provider_host: &str,
    mr_project: &str,
) -> CompliancePlan {
    if !config.enabled {
        return CompliancePlan {
            ticket_refs: Vec::new(),
            should_run: false,
            pre_notes: Vec::new(),
        };
    }
    let refs = parser::extract_ticket_refs(description, provider, mr_project, provider_host);
    let refs = dedupe_refs(refs);
    let mut pre_notes: Vec<String> = Vec::new();
    let refs = if refs.len() > crate::models::MAX_TICKETS_PER_REVIEW {
        let dropped = refs.len() - crate::models::MAX_TICKETS_PER_REVIEW;
        pre_notes.push(format!(
            "description references {} tickets, capped at {} ({} omitted)",
            refs.len(),
            crate::models::MAX_TICKETS_PER_REVIEW,
            dropped
        ));
        refs.into_iter().take(crate::models::MAX_TICKETS_PER_REVIEW).collect()
    } else {
        refs
    };
    let should_run = !refs.is_empty();
    CompliancePlan {
        ticket_refs: refs,
        should_run,
        pre_notes,
    }
}

fn dedupe_refs(refs: Vec<TicketRef>) -> Vec<TicketRef> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for r in refs {
        let key = (r.provider, r.project.clone(), r.iid, r.url.clone());
        if seen.insert(key) {
            out.push(r);
        }
    }
    out
}

/// Information for deciding whether a usable LLM is configured.
///
/// Centralised here so the run_review wiring does not need to know about
/// [`crate::llm`] internals — it just checks for an entry with a non-empty
/// `api_base` (a local provider needs no key).
pub fn has_usable_llm(configs: &[LLMConfig]) -> bool {
    configs.iter().any(|c| !c.api_base.trim().is_empty())
}

/// Decide the LLM config the compliance call should use.
///
/// Mirrors the orchestrator's "head of the chain" rule: the first config
/// with a non-empty `api_base`. When none is usable the caller must treat
/// the run as "gate on but no LLM" and skip the call.
pub fn select_llm_config(configs: &[LLMConfig]) -> Option<&LLMConfig> {
    configs.iter().find(|c| !c.api_base.trim().is_empty())
}

/// Run the M40 / RENG ticket compliance check end to end, returning a
/// [`ComplianceOutput`] the caller can attach to the [`crate::models::ReviewOutput`].
///
/// Returns `None` for the no-op paths so the caller can use
/// [`Option::is_none`] to skip `with_compliance`:
/// * `compliance.enabled = false` (no section, no calls).
/// * the MR description has no recognised ticket reference (no section, no
///   LLM call).
/// * no usable LLM is configured (the gate is on but we cannot fulfil it;
///   surface a section note so the operator sees why).
///
/// All other paths return `Some(ComplianceOutput)`. The orchestrator is
/// fail-soft: a provider-API error becomes a fetch-failure entry, an LLM
/// error becomes a section note, and a JSON parse failure marks each
/// affected ticket with `parse_error` — none of these abort the review.
pub async fn run_for_review(
    config: &ComplianceConfig,
    mr_info: &MRInfo,
    mr_url: &str,
    diff: &str,
    llm_configs: &[LLMConfig],
    _token: &str,
) -> Option<ComplianceOutput> {
    let mut output = ComplianceOutput::empty();
    let plan = plan_from_mr(config, mr_info, mr_url);
    // The gate: `enabled = false` is a no-op, as is "enabled but no refs".
    if !plan.should_run {
        // Even when the gate is off, surface the pre-notes (currently
        // empty unless the cap fired) for consistency.
        output.notes.extend(plan.pre_notes);
        if !output.notes.is_empty() || !plan.ticket_refs.is_empty() {
            // Carry the cap-note into the report only when at least one
            // pre-note was produced (the cap firing case). Otherwise the
            // gate-off no-op keeps the section absent (returns None).
            return Some(output);
        }
        return None;
    }
    output.notes.extend(plan.pre_notes);

    // Resolve the provider client. We construct a fresh client from the
    // MR URL because the orchestrator owns its own provider clients; the
    // token is the same token `run_review` was given.
    //
    // GitLab: the existing GitLabClient is parameterised by MR URL, but
    // we want a free-standing client (no MR binding) so we can call
    // fetch_issue with arbitrary project paths.
    //
    // GitHub: similar — the GitHubClient is parameterised by PR URL.
    //
    // We forward the `token` to fetch.rs and reuse its HTTP plumbing to
    // avoid a duplicate provider client per call.

    // Fetch each ticket via the compliance::fetch module.
    let mut fetched: Vec<FetchedTicket> = Vec::new();
    for r in &plan.ticket_refs {
        let ticket = fetch::fetch_ticket_via_provider(mr_url, r).await;
        if ticket.error.is_some() {
            output.fetch_failures.push(ticket);
        } else {
            fetched.push(ticket);
        }
    }
    if fetched.is_empty() {
        // Every fetch failed AND no LLM call is useful — keep the section
        // so the operator sees the fetch failures.
        return Some(output);
    }

    // Gate the LLM call on having a usable LLM.
    let Some(llm_cfg) = select_llm_config(llm_configs) else {
        output.notes.push(
            "compliance gate is on but no usable LLM is configured; the \
             ## Ticket compliance check section lists the fetched tickets \
             only."
                .to_string(),
        );
        // We still emit the section with the fetched-ticket list, so the
        // operator can see that the gate fired; push the tickets into
        // fetch_failures so the renderer surfaces them.
        for f in fetched {
            output.fetch_failures.push(f);
        }
        return Some(output);
    };

    // Build the prompt + run the single LLM call.
    let (system, user) = match prompt::build_compliance_prompt(mr_info, diff, &fetched) {
        Ok(p) => p,
        Err(e) => {
            output.notes.push(format!("compliance prompt build failed: {e}"));
            for f in fetched {
                output.fetch_failures.push(f);
            }
            return Some(output);
        }
    };

    let llm_client = crate::llm::client::LLMClient::new();
    let result = llm_client.complete(llm_cfg, &system, &user).await;
    match result {
        Ok(completion) => {
            let mut checks = prompt::parse_compliance_response(&completion.content, &plan.ticket_refs);
            // Attach the title from the fetched body so the section can
            // surface it (the parser does not see the title).
            for (check, f) in checks.iter_mut().zip(fetched.iter()) {
                check.ticket_title = f.title.clone();
            }
            output.checks = checks;
            output.llm_provider = Some(completion.provider.clone());
            output.llm_model = Some(completion.model.clone());
            // RENG-75: record the entry fp in-memory only.
            if let Ok(fp) = std::panic::catch_unwind(|| completion.entry_fp.clone()) {
                output.llm_fp = fp;
            }
            Some(output)
        }
        Err(e) => {
            output.notes.push(format!("compliance LLM call failed: {e}"));
            for f in fetched {
                output.fetch_failures.push(f);
            }
            Some(output)
        }
    }
}

/// Convenience: build the [`TicketProvider`] from an MR URL.
///
/// `mr_url` is the URL the user passed to `review`. `gitlab` and `github`
/// token env names are the CLI's existing routing signal — same heuristic
/// the CLI uses to pick `GitHubProvider` vs `GitLabProvider`.
pub fn provider_from_mr_url(mr_url: &str) -> TicketProvider {
    if mr_url.contains("github.com") || mr_url.contains(".github.") {
        TicketProvider::GitHub
    } else {
        TicketProvider::GitLab
    }
}

/// Resolve the host of an MR URL.
///
/// Returns `""` (the empty string) when the URL cannot be parsed — the
/// parser treats that as "no host gate", matching pr-agent's behaviour on
/// unparseable URLs.
pub fn host_from_mr_url(mr_url: &str) -> String {
    let stripped = mr_url
        .strip_prefix("https://")
        .or_else(|| mr_url.strip_prefix("http://"))
        .unwrap_or(mr_url);
    stripped
        .split('/')
        .next()
        .unwrap_or("")
        .split('@')
        .next_back()
        .unwrap_or("")
        .to_string()
}

/// Convenience used by the lib.rs wiring: build a [`CompliancePlan`] from
/// the runtime pieces the run_review call already has in hand.
pub fn plan_from_mr(config: &ComplianceConfig, mr_info: &MRInfo, mr_url: &str) -> CompliancePlan {
    let provider = provider_from_mr_url(mr_url);
    let host = host_from_mr_url(mr_url);
    plan(config, &mr_info.description, provider, &host, &mr_info.project_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_local_github(iid: u32) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitHub,
            project: None,
            iid,
            url: format!("https://github.com/owner/repo/issues/{iid}"),
            source: format!("#{}", iid),
        }
    }

    #[test]
    fn gate_is_false_when_disabled() {
        let cfg = ComplianceConfig { enabled: false };
        assert!(!gate(&cfg, &[ref_local_github(1)]));
    }

    #[test]
    fn gate_is_false_when_enabled_but_no_refs() {
        let cfg = ComplianceConfig { enabled: true };
        assert!(!gate(&cfg, &[]));
    }

    #[test]
    fn gate_is_true_when_enabled_and_refs_present() {
        let cfg = ComplianceConfig { enabled: true };
        assert!(gate(&cfg, &[ref_local_github(1)]));
    }

    #[test]
    fn truncate_chars_handles_short_bodies() {
        assert_eq!(truncate_chars("hi", 10), "hi");
    }

    #[test]
    fn truncate_chars_appends_marker_for_long_bodies() {
        let long: String = "x".repeat(20);
        let out = truncate_chars(&long, 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("truncated"));
        assert_eq!(out.chars().filter(|c| *c == 'x').count(), 10);
    }

    #[test]
    fn fetched_ticket_body_clipped_applies_the_budget() {
        let mut r = FetchedTicket {
            ticket_ref: ref_local_github(1),
            title: "t".into(),
            body: "y".repeat(20_000),
            web_url: None,
            error: None,
        };
        assert_eq!(r.body_clipped(100).chars().filter(|c| *c == 'y').count(), 100);
        r.body = "tiny".into();
        assert_eq!(r.body_clipped(100), "tiny");
    }

    #[test]
    fn fetched_ticket_is_fetched_only_on_success() {
        let mut r = FetchedTicket {
            ticket_ref: ref_local_github(1),
            title: "t".into(),
            body: String::new(),
            web_url: None,
            error: None,
        };
        assert!(r.is_fetched());
        r.error = Some("404".into());
        assert!(!r.is_fetched());
    }

    #[test]
    fn provider_from_mr_url_picks_github_for_github_urls() {
        assert_eq!(
            provider_from_mr_url("https://github.com/owner/repo/pull/1"),
            TicketProvider::GitHub
        );
        assert_eq!(
            provider_from_mr_url("https://x.github.example/o/r/pull/1"),
            TicketProvider::GitHub
        );
    }

    #[test]
    fn provider_from_mr_url_picks_gitlab_for_other_hosts() {
        assert_eq!(
            provider_from_mr_url("https://gitlab.example.com/g/p/-/merge_requests/1"),
            TicketProvider::GitLab
        );
    }

    #[test]
    fn host_from_mr_url_strips_scheme_and_path() {
        assert_eq!(
            host_from_mr_url("https://gitlab.example.com:8443/g/p/-/merge_requests/1"),
            "gitlab.example.com:8443"
        );
        assert_eq!(host_from_mr_url("https://github.com/owner/repo/pull/1"), "github.com");
    }

    #[test]
    fn host_from_mr_url_handles_no_scheme() {
        // No scheme: the whole first segment becomes the "host".
        assert_eq!(host_from_mr_url("gitlab.example.com/g/p"), "gitlab.example.com");
    }

    #[test]
    fn select_llm_config_picks_first_usable() {
        let configs = vec![
            LLMConfig {
                provider: "x".into(),
                model: "m".into(),
                api_key: String::new(),
                api_base: String::new(),
                max_tokens: 0,
                temperature: 0.0,
                disable_thinking: None,
                disabled: false,
            },
            LLMConfig {
                provider: "x".into(),
                model: "m".into(),
                api_key: "k".into(),
                api_base: "https://api".into(),
                max_tokens: 0,
                temperature: 0.0,
                disable_thinking: None,
                disabled: false,
            },
        ];
        assert_eq!(select_llm_config(&configs).unwrap().api_base, "https://api");
    }

    #[test]
    fn select_llm_config_returns_none_for_empty() {
        assert!(select_llm_config(&[]).is_none());
    }

    #[test]
    fn has_usable_llm_requires_non_empty_api_base() {
        let mut cfg = LLMConfig {
            provider: "x".into(),
            model: "m".into(),
            api_key: String::new(),
            api_base: String::new(),
            max_tokens: 0,
            temperature: 0.0,
            disable_thinking: None,
            disabled: false,
        };
        assert!(!has_usable_llm(&[cfg.clone()]));
        cfg.api_base = "https://api".into();
        assert!(has_usable_llm(&[cfg]));
    }

    #[test]
    fn plan_returns_no_refs_when_disabled() {
        let cfg = ComplianceConfig { enabled: false };
        let plan = plan(&cfg, "Closes #42", TicketProvider::GitHub, "github.com", "owner/repo");
        assert!(!plan.should_run);
        assert!(plan.ticket_refs.is_empty());
    }

    #[test]
    fn plan_caps_refs_at_max() {
        let cfg = ComplianceConfig { enabled: true };
        let description = "Closes #1 #2 #3 #4 #5";
        let plan = plan(&cfg, description, TicketProvider::GitHub, "github.com", "owner/repo");
        assert_eq!(plan.ticket_refs.len(), crate::models::MAX_TICKETS_PER_REVIEW);
        assert!(plan.should_run);
        assert!(plan.pre_notes.iter().any(|n| n.contains("capped")));
    }
}
