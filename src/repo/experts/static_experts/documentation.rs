use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;

// ─── Documentation ────────────────────────────

/// Static expert that evaluates documentation quality in the repository.
///
/// Checks for presence of README, CHANGELOG, and LICENSE files, and
/// measures the comment-to-code ratio in Rust source files.
/// Does not require an LLM.
pub struct Documentation;

#[async_trait]
impl RepoExpert for Documentation {
    fn name(&self) -> &str {
        "documentation"
    }
    fn weight(&self) -> u8 {
        10
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let mut score: i32 = 0;
        let mut details = Vec::new();

        // README
        let has_readme = ctx.entries.iter().any(|e| e.path.ends_with("README.md"));
        if has_readme {
            score += 30;
        } else {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: "Missing README.md".to_string(),
                file: None,
                recommendation: Some("Add a README.md describing the project's purpose, setup, and usage.".to_string()),
                effort: Some("small".to_string()),
                ..Default::default()
            });
        }

        // CHANGELOG
        let has_changelog = ctx.entries.iter().any(|e| e.path.ends_with("CHANGELOG.md"));
        if has_changelog {
            score += 20;
        } else {
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: "Missing CHANGELOG.md".to_string(),
                file: None,
                recommendation: Some("Add a CHANGELOG.md to track user-visible changes per release.".to_string()),
                effort: Some("small".to_string()),
                ..Default::default()
            });
        }

        // LICENSE
        let has_license = ctx.entries.iter().any(|e| e.path.contains("LICENSE"));
        if has_license {
            score += 20;
        } else {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: "Missing LICENSE file".to_string(),
                file: None,
                recommendation: Some("Add a LICENSE file at the repository root.".to_string()),
                effort: Some("trivial".to_string()),
                ..Default::default()
            });
        }

        // Comment ratio — per-file language-aware
        let app_config = ctx.config.as_deref();
        let mut comment_lines: usize = 0;
        let mut total_lines: usize = 0;

        for entry in &ctx.entries {
            if entry.is_binary || entry.is_generated {
                continue;
            }
            let profile = crate::language::get_profile(&entry.language, app_config);
            let prefixes = crate::language::all_comment_prefixes(&profile);
            if let Ok(content) = std::fs::read_to_string(&entry.path) {
                total_lines += content.lines().count();
                comment_lines += content
                    .lines()
                    .filter(|l| prefixes.iter().any(|p| l.trim().starts_with(p)))
                    .count();
            }
        }

        let comment_ratio = if total_lines > 0 {
            comment_lines as f64 / total_lines as f64
        } else {
            0.0
        };
        if comment_ratio > 0.1 {
            score += 30;
        } else if comment_ratio > 0.05 {
            score += 15;
        } else {
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: format!("Low comment ratio ({:.1}%)", comment_ratio * 100.0),
                file: None,
                recommendation: Some(
                    "Add doc comments to public API items and comments to non-obvious logic.".to_string(),
                ),
                effort: Some("medium".to_string()),
                ..Default::default()
            });
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score: score.clamp(0, 100) as u8,
            summary: format!(
                "README={}, CHANGELOG={}, LICENSE={}, comments {:.1}%",
                if has_readme { "yes" } else { "no" },
                if has_changelog { "yes" } else { "no" },
                if has_license { "yes" } else { "no" },
                comment_ratio * 100.0
            ),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        })
    }
}
