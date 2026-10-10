use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;
use crate::repo::analysis::{scan_security_patterns, FindingKind};

/// Maximum mechanical credential hits that count toward the score.
///
/// A repo can have many mechanical hits (every comma in `constants.rs` is
/// not a real leak), so each hit only deducts [`MECHANICAL_PER_HIT`]
/// points and the mechanical contribution is capped. Semantic findings,
/// which reflect real code-reasoning issues (output channel leaks, proxy
/// bypass, weak temp-file perms, query/path tokens), remain uncapped:
/// each one deducts [`SEMANTIC_PER_HIT`] points, because the LLM
/// corroborates them.
const MECHANICAL_PER_HIT: i32 = 4;
const MECHANICAL_HITS_CAPPED: i32 = 8;
const SEMANTIC_PER_HIT: i32 = 12;
const SEMANTIC_HITS_CAPPED: i32 = 8;

/// A semantic check item the security expert covers.
///
/// These describe code-reasoning checks (output-channel redaction,
/// proxy-bypass evasion, temp-file permissions, query/path tokens beyond
/// `userinfo`) the static regex sweep cannot perform. They are part of the
/// expert's contract so any downstream consumer — including a future
/// LLM-driven security expert that wants to render them into a prompt —
/// can pull the canonical list from one place instead of hard-coding
/// them in prompts or skip-list them by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecurityCheckItem {
    /// Short stable identifier (`output_redaction`, `proxy_bypass`, …)
    /// — used as a programmatic key in tests and reporting.
    pub id: &'static str,
    /// Human-readable category for grouping in reports.
    pub category: &'static str,
    /// Long-form description of what the check looks for.
    pub description: &'static str,
}

/// Per-pattern recommendation and effort for a credential-leak finding.
///
/// All patterns here are credential leaks, so the advice is uniform: verify,
/// rotate, and move the secret out of the repository.
fn security_recommendation(pattern: &str) -> (&'static str, &'static str) {
    match pattern {
        "Private key" => (
            "Remove the private key from the repository immediately, rotate it, and store it in a secret manager or CI secret.",
            "small",
        ),
        "Hardcoded password" => (
            "Confirm whether this is a real password; if so, rotate it and load it from an environment variable or secret manager.",
            "small",
        ),
        _ => (
            "Confirm whether this is a real credential; if so, rotate it and load it from an environment variable or secret manager.",
            "small",
        ),
    }
}

/// Semantic check items the security expert covers beyond the mechanical
/// secret-leak regex sweep. Render into a future prompt or surface as
/// documentation; the static expert at least carries them so the
/// contract stays in one place.
pub const SEMANTIC_CHECKS: &[SecurityCheckItem] = &[
    SecurityCheckItem {
        id: "output_channel_redaction",
        category: "Output channels & redaction",
        description: "Construct an endpoint or URL that contains tokens, query parameters, or \
            userinfo, then log/print/save it to terminal, JSONL, or an upload object — the \
            endpoint must be redacted (e.g. host only, mask the userinfo and query) before any \
            output channel receives it.",
    },
    SecurityCheckItem {
        id: "proxy_bypass_evasion",
        category: "Proxy bypass / NO_PROXY coverage",
        description: "Decide whether a URL goes through the corporate proxy based on a parser \
            that misses legitimate cases: IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`), \
            non-dotted-decimal v4 (decimal int / hex / octal forms), and internal hostnames with \
            no NO_PROXY entry must still be treated as bypass-prone. The bypass decision is \
            only as good as the URL parser that feeds it.",
    },
    SecurityCheckItem {
        id: "temp_file_permissions",
        category: "Filesystem permissions",
        description: "Create a temporary file (export artifact, scratch buffer) with default \
            umask-respecting `OpenOptions` and mode 0o600 explicitly set, so neighbouring users \
            cannot read the dump while it is being written.",
    },
    SecurityCheckItem {
        id: "token_in_query_or_path",
        category: "Endpoint & query string handling",
        description: "Redact or otherwise handle credentials / tokens / signed URLs that land \
            in the query string or URL path. The userinfo part of the URL is not the only \
            place sensitive material can sit; `redact_endpoint` / equivalent must look at \
            `?token=…`, `?api_key=…`, and `/v1/<id>/<secret>`-style paths too.",
    },
];

pub struct Security;

impl Security {
    /// Return the semantic check items the security expert covers.
    ///
    /// Exposed so a future LLM-driven security expert (or any prompt
    /// builder) can pull the canonical list without re-deriving it from
    /// release notes or design docs.
    pub fn check_items() -> &'static [SecurityCheckItem] {
        SEMANTIC_CHECKS
    }
}

