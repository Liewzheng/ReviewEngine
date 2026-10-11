use anyhow::Result;
use async_trait::async_trait;

use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use super::style_tool_key;
use crate::llm::client::LLMClient;

/// Prefix used to mark findings that are hygiene reminders — useful
/// configuration files (`.editorconfig`, `.rustfmt.toml`, …) that are
/// missing — rather than quality defects. The tag lets downstream
/// surfaces (and tests) distinguish "the lint did not find a config"
/// from "the lint found a real style violation", and keeps the
/// missing-config reminder out of the code-quality scoring band
/// (RENG-115: a missing style config is not a quality defect; it
/// must not drag the score to 0 or grab the Top Risk slot).
const HYGIENE_TAG: &str = "[hygiene]";

/// Static expert that reports style-config presence as a hygiene
/// reminder.
///
/// The score is decoupled from missing-config findings: a repo that
/// ships no `.editorconfig` / `.rustfmt.toml` / `.clippy.toml` / etc.
/// is a **hygiene** problem (the user should add the config so linters
/// can run with a known baseline), not a code-quality one. So the
/// expert no longer maps "0 of N configs present" to `0/100` — that
/// landed code_style at the top of the Top-Risks list for repos
/// whose code itself was fine, on every report run, and was the
/// root cause of RENG-115's complaint.
///
/// Instead:
///
/// - Hygiene findings (every missing-config reminder) carry the
///   `[hygiene]` tag in their message and stay severity `note` —
///   visible in the report, just not in the quality scoring channel.
/// - The score stays at `100` whenever no real code-style defect was
///   found. Today the static regex sweep has no body of real style
///   checks (lint findings come from the linters themselves); the
///   score is therefore always `100` in practice, which is the
///   honest answer for a tool that only inventories config files.
///
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
        // Basenames present in the repo. Exact name matching (not
        // `ends_with`), so e.g. "my-rustfmt.toml" cannot satisfy the
        // rustfmt check.
        let present: std::collections::BTreeSet<&str> = ctx
            .entries
            .iter()
            // Skip binary / generated entries — `Cargo.lock` and the
            // like should never satisfy `.editorconfig` etc.
            .filter(|e| !e.is_binary && !e.is_generated)
            .filter_map(|e| std::path::Path::new(&e.path).file_name())
            .filter_map(|n| n.to_str())
            .collect();

        let mut details = Vec::new();
        let mut hygiene_missing = 0usize;
        let mut hygiene_total = 0usize;

        // .editorconfig is language-agnostic and always relevant.
        hygiene_total += 1;
        if present.contains(".editorconfig") {
            // Present — no hygiene finding; do not count as missing.
        } else {
            hygiene_missing += 1;
            details.push(ScoreItem {
                severity: "note".to_string(),
                message: format!(
                    "{} Missing .editorconfig — basic formatter/indent settings have no project-wide source of truth",
                    HYGIENE_TAG
                ),
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
            hygiene_total += 1;
            if files.iter().any(|f| present.contains(f.as_str())) {
                // Present — no hygiene finding; do not count as missing.
            } else {
                hygiene_missing += 1;
                details.push(ScoreItem {
                    severity: "note".to_string(),
                    message: format!(
                        "{} Missing style config for {}: none of [{}] found",
                        HYGIENE_TAG,
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

        // The score measures genuine code-style defects, not the
        // presence of optional configs. The static sweep has no
        // genuine style checks today, so the score is always 100;
        // this keeps a fully unconfigured repo out of the Critical
        // band and out of the Top-Risks slot just because someone
        // forgot to commit a `.rustfmt.toml`.
        let score = 100u8;

        let summary = format!(
            "Style hygiene: {}/{} style configs present ({}); missing items surfaced as [hygiene] notes, score reflects genuine style defects only",
            hygiene_total - hygiene_missing,
            hygiene_total,
            if present.contains(".editorconfig") { "editorconfig ✓" } else { "editorconfig ✗" },
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
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity pin the [`HYGIENE_TAG`] prefix contract: every missing-
    /// config finding in production carries it, so the report-layer
    /// filter (`scoring::is_hygiene_finding`) can distinguish them
    /// from genuine style defects without depending on the precise
    /// tag string. The shared `static_experts::tests::code_style_*`
    /// tests cover the produced findings in full.
    #[test]
    fn hygiene_tag_prefix_marks_missing_config_findings() {
        assert!(HYGIENE_TAG.starts_with('['));
        assert!(HYGIENE_TAG.ends_with(']'));
        // Sanity: the constant is what messages get prefixed with.
        let tagged = format!("{HYGIENE_TAG} Missing .editorconfig");
        assert!(tagged.starts_with(HYGIENE_TAG));
        assert!(tagged.ends_with("Missing .editorconfig"));
    }
}
