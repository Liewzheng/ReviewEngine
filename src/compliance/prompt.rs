//! LLM prompt for the ticket compliance check (M40 / RENG).
//!
//! Mirrors pr-agent's `ticket_pr_compliance_check.py` system prompt shape
//! (system: "You review PRs against linked tickets; output per-ticket:
//! ticket_requirements / satisfied / not-satisfied / needs-verification")
//! while keeping the field names the M39 report recommends:
//!
//! * `requirement` — bullet list of requirements read from the ticket body
//! * `satisfied` — bullet list of requirements satisfied by the diff
//! * `not_satisfied` — bullet list of requirements NOT satisfied
//! * `need_verification` — bullet list of items requiring human verification
//!
//! The schema is JSON: one entry per ticket, keyed by the canonical ticket
//! URL, so `parse_compliance_response` can match it back to the input.
//!
//! LLM real-world behaviour (M41 live smoke, mimo-v2.5-pro):
//! * The model picks the GitLab REST API URL (`/api/v4/projects/.../issues/N`)
//!   for the per-ticket key, even though the system prompt asks for the
//!   work_items URL. The prompt says nothing about which URL form to use,
//!   and the user-prompt template embeds the API URL (because that is what
//!   `gitlab_issue_url` produces for fetching). The parser used to try only
//!   `r.url` (the work_items URL) and `r.source` (`#73`); neither matched,
//!   so every ticket became `parse_error: ticket missing from response`.
//!   [`parse_compliance_response`] now tries every plausible URL shape a
//!   model might pick — the canonical work_items URL with and without a
//!   trailing slash, the GitLab REST API URL, the GitHub REST API URL,
//!   the bare iid as a string, and the surface form `#N` / `!N` — so a
//!   model that picks any of them still maps back to the right ticket.

use anyhow::Result;
use minijinja::Environment;
use serde::Serialize;

use super::{FetchedTicket, TicketProvider, TicketRef};
use crate::models::MRInfo;

/// The system prompt: direct the LLM to focus only on the ticket ↔ diff
/// relationship and to use the four fields verbatim.
///
/// M42 — the previous version asked for "one key per ticket URL" without
/// pinning the URL shape. `mimo-v2.5-pro` then picked the GitLab REST
/// API URL (`/api/v4/projects/.../issues/N`) instead of the work_items
/// URL we feed to the model, and the parser rejected every entry as
/// `ticket missing from response`. The prompt now pins the key to the
/// canonical URL and shows one explicit example using that exact
/// shape. The parser still tolerates the API-URL key shape and the
/// other variants (M42 `candidate_keys_for_ref`), so this is a defence
/// in depth rather than a hard requirement.
pub const COMPLIANCE_SYSTEM_TEMPLATE: &str = r#"You review code changes against their linked tickets. For each ticket provided, judge how the diff satisfies the ticket's requirements.

Output JSON ONLY, no prose outside the JSON, no markdown fences. The JSON MUST be an object with one key per ticket, and the key MUST be the exact `URL:` line shown for that ticket in the user prompt (the work_items / issues web URL — not the GitHub / GitLab REST API URL, not the bare ticket number, not a markdown link).

Example (do not copy the text, copy the key shape):

{
  "https://gitlab.example.com/group/project/-/work_items/73": {
    "requirement": "- one\n- three",
    "satisfied": "- one done",
    "not_satisfied": "- two missing",
    "need_verification": "- one spec"
  }
}

Each ticket's entry MUST have these four string fields, each a bullet list with one bullet per line:

- "requirement": bullet list of requirements you read from the ticket body
- "satisfied": bullet list of requirements the diff satisfies; cite file:line evidence
- "not_satisfied": bullet list of requirements the diff does NOT satisfy; cite file:line evidence
- "need_verification": bullet list of items needing human verification

Rules:
- Read the requirements from the TICKET BODY, not from the diff.
- Every bullet line MUST start with "- " followed by the text.
- If a field has no items, output a single "- none" line.
- Do NOT wrap the JSON in ``` or ```json fences — the parser strips them but a raw object is preferred.
- Cite file:line for satisfied / not_satisfied items when you can.
- need_verification is for items you cannot judge from the diff alone (spec interpretation, performance, deployment).
- Do not invent requirements the ticket does not state.
"#;

