use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use super::style_tool_key;
use crate::llm::client::LLMClient;

/// Static expert that evaluates code style configuration.
///
/// Normalised scoring: `.editorconfig` (always applicable) plus one check
/// per style tool of every detected language; the score is the share of
/// applicable checks satisfied, so 100 is always reachable. Every missing
/// item produces a `note` finding with a concrete recommendation.
/// Does not require an LLM.
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

        // Basenames present in the repo. Exact name matching (not
        // `ends_with`), so e.g. "my-rustfmt.toml" cannot satisfy the
        // rustfmt check.
        let present: std::collections::BTreeSet<&str> = ctx
            .entries
            .iter()
            .filter_map(|e| std::path::Path::new(&e.path).file_name())
            .filter_map(|n| n.to_str())
            .collect();

        let mut applicable = 0usize;
        let mut satisfied = 0usize;

        // .editorconfig is language-agnostic and always applicable.
        applicable += 1;
        if present.contains(".editorconfig") {
            satisfied += 1;
        } else {
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: "Missing .editorconfig".to_string(),
                file: None,
                recommendation: Some(
                    "Add an .editorconfig at the repository root (e.g. `root = true`, `indent_style = space`, `indent_size = 4`) so every editor applies the same basic formatting."
                        .to_string(),
                ),
                effort: Some("trivial".to_string()),
                ..Default::default()
            });
        }

        // Languages actually detected in the repo (source files only).
        let app_config = ctx.config.as_deref();
        let mut langs_seen = std::collections::BTreeSet::new();
        for entry in &ctx.entries {
            if entry.is_binary || entry.is_generated {
                continue;
            }
            langs_seen.insert(entry.language.clone());
        }

        // Group style configs per tool; several file names can configure
        // the same tool, and a group is satisfied when any of its files is
        // present — otherwise a fully configured repo could never reach
        // 100 (e.g. nobody ships both `rustfmt.toml` and `.rustfmt.toml`).
        // tool key -> (candidate file names, languages that use the tool)
        let mut groups: std::collections::BTreeMap<String, (Vec<String>, Vec<String>)> =
            std::collections::BTreeMap::new();
        for lang in &langs_seen {
            let profile = crate::language::get_profile(lang, app_config);
            for config_file in &profile.style_configs {
                let key = style_tool_key(config_file);
                if key == "editorconfig" {
                    continue; // already counted as the always-applicable item
                }
                let (files, langs) = groups.entry(key).or_default();
                if !files.contains(config_file) {
                    files.push(config_file.clone());
                }
                if !langs.contains(lang) {
                    langs.push(lang.clone());
                }
            }
        }

        for (files, langs) in groups.values() {
            applicable += 1;
            if files.iter().any(|f| present.contains(f.as_str())) {
                satisfied += 1;
            } else {
                details.push(ScoreItem {
                    severity: "note".to_string(),
                    message: format!(
                        "Missing style config for {}: none of [{}] found",
                        langs.join("/"),
                        files.join(", ")
                    ),
                    file: None,
                    recommendation: Some(format!(
                        "Add `{}` at the repository root to pin the {} style configuration instead of relying on tool defaults, which drift between versions.",
                        files[0],
                        langs.join("/")
                    )),
                    effort: Some("trivial".to_string()),
                    ..Default::default()
                });
            }
        }

        // Normalised: hits / applicable × 100, rounded. `applicable` is at
        // least 1 (.editorconfig), so this cannot divide by zero.
        let score = ((satisfied * 100 + applicable / 2) / applicable) as u8;

        let summary = format!(
            "Style: editorconfig={}, {}/{} applicable style configs present, langs=[{}]",
            if present.contains(".editorconfig") { "yes" } else { "no" },
            satisfied,
            applicable,
            langs_seen.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
        );

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary,
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        })
    }
}
