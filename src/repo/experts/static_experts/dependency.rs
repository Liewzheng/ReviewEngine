use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;

// ─── Dependency ───────────────────────────────

/// Static expert that evaluates dependency health from `Cargo.lock`.
///
/// Counts declared dependencies and flags repositories with more than
/// 200 dependencies for audit. Does not require an LLM.
pub struct Dependency;

#[async_trait]
impl RepoExpert for Dependency {
    fn name(&self) -> &str {
        "dependency"
    }
    fn weight(&self) -> u8 {
        10
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let mut details = Vec::new();

        // Count dependencies from Cargo.lock
        let dep_count = ctx
            .entries
            .iter()
            .filter(|e| e.path.ends_with("Cargo.lock"))
            .filter_map(|e| std::fs::read_to_string(&e.path).ok())
            .map(|content| content.lines().filter(|l| l.trim().starts_with("name = ")).count())
            .next()
            .unwrap_or(0);

        let score = if dep_count == 0 {
            100
        } else if dep_count > 200 {
            60
        } else if dep_count > 100 {
            75
        } else if dep_count > 50 {
            85
        } else {
            95
        };

        if dep_count > 200 {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: format!("{} dependencies — consider auditing for stale packages", dep_count),
                file: None,
                recommendation: Some(
                    "Run `cargo audit` for known vulnerabilities and update stale or duplicate dependencies."
                        .to_string(),
                ),
                effort: Some("medium".to_string()),
                ..Default::default()
            });
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary: format!("{} dependencies from Cargo.lock", dep_count),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        })
    }
}