/// The user-prompt template: the MR metadata, the diff, and the tickets
/// (one block per ticket).
pub const COMPLIANCE_USER_TEMPLATE: &str = r#"## Merge / pull request
Title: {{ title }}
Branch: {{ source_branch }} → {{ target_branch }}
{{ mr_description_block }}

## Diff
```
{{ diff }}
```

## Linked tickets
{% for ticket in tickets %}
### {{ ticket.title }} ({{ ticket.source }})
URL: {{ ticket.url }}

{{ ticket.body }}

{% endfor %}
"#;

/// Build the (system, user) prompt pair for the compliance LLM call.
pub fn build_compliance_prompt(mr_info: &MRInfo, diff: &str, fetched: &[FetchedTicket]) -> Result<(String, String)> {
    let env = Environment::new();
    // We register the templates as strings (not files) so the function is
    // self-contained and has no on-disk dep.
    let mut env = env;
    env.add_template("compliance_user", COMPLIANCE_USER_TEMPLATE)?;

    // System prompt is the static template; no Jinja substitution needed.
    let system = COMPLIANCE_SYSTEM_TEMPLATE.to_string();

    // Render only fetched tickets; failures are surfaced in notes.
    let tickets_rendered: Vec<TicketForRender> = fetched
        .iter()
        .filter(|t| t.is_fetched())
        .map(|t| TicketForRender {
            title: if t.title.is_empty() {
                format!("Ticket #{}", t.ticket_ref.iid)
            } else {
                t.title.clone()
            },
            source: t.ticket_ref.source.clone(),
            url: t.ticket_ref.url.clone(),
            body: t.body.clone(),
        })
        .collect();
    let ctx = serde_json::json!({
        "title": mr_info.title,
        "source_branch": mr_info.source_branch,
        "target_branch": mr_info.target_branch,
        "mr_description_block": if mr_info.description.is_empty() {
            String::new()
        } else {
            format!("Description:\n{}", mr_info.description)
        },
        "diff": diff,
        "tickets": tickets_rendered,
    });
    let user = env.get_template("compliance_user")?.render(&ctx)?;
    Ok((system, user))
}

#[derive(Debug, Serialize)]
struct TicketForRender {
    title: String,
    source: String,
    url: String,
    body: String,
}

/// Parse the LLM JSON response into one [`super::TicketCheck`] per ticket.
///
/// Returns an empty `Vec` on parse failure (with the raw response preserved
/// on the first returned entry's `parse_error`) so the orchestrator can
/// surface the failure as a section note rather than blocking the review.
pub fn parse_compliance_response(raw: &str, refs: &[TicketRef]) -> Vec<super::TicketCheck> {
    let value: serde_json::Value = match extract_json_object(raw) {
        Ok(v) => v,
        Err(e) => {
            // One entry per ref, all marked as parse-failed, so the section
            // can render a per-ticket error line.
            return refs
                .iter()
                .map(|r| super::TicketCheck {
                    ticket_ref: r.clone(),
                    ticket_title: String::new(),
                    requirement: String::new(),
                    satisfied: String::new(),
                    not_satisfied: String::new(),
                    need_verification: String::new(),
                    parse_error: Some(format!("JSON extract: {e}")),
                    raw_response: raw.to_string(),
                })
                .collect();
        }
    };
    let Some(obj) = value.as_object() else {
        return refs
            .iter()
            .map(|r| super::TicketCheck {
                ticket_ref: r.clone(),
                ticket_title: String::new(),
                requirement: String::new(),
                satisfied: String::new(),
                not_satisfied: String::new(),
                need_verification: String::new(),
                parse_error: Some("response is not a JSON object".into()),
                raw_response: raw.to_string(),
            })
            .collect();
    };
    refs.iter()
        .map(|r| {
            // LLM real-world quirk (M41): mimo picks the GitLab REST API
            // URL (e.g. `/api/v4/projects/g%2Fp/issues/73`) for the per-
            // ticket key, even though the prompt asks for the work_items
            // URL. The system prompt does not pin the key shape, so we
            // accept every plausible form the model might use — the
            // canonical URL with and without a trailing slash, the API
            // URL, the GitHub REST URL (for cross-project GitHub refs),
            // the bare iid as a string, and the surface forms (`#N` /
            // `!N`). The helper returns the keys in priority order so a
            // model that picks any of them still maps back to the right
            // ticket (M42 finding: `parse_compliance_response` was the
            // single source of truth for the MR66 render failure).
            let entry = candidate_keys_for_ref(r)
                .into_iter()
                .find_map(|k| obj.get(&k).cloned())
                .unwrap_or(serde_json::Value::Null);
            let fields = match entry {
                serde_json::Value::Null => Err("ticket missing from response".to_string()),
                serde_json::Value::Object(map) => Ok((
                    map.get("requirement")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    map.get("satisfied").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    map.get("not_satisfied")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    map.get("need_verification")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )),
                _ => Err("entry is not an object".to_string()),
            };
            match fields {
                Ok((requirement, satisfied, not_satisfied, need_verification)) => super::TicketCheck {
                    ticket_ref: r.clone(),
                    ticket_title: String::new(),
                    requirement,
                    satisfied,
                    not_satisfied,
                    need_verification,
                    parse_error: None,
                    raw_response: raw.to_string(),
                },
                Err(e) => super::TicketCheck {
                    ticket_ref: r.clone(),
                    ticket_title: String::new(),
                    requirement: String::new(),
                    satisfied: String::new(),
                    not_satisfied: String::new(),
                    need_verification: String::new(),
                    parse_error: Some(e),
                    raw_response: raw.to_string(),
                },
            }
        })
        .collect()
}

