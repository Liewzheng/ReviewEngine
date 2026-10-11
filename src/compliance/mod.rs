//! M40 / RENG ticket compliance check (M39 §7 Option A).
//!
//! When [`crate::models::ComplianceConfig::enabled`] is true and an MR/PR
//! description references a recognised ticket (`Closes #N`, `org/repo#N`,
//! `<work_item_URL>`), the review fetches each referenced ticket and runs
//! a single LLM call that compares the diff to the ticket's requirements.
//! The result is rendered as a `## Ticket compliance check` section
//! alongside the per-expert findings.
//!
//! GitLab's `!N` MR sigil is intentionally NOT matched here: it refers
//! to a merge request, not an issue, and fetching "issue #N" via the
//! issues endpoint for an MR reference yields an unrelated object (or a
//! 404). Operators who want MR↔MR traceability should look at the MR
//! description instead. This matches pr-agent's
//! `GITLAB_TICKET_PATTERN` choice.
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
/// project references; `None` for the local `#N` shorthand that
/// resolves to the MR's own project. GitLab `!N` is the MR sigil and
/// is intentionally NOT parsed by this module (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketRef {
    /// Which provider the ticket lives on.
    pub provider: TicketProvider,
    /// Project path / owner-repo. `None` only for the local `#N`
    /// shorthand that inherits the MR's own project. (The GitLab `!N`
    /// MR sigil is intentionally NOT parsed — see the module docs.)
    pub project: Option<String>,
    /// Ticket internal id (iid for GitLab, number for GitHub).
    pub iid: u32,
    /// Canonical URL identifying this ticket, after host gating.
    pub url: String,
    /// Original surface form: `#42`, `org/repo#42`, the work-item URL,
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
    /// Ticket references we could not fetch (HTTP error, transport,
    /// missing project, etc.). The renderer labels these "fetch failed"
    /// so the reader does not mistake them for the no-LLM case (P2-2).
    /// `#[serde(default)]` so an r1-shaped stored JSON (without this
    /// field) deserializes cleanly — forward-compat insurance for any
    /// never-merged pre-M40-r2 records.
    #[serde(default)]
    pub fetch_failures: Vec<FetchedTicket>,
    /// Tickets fetched successfully but never analysed — either no usable
    /// LLM was configured or the LLM call itself errored. The renderer
    /// surfaces them as "fetched, no analysis" so the reader can tell
    /// them apart from genuine fetch failures (P2-2). Same forward-
    /// compat note as `fetch_failures`.
    #[serde(default)]
    pub unanalysed_tickets: Vec<FetchedTicket>,
    /// Free-form notes about the run — e.g. "the LLM call failed: …", or
    /// "description references 4 tickets, capped at 3".
    #[serde(default)]
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
            unanalysed_tickets: Vec::new(),
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

