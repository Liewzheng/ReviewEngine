use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;

// ─── CodeOrganization ─────────────────────────

/// Static expert that evaluates repository code organisation.
///
/// Checks directory nesting depth, file-count-to-volume ratio, and
/// identifies overly large source files. Does not require an LLM.
pub struct CodeOrganization;

#[async_trait]
impl RepoExpert for CodeOrganization {
    fn name(&self) -> &str {
        "code_organization"
    }
    fn weight(&self) -> u8 {
        15
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let mut details = Vec::new();
        let mut score: i32 = 100;
        let source_count = ctx.entries.iter().filter(|e| !e.is_binary && !e.is_generated).count();
        let source_loc: usize = ctx
            .entries
            .iter()
            .filter(|e| !e.is_binary && !e.is_generated)
            .map(|e| e.loc)
            .sum();

        // Penalize very deep directory nesting (more than 4 levels from src/)
        let max_depth = ctx
            .entries
            .iter()
            .filter_map(|e| std::path::Path::new(&e.path).parent())
            .filter_map(|p| p.to_str())
            .filter(|p| p.starts_with("src/"))
            .map(|p| p.matches('/').count())
            .max()
            .unwrap_or(0);
        if max_depth > 4 {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: format!("Deep directory nesting ({} levels)", max_depth),
                file: None,
                recommendation: Some(
                    "Flatten the directory structure to keep nesting below 4 levels and reduce import complexity."
                        .to_string(),
                ),
                effort: Some("medium".to_string()),
                ..Default::default()
            });
            score -= 10;
        }

        // Penalize if the repo is all-in-one file
        if source_count <= 3 && source_loc > 1000 {
            details.push(ScoreItem {
                severity: "high".to_string(),
                message: "Very few files for the code volume".to_string(),
                file: None,
                recommendation: Some(
                    "Split the monolithic file(s) into modules by responsibility to separate concerns.".to_string(),
                ),
                effort: Some("large".to_string()),
                ..Default::default()
            });
            score -= 20;
        }

        let avg = source_loc.checked_div(source_count).unwrap_or(0);

        // Graduated penalty for large files: 1 point per 100 lines over 500,
        // capped at 40.  This is fairer than a flat per-file deduction — a
        // 550-line file and a 1055-line file should not cost the same.
        let excess: usize = ctx
            .entries
            .iter()
            .filter(|e| !e.is_binary && !e.is_generated && e.language != "Documentation" && e.language != "Config")
            .map(|e| if e.loc > 500 { e.loc - 500 } else { 0 })
            .sum();
        let large_count = ctx
            .entries
            .iter()
            .filter(|e| !e.is_binary && !e.is_generated && e.language != "Documentation" && e.language != "Config")
            .filter(|e| e.loc > 500)
            .count();
        let large_deduction = (excess / 100).min(40) as i32;
        if large_deduction > 0 {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: format!(
                    "{} files exceed 500 lines ({} excess LOC across all files)",
                    large_count, excess
                ),
                file: None,
                recommendation: Some("Split the oversized files into smaller modules by responsibility.".to_string()),
                effort: Some("medium".to_string()),
                ..Default::default()
            });
            score -= large_deduction;
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score: score.clamp(0, 100) as u8,
            summary: format!(
                "{} source files, avg {} LOC/file, {} large files",
                source_count, avg, large_count
            ),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        })
    }
}