/// Pull a top-level JSON object out of `text`, tolerating prose or
/// markdown fences the LLM may have added despite the system instruction.
///
/// M41 finding: the previous version tried bare `` ``` `` fences only.
/// `mimo-v2.5-pro` wraps the JSON in language-tagged fences (```` ```json
/// … ```` ), and when the model emits a truncated body the outer code
/// fence also appears in the captured string — leaving a stray
/// `` ```json `` line in front of the actual JSON that serde_json
/// rejects. We now strip every opening/closing fence line (with or
/// without a language tag) before the brace walk, then try the trimmed
/// body first and finally the brace-walk fallback.
fn extract_json_object(text: &str) -> Result<serde_json::Value, String> {
    let stripped = strip_markdown_fences(text);
    let trimmed = stripped.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if v.is_object() {
            return Ok(v);
        }
    }
    // Walk the text looking for the first `{` that starts an object whose
    // matching `}` produces a parseable object. Skips `{` inside strings
    // (the only safe-enough heuristic for the small responses the LLM
    // returns here).
    let bytes = trimmed.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'{' {
            continue;
        }
        // Walk forward, tracking string state and brace depth, until the
        // matching `}`.
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut escape = false;
        for (j, &c) in bytes.iter().enumerate().skip(i) {
            if escape {
                escape = false;
                continue;
            }
            match c {
                b'\\' if in_string => escape = true,
                b'"' => in_string = !in_string,
                b'{' if !in_string => depth += 1,
                b'}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        let candidate = &trimmed[i..=j];
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
                            if v.is_object() {
                                return Ok(v);
                            }
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    Err("no JSON object found in response".to_string())
}

/// Strip every line that is a bare markdown fence (`` ``` ``, possibly
/// followed by a language tag like `json` / `JSON` / `text`) so the
/// remaining body is parseable as JSON. The function preserves line
/// endings by replacing the fence line with an empty string; if the
/// resulting body is not JSON, [`extract_json_object`] still falls back
/// to the brace walk.
fn strip_markdown_fences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_end();
        let stripped = trimmed.trim_start();
        // An opening or closing fence: three or more backticks, optionally
        // followed by a language tag and a space. Bare `` ``` `` on its
        // own line is the closing fence, `` ```json `` / `` ```text ``
        // are opening fences. `` ```` ```` are also accepted because
        // some models / IDEs render the inner code block with a longer
        // backtick run than the outer.
        if is_fence_line(stripped) {
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// True when `line` is exactly a markdown code fence — three or more
/// backticks, optionally followed by a non-backtick info phrase (the
/// language tag).
fn is_fence_line(line: &str) -> bool {
    let mut count = 0u32;
    for c in line.chars() {
        if c == '`' {
            count += 1;
        } else {
            // The info string after the backticks may contain almost any
            // character, but a leading backtick is enough to mark this as
            // a fence. If we hit a non-backtick char first, the rest of
            // the line is the info string — anything after `` ``` `` (or
            // a longer run) is allowed.
            break;
        }
    }
    count >= 3
}

/// Build the ordered list of keys to try when matching the LLM JSON
/// object against one [`TicketRef`]. The first key that exists in the
/// object wins; everything after it is dropped.
///
/// Order matters: the canonical URL first so the prompt's intended key
/// shape (the work_items URL we feed to the model) maps cleanly, the API
/// URL second because that is what mimo picks in practice (M41), then
/// trailing-slash variants and the surface forms. The bare iid is a
/// last-resort fallback for a model that decided to key the JSON by
/// ticket number only.
fn candidate_keys_for_ref(r: &TicketRef) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let url_variants = url_key_variants(r);
    for url in &url_variants {
        if !keys.contains(url) {
            keys.push(url.clone());
        }
    }
    if !r.source.is_empty() && !keys.contains(&r.source) {
        keys.push(r.source.clone());
    }
    // The GitLab MR sigil `!N` is intentionally ignored by the parser
    // (see `parser.rs` module docs), but an LLM that has seen the MR
    // description will echo back the `!N` it observed — accepting it
    // here costs nothing and matches what the model actually wrote.
    match r.provider {
        TicketProvider::GitLab => {
            let bang = format!("!{}", r.iid);
            if !keys.contains(&bang) {
                keys.push(bang);
            }
        }
        TicketProvider::GitHub => {}
    }
    // Bare iid as a string — the last-resort fallback.
    let iid = r.iid.to_string();
    if !keys.contains(&iid) {
        keys.push(iid);
    }
    keys
}

/// Every URL-shaped key a single ref can match against. For a GitLab
/// `https://gitlab.example.com/g/p/-/work_items/42` ref, this returns
/// the work_items URL (with and without a trailing slash) plus the
/// REST API URL the GitLab client uses to fetch the ticket. For a
/// GitHub `…/issues/42` ref, it returns the web URL and the REST API
/// URL.
fn url_key_variants(r: &TicketRef) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !r.url.is_empty() {
        out.push(r.url.clone());
        if r.url.ends_with('/') {
            out.push(r.url.trim_end_matches('/').to_string());
        } else {
            out.push(format!("{}/", r.url));
        }
    }
    match r.provider {
        TicketProvider::GitLab => {
            if let Some(api) = gitlab_api_url_for_ref(r) {
                if !out.contains(&api) {
                    out.push(api);
                }
            }
        }
        TicketProvider::GitHub => {
            if let Some(api) = github_api_url_for_ref(r) {
                if !out.contains(&api) {
                    out.push(api);
                }
            }
        }
    }
    out
}

