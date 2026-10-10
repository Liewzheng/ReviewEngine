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

use anyhow::Result;
use minijinja::Environment;
use serde::Serialize;

use super::{FetchedTicket, TicketProvider, TicketRef};
use crate::models::MRInfo;

/// The system prompt: direct the LLM to focus only on the ticket ↔ diff
/// relationship and to use the four fields verbatim.
pub const COMPLIANCE_SYSTEM_TEMPLATE: &str = r#"You review code changes against their linked tickets. For each ticket provided, judge how the diff satisfies the ticket's requirements.

Output JSON ONLY, no prose outside the JSON, no markdown fences. The JSON MUST be an object with one key per ticket URL:

{
  "<ticket_url>": {
    "requirement": "<bullet list of requirements you read from the ticket body, one per '- ' line; '-' empty bullets not allowed; use '-' for each requirement>",
    "satisfied": "<bullet list of requirements the diff satisfies, one per '- ' line; '-' empty bullets not allowed; use '-' for each satisfied item; cite file:line evidence>",
    "not_satisfied": "<bullet list of requirements the diff does NOT satisfy, one per '- ' line; '-' empty bullets not allowed; use '-' for each missing item; cite file:line evidence>",
    "need_verification": "<bullet list of items needing human verification, one per '- ' line; '-' empty bullets not allowed; use '-' for each item>"
  }
}

Rules:
- Read the requirements from the TICKET BODY, not from the diff.
- Every bullet line MUST start with "- " followed by the text.
- If a field has no items, output a single "- none" line.
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
            // Try canonical URL first, then the source form (some models
            // echo back the surface form).
            let entry = obj
                .get(&r.url)
                .or_else(|| obj.get(&r.source))
                .cloned()
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
fn extract_json_object(text: &str) -> Result<serde_json::Value, String> {
    let trimmed = text.trim();
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

#[allow(dead_code)]
pub(crate) const _PROVIDER_DOC: TicketProvider = TicketProvider::GitLab;

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_gitlab(iid: u32) -> TicketRef {
        TicketRef {
            provider: TicketProvider::GitLab,
            project: Some("g/p".into()),
            iid,
            url: format!("https://gitlab.example.com/g/p/-/work_items/{iid}"),
            source: format!("!{iid}"),
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
}