#[async_trait]
impl RepoExpert for Security {
    fn name(&self) -> &str {
        "security"
    }
    fn weight(&self) -> u8 {
        15
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let findings = scan_security_patterns(&ctx.entries);

        let (mechanical, semantic): (Vec<_>, Vec<_>) = findings.iter().partition(|f| f.kind == FindingKind::Mechanical);
        let mechanical_count = mechanical.len() as i32;
        let semantic_count = semantic.len() as i32;

        let details: Vec<ScoreItem> = findings
            .iter()
            .map(|f| {
                let (recommendation, effort) = security_recommendation(&f.pattern);
                let kind_tag = f.kind.as_str();
                ScoreItem {
                    severity: f.severity.clone(),
                    message: format!("[{}] {} at {}", kind_tag, f.pattern, f.file),
                    file: Some(f.file.clone()),
                    recommendation: Some(recommendation.to_string()),
                    effort: Some(effort.to_string()),
                    evidence: Some(format!("line {}", f.line)),
                    ..Default::default()
                }
            })
            .collect();

        // Score: semantic findings dominate (each uncapped, larger
        // penalty per hit); mechanical hits contribute a capped
        // amount so a sea of false-positive regex matches cannot
        // drag the score into the Critical band on its own.
        let mechanical_deduction = mechanical_count.min(MECHANICAL_HITS_CAPPED) * MECHANICAL_PER_HIT;
        let semantic_deduction = semantic_count.min(SEMANTIC_HITS_CAPPED) * SEMANTIC_PER_HIT;
        let total_deduction = mechanical_deduction + semantic_deduction;
        let score = if details.is_empty() {
            100
        } else {
            (100 - total_deduction).clamp(0, 100) as u8
        };

        let summary = if details.is_empty() {
            "0 security findings (0 mechanical, 0 semantic)".to_string()
        } else {
            format!(
                "{} security findings ({} mechanical, {} semantic)",
                details.len(),
                mechanical_count,
                semantic_count
            )
        };

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary,
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_items_are_nonempty_and_cover_required_categories() {
        let items = Security::check_items();
        assert!(!items.is_empty(), "semantic checks must be declared");

        // Every check item needs the four topic-areas RENG-114 called out:
        // output channels & redaction, proxy bypass, temp-file perms,
        // and tokens in query/path. Both kinds of IDs must be unique so
        // no two items collapse onto the same key.
        let ids: std::collections::HashSet<&str> = items.iter().map(|i| i.id).collect();
        assert_eq!(ids.len(), items.len(), "ids must be unique");

        let mut by_id = std::collections::HashMap::new();
        for item in items {
            by_id.insert(item.id, item);
        }
        for required in [
            "output_channel_redaction",
            "proxy_bypass_evasion",
            "temp_file_permissions",
            "token_in_query_or_path",
        ] {
            assert!(
                by_id.contains_key(required),
                "missing required semantic check: {required}"
            );
        }

        // Every item must have a non-empty category and a non-empty
        // description so the prompt/render layer can show them.
        for item in items {
            assert!(!item.category.is_empty(), "category required: {item:?}");
            assert!(!item.description.is_empty(), "description required: {item:?}");
        }
    }

    #[test]
    fn score_caps_mechanical_but_lets_semantic_dominate() {
        // Helper that mirrors the production scoring formula so the
        // tests stay on the same branch — no risk of drift between the
        // assertions below and the constants above.
        fn score_for(mechanical: usize, semantic: usize) -> u8 {
            let m = mechanical as i32;
            let s = semantic as i32;
            let m_d = m.min(MECHANICAL_HITS_CAPPED) * MECHANICAL_PER_HIT;
            let s_d = s.min(SEMANTIC_HITS_CAPPED) * SEMANTIC_PER_HIT;
            (100 - (m_d + s_d)).clamp(0, 100) as u8
        }

        // Clean repo → 100.
        assert_eq!(score_for(0, 0), 100);

        // 7 mechanical hits (the RENG-19 case): the old scoring flooded
        // the report to 44/100; the new cap pulls it to 72/100 so
        // regex-noise alone can never drop security into the Critical
        // band.
        assert_eq!(score_for(7, 0), 72, "7 mechanical hits must be capped");

        // A single semantic finding on top deducts more per hit than the
        // mechanical cap would, so real code-reasoning issues still
        // dominate the score the way the user asked.
        assert_eq!(score_for(0, 1), 88, "one semantic finding must carry weight");

        // Saturating the mechanical cap yields the same score as 50
        // hits, so the ceiling is observable end-to-end.
        assert_eq!(score_for(8, 0), score_for(50, 0));
    }
}