/// Derive the GitLab REST API URL (`/api/v4/projects/{enc}/issues/{iid}`)
/// from a [`TicketRef`].
///
/// `r.url` carries the web URL (`/-/work_items/N` or `/-/issues/N`).
/// The REST form differs only by `/api/v4/projects/` between the host
/// and the project segment. If we cannot derive the API URL because the
/// ref has no project or no host, return `None` — the caller will skip
/// that candidate.
fn gitlab_api_url_for_ref(r: &TicketRef) -> Option<String> {
    let url = r.url.as_str();
    let project = r.project.as_deref()?;
    // Strip the scheme.
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    // Split into host (first path segment) and the remainder.
    let mut split = after_scheme.splitn(2, '/');
    let host = split.next().unwrap_or("").to_string();
    let path = split.next().unwrap_or("");
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let encoded = encode_project_path(project);
    Some(format!("https://{}/api/v4/projects/{}/issues/{}", host, encoded, r.iid))
}

/// Derive the GitHub REST API URL (`/repos/{owner}/{repo}/issues/{iid}`)
/// from a [`TicketRef`]. Used when the LLM picks the REST form over the
/// web URL.
///
/// GitHub's REST API lives at `api.github.com` for the public host and
/// at the same host for GHE-Server (where the REST endpoint is under
/// `/api/v3`). The model the LLM saw in the user prompt is whatever the
/// `github_api_base_from_mr_url` helper produced, so we mirror that
/// rule: `github.com` (and `.github.com` suffixes) maps to
/// `api.github.com`, every other host keeps its host.
fn github_api_url_for_ref(r: &TicketRef) -> Option<String> {
    let url = r.url.as_str();
    let project = r.project.as_deref()?;
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = after_scheme.split('/').next().unwrap_or("").to_string();
    if host.is_empty() {
        return None;
    }
    let host_no_port = host.split(':').next().unwrap_or(&host).to_string();
    let api_host = if host_no_port.eq_ignore_ascii_case("github.com")
        || host_no_port.to_ascii_lowercase().ends_with(".github.com")
    {
        "api.github.com".to_string()
    } else {
        host
    };
    Some(format!("https://{}/repos/{}/issues/{}", api_host, project, r.iid))
}