/// Decide the LLM config the compliance call should use.
///
/// Mirrors the orchestrator's "head of the chain" rule: the first config
/// with a non-empty `api_base`. When none is usable the caller must treat
/// the run as "gate on but no LLM" and skip the call.
fn select_llm_config(configs: &[LLMConfig]) -> Option<&LLMConfig> {
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
///
/// `token` is the platform PAT the caller already holds for the MR/PR
/// fetch — the function does NOT read from `GITLAB_TOKEN` /
/// `GITHUB_TOKEN` env vars (P2-1).
pub async fn run_for_review(
    config: &ComplianceConfig,
    mr_info: &MRInfo,
    mr_url: &str,
    diff: &str,
    llm_configs: &[LLMConfig],
    token: &str,
) -> Option<ComplianceOutput> {
    let mut output = ComplianceOutput::empty();
    let plan = plan_from_mr(config, mr_info, mr_url);
    // The gate: `enabled = false` is a no-op, as is "enabled but no refs".
    if !plan.should_run {
        output.notes.extend(plan.pre_notes);
        if !output.notes.is_empty() {
            return Some(output);
        }
        return None;
    }
    output.notes.extend(plan.pre_notes);

    // Fetch each ticket via the provider client.
    let mut fetched: Vec<FetchedTicket> = Vec::new();
    for r in &plan.ticket_refs {
        let ticket = fetch::fetch_ticket_via_provider(token, mr_url, r).await;
        if ticket.error.is_some() {
            output.fetch_failures.push(ticket);
        } else {
            fetched.push(ticket);
        }
    }
    if fetched.is_empty() {
        // Every fetch failed — keep the section so the operator sees the
        // fetch failures. No LLM call is useful when nothing fetched.
        return Some(output);
    }

    // Gate the LLM call on having a usable LLM.
    let Some(llm_cfg) = select_llm_config(llm_configs) else {
        // P2-2: the tickets WERE fetched, so render them as fetched
        // (not as fetch failures). The renderer distinguishes the two
        // statuses, and the reader sees the real picture: the section
        // is gated on the LLM, the fetch itself succeeded.
        output
            .notes
            .push("compliance gate is on but no usable LLM is configured; the ## Ticket compliance check section lists the fetched tickets only.".to_string());
        output.unanalysed_tickets = std::mem::take(&mut fetched);
        return Some(output);
    };

    // Build the prompt + run the single LLM call.
    let (system, user) = match prompt::build_compliance_prompt(mr_info, diff, &fetched) {
        Ok(p) => p,
        Err(e) => {
            output.notes.push(format!("compliance prompt build failed: {e}"));
            output.unanalysed_tickets = std::mem::take(&mut fetched);
            return Some(output);
        }
    };

    let llm_client = crate::llm::client::LLMClient::new();
    let result = llm_client.complete(llm_cfg, &system, &user).await;
    match result {
        Ok(completion) => {
            let mut checks = prompt::parse_compliance_response(&completion.content, &plan.ticket_refs);
            let valid_checks = checks.iter().filter(|c| c.parse_error.is_none()).count();
            // M42-R2 (RENG-46 §3): a `finish_reason: "length"` truncation
            // leaves the JSON cut mid-string and the per-ticket parser
            // emits parse_error for every entry. The model simply ran out
            // of `max_tokens` on the batch prompt — re-sending the same
            // call with a larger budget would burn the same budget on the
            // first ticket's prose; instead we fall back to one call per
            // ticket so each gets its own fresh budget. The cost is bounded
            // by `MAX_TICKETS_PER_REVIEW` (already paying 1, worst case N).
            // OpenAI's `stop` / `tool_calls` / `content_filter` and
            // Anthropic's `end_turn` / `stop_sequence` are not length
            // truncations and do not trigger the fallback.
            let truncated = completion.finish_reason.as_deref() == Some("length")
                || completion.finish_reason.as_deref() == Some("max_tokens");
            if truncated && (valid_checks == 0 || valid_checks < fetched.len()) {
                output.notes.push(format!(
                    "compliance batch LLM call truncated at finish_reason={:?}; falling back to per-ticket calls",
                    completion.finish_reason
                ));
                return run_per_ticket_fallback(
                    &llm_client,
                    llm_cfg,
                    mr_info,
                    diff,
                    &plan.ticket_refs,
                    &fetched,
                    output,
                )
                .await;
            }
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
            // P2-2: distinguish "LLM call failed" from "fetch failed":
            // the fetch succeeded, the LLM didn't — keep the tickets on a
            // separate field so the renderer can label them correctly.
            //
            // M42 / RENG-46: categorise the failure class so the rendered
            // Notes tell the operator whether it was a JSON-decode
            // (transport hiccup, body captured), an HTTP-status
            // (provider rejected the request), or an empty content
            // (the model answered nothing). All three currently show
            // up as the same opaque message.
            output
                .notes
                .push(format!("compliance LLM call failed: {}", categorise_llm_error(&e)));
            output.unanalysed_tickets = std::mem::take(&mut fetched);
            Some(output)
        }
    }
}

/// Run the compliance prompt one ticket at a time and merge the results.
///
/// Used when the batch call came back with `finish_reason: "length"` —
/// the model hit the output budget before emitting all the per-ticket
/// fields. One call per ticket gives each ticket its own full budget so
/// a 3-ticket prompt that overruns 4096 tokens becomes three independent
/// ~1.5K-token calls that each fit comfortably.
///
/// The merged `ComplianceOutput` carries one `TicketCheck` per fetched
/// ticket; per-ticket call failures become `parse_error` rows (the same
/// shape the parser uses for a single-call parse miss), and a summary
/// note records how many calls succeeded.
async fn run_per_ticket_fallback(
    llm_client: &crate::llm::client::LLMClient,
    llm_cfg: &LLMConfig,
    mr_info: &MRInfo,
    diff: &str,
    ticket_refs: &[TicketRef],
    fetched: &[FetchedTicket],
    mut output: ComplianceOutput,
) -> Option<ComplianceOutput> {
    let mut checks: Vec<TicketCheck> = Vec::with_capacity(fetched.len());
    let mut succeeded = 0usize;
    for (i, f) in fetched.iter().enumerate() {
        // Single-ticket slice — the Jinja template renders one block.
        let one = std::slice::from_ref(f);
        let (system, user) = match prompt::build_compliance_prompt(mr_info, diff, one) {
            Ok(p) => p,
            Err(e) => {
                output.notes.push(format!(
                    "compliance per-ticket prompt build failed for ticket #{}: {}",
                    f.ticket_ref.iid, e
                ));
                checks.push(make_parse_error_check(
                    &f.ticket_ref,
                    &f.title,
                    "",
                    "prompt build failed",
                ));
                continue;
            }
        };
        match llm_client.complete(llm_cfg, &system, &user).await {
            Ok(completion) => {
                let one_ref = std::slice::from_ref(ticket_refs.get(i).unwrap_or(&f.ticket_ref));
                let mut per_ticket = prompt::parse_compliance_response(&completion.content, one_ref);
                if let Some(check) = per_ticket.first_mut() {
                    check.ticket_title = f.title.clone();
                    if check.parse_error.is_none() {
                        succeeded += 1;
                    }
                }
                checks.extend(per_ticket);
            }
            Err(e) => {
                output.notes.push(format!(
                    "compliance per-ticket LLM call failed for ticket #{}: {}",
                    f.ticket_ref.iid,
                    categorise_llm_error(&e)
                ));
                checks.push(make_parse_error_check(
                    &f.ticket_ref,
                    &f.title,
                    &format!("per-ticket LLM call failed: {e}"),
                    "",
                ));
            }
        }
    }
    output.checks = checks;
    // Note: leave `unanalysed_tickets` empty — every ticket has a row,
    // either a parsed verdict or a parse-error placeholder. The caller
    // asked for per-ticket analysis, so the "fetched but not analysed"
    // semantic no longer applies.
    output.unanalysed_tickets.clear();
    output.notes.push(format!(
        "compliance per-ticket fallback: {}/{} tickets parsed",
        succeeded,
        fetched.len()
    ));
    if succeeded > 0 {
        // Stamp provider/model from the last successful per-ticket call.
        // (Each call attributes to the same config in the chain, so we
        // pick the most recent hit rather than the first — the last
        // one wins if the chain advanced mid-batch.)
        // For single-config chains (the common case) all calls hit the
        // same entry and the value is identical.
        output.llm_provider = Some(llm_cfg.provider.clone());
        output.llm_model = Some(llm_cfg.model.clone());
    }
    Some(output)
}

/// Build a [`TicketCheck`] that records a per-ticket fallback failure.
///
/// Used when the per-ticket sub-step failed to produce a verdict
/// (prompt build failed, LLM call errored, parser emitted an error
/// row). Mirrors the shape [`prompt::parse_compliance_response`]
/// uses for batch-call parse failures so the renderer labels the row
/// the same way regardless of which path produced it.
fn make_parse_error_check(ticket_ref: &TicketRef, title: &str, parse_error: &str, raw_response: &str) -> TicketCheck {
    TicketCheck {
        ticket_ref: ticket_ref.clone(),
        ticket_title: title.to_string(),
        requirement: String::new(),
        satisfied: String::new(),
        not_satisfied: String::new(),
        need_verification: String::new(),
        parse_error: if parse_error.is_empty() {
            None
        } else {
            Some(parse_error.to_string())
        },
        raw_response: raw_response.to_string(),
    }
}

/// Render a one-line summary of an LLM call error so the operator can
/// triage without grepping through chain entries. Order matters: the
/// typed markers (M42's [`crate::llm::client::JsonDecodeError`] /
/// [`crate::llm::client::EmptyCompletion`]) are tried first so the
/// summary names the failure class even when the underlying message
/// looks identical.
fn categorise_llm_error(err: &anyhow::Error) -> String {
    // Walk the chain so a `.context(...)` wrapper does not hide the
    // typed marker the inner step attached.
    for cause in err.chain() {
        if cause.downcast_ref::<crate::llm::client::JsonDecodeError>().is_some() {
            return format!("JSON decode: {err}");
        }
        if cause.downcast_ref::<crate::llm::client::EmptyCompletion>().is_some() {
            return format!("empty completion: {err}");
        }
    }
    // Fall back to the chain message — keep the inner text so an HTTP
    // status like `LLM API returned 401 Unauthorized: …` is visible.
    format!("{err:#}")
}

/// Convenience: build the [`TicketProvider`] from an MR URL.
///
/// `mr_url` is the URL the user passed to `review`. Detection follows
/// pr-agent's GitHub/GitLab routing, which the CLI itself mirrors
/// (`src/lib.rs:94`):
/// 1. `…/pull/<number>` is the GitHub PR-shape → GitHub.
/// 2. `…/-/merge_requests/<number>` is the GitLab MR-shape → GitLab.
/// 3. Otherwise: hostname heuristic (`github.com` / `.github.`).
///
/// The path-shape check comes first because self-hosted GitHub
/// Enterprise instances (`https://github.example.com/...`) and wiremock
/// fixtures (`http://127.0.0.1:port/...`) both fail the hostname check
/// but the path shape disambiguates them.
pub fn provider_from_mr_url(mr_url: &str) -> TicketProvider {
    if mr_url.contains("/pull/") {
        return TicketProvider::GitHub;
    }
    if mr_url.contains("/-/merge_requests/") {
        return TicketProvider::GitLab;
    }
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
        // `.github.` hostname: still GitHub.
        assert_eq!(
            provider_from_mr_url("https://x.github.example/o/r/pull/1"),
            TicketProvider::GitHub
        );
        // `/pull/` path with a self-hosted GitHub Enterprise host (no
        // `github.com` substring): path shape wins.
        assert_eq!(
            provider_from_mr_url("https://github.example.com/o/r/pull/1"),
            TicketProvider::GitHub
        );
    }

    #[test]
    fn provider_from_mr_url_picks_gitlab_for_other_hosts() {
        assert_eq!(
            provider_from_mr_url("https://gitlab.example.com/g/p/-/merge_requests/1"),
            TicketProvider::GitLab
        );
        // Plain host with no signal defaults to GitLab (the production
        // routing default at `src/lib.rs:94`).
        assert_eq!(provider_from_mr_url("https://example.com/o/r"), TicketProvider::GitLab);
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
