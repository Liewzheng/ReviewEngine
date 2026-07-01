use anyhow::Result;
use async_trait::async_trait;

use super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
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
        })
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
        let mut details: Vec<ScoreItem> = findings
            .iter()
            .map(|f| ScoreItem {
                severity: f.severity.clone(),
                message: format!("{} at {}", f.pattern, f.file),
                file: Some(f.file.clone()),
                ..Default::default()
            })
            .collect();

        let score = if findings.is_empty() {
            100
        } else {
            let deduction = (findings.len() as i32).min(20) * 8;
            (100 - deduction).clamp(0, 100) as u8
        };

        if !details.is_empty() {
            details.insert(
                0,
                ScoreItem {
                    severity: "high".to_string(),
                    message: format!("{} security patterns detected", findings.len()),
                    file: None,
                    ..Default::default()
                },
            );
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary: format!("{} security findings", findings.len()),
            details,
        })
    }
}

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
        })
    }
}

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
                ..Default::default()
            });
        }

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary: format!("{} dependencies from Cargo.lock", dep_count),
            details,
        })
    }
}

// ─── CodeStyle ────────────────────────────────

/// Static expert that evaluates code style configuration.
///
/// Checks for presence of `rustfmt.toml`, `clippy.toml`, and
/// `.editorconfig` files. Does not require an LLM.
pub struct CodeStyle;

#[async_trait]
impl RepoExpert for CodeStyle {
    fn name(&self) -> &str {
        "code_style"
    }
    fn weight(&self) -> u8 {
        5
    }
    fn requires_llm(&self) -> bool {
        false
    }

    async fn evaluate(&self, ctx: &RepoContext, _llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let mut details = Vec::new();
        let mut score: i32 = 0;

        // editorconfig is language-agnostic
        if ctx.entries.iter().any(|e| e.path.ends_with(".editorconfig")) {
            score += 25;
        } else {
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: "Missing .editorconfig".to_string(),
                file: None,
                ..Default::default()
            });
        }

        // Language-specific style tooling — check all languages present
        let app_config = ctx.config.as_deref();
        let mut langs_seen = std::collections::BTreeSet::new();
        for entry in &ctx.entries {
            if entry.is_binary || entry.is_generated {
                continue;
            }
            if langs_seen.insert(entry.language.clone()) {
                let profile = crate::language::get_profile(&entry.language, app_config);
                for config_file in &profile.style_configs {
                    if ctx.entries.iter().any(|e| e.path.ends_with(config_file)) {
                        score += 15;
                    }
                }
            }
        }

        let langs_summary: Vec<String> = langs_seen.iter().take(4).cloned().collect();
        let summary = format!(
            "Style: editorconfig={}, detected langs = [{}]",
            if ctx.entries.iter().any(|e| e.path.ends_with(".editorconfig")) {
                "yes"
            } else {
                "no"
            },
            langs_summary.join(", "),
        );

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score: score.clamp(0, 100) as u8,
            summary,
            details,
        })
    }
}