/// URL-encode a `group/project` (GitLab) or `owner/repo` (GitHub)
/// project path the same way the fetch helpers do. Only `/` is allowed
/// to remain a literal slash; everything else gets percent-encoded.
fn encode_project_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            let mut out = String::with_capacity(segment.len());
            for b in segment.bytes() {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                        out.push(b as char);
                    }
                    _ => {
                        out.push_str(&format!("%{:02X}", b));
                    }
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("%2F")
}

#[cfg(test)]
mod tests {
    use super::super::TicketProvider;
    use super::*;

    fn ref_gitlab(iid: u32) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitLab,
            project: Some("g/p".into()),
            iid,
            url: format!("https://gitlab.example.com/g/p/-/work_items/{iid}"),
            source: format!("#{iid}"),
        }
    }

    fn fetched_with(iid: u32, body: &str) -> FetchedTicket {
        FetchedTicket {
            ticket_ref: ref_gitlab(iid),
            title: format!("Ticket {iid}"),
            body: body.into(),
            web_url: None,
            error: None,
        }
    }

    #[test]
    fn build_prompt_renders_each_ticket_block() {
        let mr = MRInfo::new("g/p".into(), "MR title".into(), "feat/x".into(), "main".into());
        let fetched = vec![
            fetched_with(73, "first requirement"),
            fetched_with(74, "second requirement"),
        ];
        let (system, user) = build_compliance_prompt(&mr, "diff --git a/x b/x", &fetched).unwrap();
        assert!(system.contains("review code changes against their linked tickets"));
        assert!(user.contains("MR title"));
        assert!(user.contains("diff --git a/x b/x"));
        assert!(user.contains("Ticket 73"));
        assert!(user.contains("first requirement"));
        assert!(user.contains("Ticket 74"));
        assert!(user.contains("second requirement"));
        // Skipped failures must NOT appear in the user prompt (they are
        // surfaced as notes instead).
        let mut failing = fetched_with(75, "skipped");
        failing.error = Some("HTTP 404".into());
        let fetched = vec![fetched_with(73, "ok"), failing];
        let (_, user) = build_compliance_prompt(&mr, "diff", &fetched).unwrap();
        assert!(user.contains("Ticket 73"));
        assert!(!user.contains("Ticket 75"));
        assert!(!user.contains("skipped"));
    }

    #[test]
    fn parse_response_extracts_per_ticket_fields() {
        let refs = vec![ref_gitlab(73), ref_gitlab(74)];
        let response = r#"{
            "https://gitlab.example.com/g/p/-/work_items/73": {
                "requirement": "- first",
                "satisfied": "- done",
                "not_satisfied": "- missing",
                "need_verification": "- spec"
            },
            "https://gitlab.example.com/g/p/-/work_items/74": {
                "requirement": "- second",
                "satisfied": "- none",
                "not_satisfied": "- gap",
                "need_verification": "- none"
            }
        }"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].ticket_ref.iid, 73);
        assert!(checks[0].requirement.contains("first"));
        assert!(checks[0].satisfied.contains("done"));
        assert!(checks[0].not_satisfied.contains("missing"));
        assert!(checks[0].need_verification.contains("spec"));
        assert_eq!(checks[1].ticket_ref.iid, 74);
        assert!(checks[1].satisfied.contains("none"));
        assert!(checks[1].not_satisfied.contains("gap"));
    }

    #[test]
    fn parse_response_tolerates_markdown_fences() {
        let refs = vec![ref_gitlab(73)];
        let response = r#"```json
{
  "https://gitlab.example.com/g/p/-/work_items/73": {
    "requirement": "- r",
    "satisfied": "- s",
    "not_satisfied": "- n",
    "need_verification": "- v"
  }
}
```"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].requirement.contains("r"));
    }

    #[test]
    fn parse_response_tolerates_prose_around_json() {
        let refs = vec![ref_gitlab(73)];
        let response = "Here is the JSON:\n{\"https://gitlab.example.com/g/p/-/work_items/73\":{\"requirement\":\"- r\",\"satisfied\":\"- s\",\"not_satisfied\":\"- n\",\"need_verification\":\"- v\"}}";
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].requirement.contains("r"));
    }

    #[test]
    fn parse_response_missing_ticket_gets_parse_error() {
        let refs = vec![ref_gitlab(73), ref_gitlab(74)];
        let response = r#"{
            "https://gitlab.example.com/g/p/-/work_items/73": {
                "requirement": "- r",
                "satisfied": "- s",
                "not_satisfied": "- n",
                "need_verification": "- v"
            }
        }"#;
        let checks = parse_compliance_response(response, &refs);
        assert!(checks[0].parse_error.is_none());
        assert!(checks[1].parse_error.is_some(), "missing ticket gets a parse error");
    }

    #[test]
    fn parse_response_handles_total_garbage() {
        let refs = vec![ref_gitlab(73)];
        let response = "I am a model and I refuse to comply.";
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_some());
        assert!(!checks[0].raw_response.is_empty());
    }

    #[test]
    fn parse_response_handles_non_object_response() {
        let refs = vec![ref_gitlab(73)];
        let response = "[1, 2, 3]";
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_some());
    }

    #[test]
    fn extract_json_object_handles_brace_in_string_value() {
        let v = extract_json_object(r#"{"k": "value with } brace"}"#).unwrap();
        assert_eq!(v["k"].as_str(), Some("value with } brace"));
    }

    /// M41 finding: mimo-v2.5-pro wraps the JSON in language-tagged fences
    /// (```` ```json ````) and uses the GitLab REST API URL as the
    /// per-ticket key. The parser must accept that key shape and strip
    /// the language-tagged fences before the brace walk. This test uses
    /// the truncated body shape captured in
    /// `reports/ticket-compliance-live-smoke/raw/mr66-closes-73.md`
    /// (the project segment / host desensitised to fixtures).
    #[test]
    fn parse_response_accepts_mimo_real_world_api_url_key_and_fences() {
        let refs = vec![ref_gitlab(73)];
        // Captured real-output shape (truncated body, original hosted in
        // raw/mr66-closes-73.md; scrubbed of the live host).
        let response = "```json\n{\n  \"https://gitlab.example.com/api/v4/projects/g%2Fp/issues/73\": {\n    \"requirement\": \"- requirement text\",\n    \"satisfied\": \"- satisfied text\",\n    \"not_satisfied\": \"- not_satisfied text\",\n    \"need_verification\": \"- need_verification text\"\n  }\n}\n```\n";
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1, "one ref → one check");
        let c = &checks[0];
        assert!(
            c.parse_error.is_none(),
            "API-URL key must be accepted, was: {:?}",
            c.parse_error
        );
        assert!(c.requirement.contains("requirement"));
        assert!(c.satisfied.contains("satisfied"));
        assert!(c.not_satisfied.contains("not_satisfied"));
        assert!(c.need_verification.contains("need_verification"));
    }

    /// Same M41 finding, with the JSON sandwiched between an outer code
    /// fence (the renderer's `` ```text `` block) and an inner fence the
    /// model added. The outer fence's closing `` ``` `` lands in front
    /// of the actual JSON body; both must be stripped before the brace
    /// walk.
    #[test]
    fn parse_response_strips_outer_and_inner_language_tagged_fences() {
        let refs = vec![ref_gitlab(73)];
        // Outer renderer fence opens with `` ```text ``, then the model
        // emits `` ```json `` … `` ``` `` inside, then the outer closes.
        let response = "```text\n```json\n{\n  \"https://gitlab.example.com/api/v4/projects/g%2Fp/issues/73\": {\n    \"requirement\": \"- r\",\n    \"satisfied\": \"- s\",\n    \"not_satisfied\": \"- n\",\n    \"need_verification\": \"- v\"\n  }\n}\n```\n```\n";
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
        assert!(checks[0].requirement.contains("r"));
    }

    /// M41 finding: the `!N` GitLab MR-sigil form is sometimes echoed
    /// back by the LLM (it appears in MR descriptions, after all).
    /// Accepting `!N` as a fallback costs nothing and matches what the
    /// model actually wrote when it picked the surface form.
    #[test]
    fn parse_response_accepts_gitlab_bang_iid_fallback() {
        let refs = vec![ref_gitlab(73)];
        let response = r#"{"!73": {"requirement": "- r", "satisfied": "- s", "not_satisfied": "- n", "need_verification": "- v"}}"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
        assert!(checks[0].requirement.contains("r"));
    }

    /// Last-resort fallback: a model that decides to key by bare iid as
    /// a string (some models strip the URL shape entirely).
    #[test]
    fn parse_response_accepts_bare_iid_string_fallback() {
        let refs = vec![ref_gitlab(73)];
        let response =
            r#"{"73": {"requirement": "- r", "satisfied": "- s", "not_satisfied": "- n", "need_verification": "- v"}}"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
    }

    /// Trailing-slash variant of the canonical URL.
    #[test]
    fn parse_response_accepts_trailing_slash_url_key() {
        let refs = vec![ref_gitlab(73)];
        let response = r#"{"https://gitlab.example.com/g/p/-/work_items/73/": {"requirement": "- r", "satisfied": "- s", "not_satisfied": "- n", "need_verification": "- v"}}"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
    }

    /// GitHub ref: the LLM picks the REST API URL
    /// (`/repos/owner/repo/issues/42`) instead of the web URL.
    #[test]
    fn parse_response_accepts_github_rest_api_url_key() {
        let ref_gh = TicketRef {
            provider: TicketProvider::GitHub,
            project: Some("owner/repo".into()),
            iid: 42,
            url: "https://github.com/owner/repo/issues/42".into(),
            source: "#42".into(),
        };
        let refs = vec![ref_gh];
        let response = r#"{"https://api.github.com/repos/owner/repo/issues/42": {"requirement": "- r", "satisfied": "- s", "not_satisfied": "- n", "need_verification": "- v"}}"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
    }

    /// When the model picks the REST URL but the host in `r.url` is a
    /// self-hosted GHE-Server (not `api.github.com`), the derived REST
    /// URL must still match.
    #[test]
    fn parse_response_accepts_github_ghe_server_rest_url_key() {
        let ref_gh = TicketRef {
            provider: TicketProvider::GitHub,
            project: Some("owner/repo".into()),
            iid: 42,
            url: "https://github.example.com/owner/repo/issues/42".into(),
            source: "#42".into(),
        };
        let refs = vec![ref_gh];
        let response = r#"{"https://github.example.com/repos/owner/repo/issues/42": {"requirement": "- r", "satisfied": "- s", "not_satisfied": "- n", "need_verification": "- v"}}"#;
        let checks = parse_compliance_response(response, &refs);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].parse_error.is_none(), "{:?}", checks[0].parse_error);
    }

    /// `extract_json_object` must accept a body where the language tag
    /// on the fence is uppercase (` ```JSON `) — the LLM-side quirk the
    /// M41 finding called out.
    #[test]
    fn extract_json_object_handles_uppercase_json_fence_tag() {
        let body = "```JSON\n{\"k\": \"v\"}\n```";
        let v = extract_json_object(body).unwrap();
        assert_eq!(v["k"].as_str(), Some("v"));
    }

    /// `extract_json_object` must accept a body that has a 4-backtick
    /// outer fence (the renderer's code block) and a 3-backtick inner
    /// fence (the model's wrapper). The brace walk is the backstop when
    /// the body is not directly parseable.
    #[test]
    fn extract_json_object_handles_mismatched_outer_and_inner_fences() {
        let body = "````\n```json\n{\"k\": \"v\"}\n```\n````";
        let v = extract_json_object(body).unwrap();
        assert_eq!(v["k"].as_str(), Some("v"));
    }

    /// Helper sanity-check: `candidate_keys_for_ref` produces the
    /// canonical URL first, then the API URL, then the surface forms.
    #[test]
    fn candidate_keys_order_canonical_then_api_then_surface() {
        let r = ref_gitlab(73);
        let keys = candidate_keys_for_ref(&r);
        assert_eq!(
            keys.first().map(|s| s.as_str()),
            Some("https://gitlab.example.com/g/p/-/work_items/73"),
            "canonical URL first"
        );
        assert!(
            keys.iter()
                .any(|k| k == "https://gitlab.example.com/api/v4/projects/g%2Fp/issues/73"),
            "API URL is one of the candidates: {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k == "#73"),
            "surface form `#73` is one of the candidates: {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k == "!73"),
            "GitLab `!N` is one of the candidates: {keys:?}"
        );
        assert!(
            keys.iter().any(|k| k == "73"),
            "bare iid is the last-resort candidate: {keys:?}"
        );
    }
}
