//! Shared verdict-list parsing for the verifier and the adjudicator.
//!
//! M18-自审 #5: the verifier (`verifier.rs`) and the adjudicator
//! (`adjudicator.rs`) used to carry near-identical copies of the same
//! "clean the YAML → strict → fenced fallback → pull the `verdicts:`
//! list" routine. The only caller-side difference is the decision shape
//! (verifier maps to `{index, drop, reason}`; the adjudicator maps to
//! `{index, verdict, new_severity, reason, cited_lines}`). The error
//! message used to differ too — the verifier said "verdict YAML parse
//! failed", the adjudicator said "adjudication YAML parse failed" — but
//! after the dedup both pass through this module and share the verifier's
//! wording. No caller-side render pin depends on the old adjudicator
//! string (`test_parse_verdicts_malformed_errors` asserts only `is_err()`),
//! so the wording change is observable in the WARN text but not pinned by
//! a test; it is a logging nit, not a behaviour break.
//!
//! This module is the one place that decides "is there a verdict list
//! here at all, and what fields does each entry carry?" — callers
//! translate the shared [`RawVerdictEntry`] into their own decision type.

/// One entry extracted from the LLM's `verdicts: [...]` list.
///
/// Fields the caller does not care about are left as `None` or empty —
/// the verifier ignores `new_severity` / `cited_lines`, the adjudicator
/// ignores the absence of either. The point of this struct is to be the
/// single place that knows the verifier-and-adjudicator shared schema.
#[derive(Debug, Default, Clone)]
pub struct RawVerdictEntry {
    /// 0-based position the LLM gave this entry. The verifier and
    /// adjudicator both use it to look up the finding the LLM meant.
    pub index: usize,
    /// Verdict string verbatim (lower-cased by callers as they need). The
    /// shared parser does not interpret it — the verifier treats anything
    /// other than "drop" as keep, the adjudicator maps
    /// `false_positive` / `false-positive` / `drop` /
    /// `downgrade` and treats the rest as confirmed.
    pub verdict: String,
    /// Optional `new_severity` field (adjudicator-only). Parsed by the
    /// adjudicator into a [`crate::models::Severity`] via its own helper;
    /// the shared parser only carries the raw string.
    pub new_severity: Option<String>,
    /// Free-form reason text. Always a `String` so the caller can
    /// `clone()` without a separate `unwrap_or_default`.
    pub reason: String,
    /// Optional cited-line range (adjudicator-only). Stored verbatim.
    pub cited_lines: String,
}

/// Parse the LLM's verdict list from `text`.
///
/// The parser tolerates fenced code blocks and surrounding prose
/// (delegates to [`crate::output::parser::clean_yaml`] and
/// [`crate::output::parser::extract_first_fenced_yaml`]). Returns an
/// error when no `verdicts:` list can be extracted at all — callers
/// fail-open on that. Entries with an unusable `index` are silently
/// skipped, matching the pre-existing behaviour in both callers.
pub fn parse_verdict_entries(text: &str) -> anyhow::Result<Vec<RawVerdictEntry>> {
    let cleaned = crate::output::parser::clean_yaml(text);
    let value: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&cleaned) {
        Ok(v) => v,
        Err(strict_err) => {
            let fenced = crate::output::parser::extract_first_fenced_yaml(text)
                .ok_or_else(|| anyhow::anyhow!("verdict YAML parse failed: {}", strict_err))?;
            serde_yaml_ng::from_str(&fenced)
                .map_err(|fenced_err| anyhow::anyhow!("verdict YAML parse failed: {}", fenced_err))?
        }
    };

    let items = value
        .get("verdicts")
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| anyhow::anyhow!("verdict response has no 'verdicts' list"))?;

    let mut entries = Vec::with_capacity(items.len());
    for item in items {
        let Some(index) = item.get("index").and_then(|v| v.as_u64()) else {
            continue;
        };
        let verdict = item.get("verdict").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let new_severity = item.get("new_severity").and_then(|v| v.as_str()).map(str::to_string);
        let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let cited_lines = item
            .get("cited_lines")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        entries.push(RawVerdictEntry {
            index: index as usize,
            verdict,
            new_severity,
            reason,
            cited_lines,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_yaml_with_full_fields() {
        let text = "```yaml\n\
                    verdicts:\n\
                    \x20 - index: 0\n\
                    \x20   verdict: drop\n\
                    \x20   reason: \"plain prose\"\n\
                    \x20 - index: 1\n\
                    \x20   verdict: downgrade\n\
                    \x20   new_severity: low\n\
                    \x20   reason: \"overstated\"\n\
                    \x20   cited_lines: \"42\"\n\
                    ```";
        let entries = parse_verdict_entries(text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 0);
        assert_eq!(entries[0].verdict, "drop");
        assert_eq!(entries[0].reason, "plain prose");
        assert!(entries[0].new_severity.is_none());
        assert_eq!(entries[1].index, 1);
        assert_eq!(entries[1].verdict, "downgrade");
        assert_eq!(entries[1].new_severity.as_deref(), Some("low"));
        assert_eq!(entries[1].cited_lines, "42");
    }

    #[test]
    fn skips_entries_without_an_index() {
        let text = "verdicts:\n  - verdict: drop\n  - index: 1\n    verdict: keep\n";
        let entries = parse_verdict_entries(text).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].index, 1);
    }

    #[test]
    fn missing_verdicts_list_errors() {
        let err = parse_verdict_entries("summary: \"no verdicts here\"").unwrap_err();
        assert!(err.to_string().contains("verdicts"));
    }

    #[test]
    fn malformed_yaml_errors() {
        let err = parse_verdict_entries("!!! not yaml at all !!!").unwrap_err();
        // The error message mentions YAML / verdict somewhere — the exact
        // wording can shift as `clean_yaml` evolves, so accept any
        // combination of those two tokens.
        let msg = err.to_string();
        assert!(
            msg.contains("verdict") || msg.contains("verdicts") || msg.contains("YAML"),
            "unexpected error: {msg}"
        );
    }
}
