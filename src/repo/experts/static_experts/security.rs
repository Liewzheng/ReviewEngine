use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;

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

pub struct Security;

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
        use crate::repo::analysis::scan_security_patterns;
        let findings = scan_security_patterns(&ctx.entries);
        let details: Vec<ScoreItem> = findings
            .iter()
            .map(|f| {
                let (recommendation, effort) = security_recommendation(&f.pattern);
                ScoreItem {
                    severity: f.severity.clone(),
                    message: format!("{} at {}", f.pattern, f.file),
                    file: Some(f.file.clone()),
                    recommendation: Some(recommendation.to_string()),
                    effort: Some(effort.to_string()),
                    ..Default::default()
                }
            })
            .collect();

        let score = if findings.is_empty() {
            100
        } else {
            let deduction = (findings.len() as i32).min(20) * 8;
            (100 - deduction).clamp(0, 100) as u8
        };

        // Section header and Summary both count the same `details` list;
        // deriving them from one source means they cannot drift apart again
        // (the old synthetic banner inflated `details` by one, making the
        // count diverge from `findings`).
        let finding_count = details.len();

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary: format!("{} security findings", finding_count),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        })
    }
}
