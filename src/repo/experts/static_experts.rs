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

// ─── CodeStyle ────────────────────────────────

/// Normalise a style-config file name to a per-tool key, so that aliases
/// configuring the same tool collapse into one check: `rustfmt.toml` and
/// `.rustfmt.toml` both map to `rustfmt`, `.eslintrc` and `.eslintrc.json`
/// to `eslintrc`. Leading dots are stripped, then the part before the
/// first remaining dot is taken, lower-cased.
fn style_tool_key(config_file: &str) -> String {
    config_file
        .trim_start_matches('.')
        .split('.')
        .next()
        .unwrap_or(config_file)
        .to_ascii_lowercase()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::experts::{ExpertScore, RepoContext, RepoExpert};
    use crate::repo::{FileEntry, RepoStats};

    fn entry(path: &str, language: &str, loc: usize) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: language.to_string(),
            loc,
            is_binary: false,
            is_generated: false,
        }
    }

    fn ctx(entries: Vec<FileEntry>) -> RepoContext {
        let stats = RepoStats {
            total_files: entries.len(),
            total_loc: entries.iter().map(|e| e.loc).sum(),
            ..Default::default()
        };
        RepoContext {
            entries,
            stats,
            llm_configs: Vec::new(),
            config: None,
            facts_block: None,
        }
    }

    async fn evaluate<E: RepoExpert + ?Sized>(expert: &E, context: &RepoContext) -> ExpertScore {
        expert
            .evaluate(context, None)
            .await
            .expect("static expert should not fail")
    }

    /// Build a temp fixture repo that triggers every static finding: a
    /// credential leak (security), a 600-line file (code_organization),
    /// a Cargo.lock with 201 packages (dependency), and nothing else so the
    /// documentation/code_style "missing file" findings fire. Returns the
    /// context plus the TempDir keep-alive handle.
    fn fixture_context() -> (RepoContext, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let big_path = dir.path().join("src").join("big.rs");
        std::fs::create_dir_all(big_path.parent().unwrap()).unwrap();
        let big_body = (0..600)
            .map(|i| format!("fn f{i}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&big_path, big_body).unwrap();

        let secret_path = dir.path().join("config").join("secret.env");
        std::fs::create_dir_all(secret_path.parent().unwrap()).unwrap();
        std::fs::write(&secret_path, "api_key = \"aaaaaaaaaaaaaaaa\"\n").unwrap();

        let lock_path = dir.path().join("Cargo.lock");
        let mut lock = String::from("version = 3\n\n");
        for i in 0..201 {
            lock.push_str(&format!("[[package]]\nname = \"pkg{i}\"\nversion = \"0.1.0\"\n\n"));
        }
        std::fs::write(&lock_path, lock).unwrap();

        let entries = vec![
            entry(big_path.to_str().unwrap(), "Rust", 600),
            entry(secret_path.to_str().unwrap(), "Config", 1),
            entry(lock_path.to_str().unwrap(), "Config", 604),
        ];
        (ctx(entries), dir)
    }

    #[tokio::test]
    async fn security_details_len_matches_summary_and_has_no_banner() {
        let (context, _dir) = fixture_context();
        let score = evaluate(&Security, &context).await;
        assert!(!score.details.is_empty(), "fixture must contain a credential hit");

        // Summary count must equal the rendered details count (the old
        // synthetic banner inflated details by one and made them diverge).
        let summary_count: usize = score
            .summary
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .expect("summary should start with a count");
        assert_eq!(summary_count, score.details.len());

        // No synthetic banner: every detail is a real hit with a file path,
        // and none claims a bare "N security patterns detected" count.
        for d in &score.details {
            assert!(d.file.is_some(), "banner pseudo-finding must be gone: {}", d.message);
            assert!(
                !d.message.ends_with(" security patterns detected"),
                "banner message should not exist: {}",
                d.message
            );
        }
    }

    #[tokio::test]
    async fn static_findings_have_recommendation_and_effort() {
        let (context, _dir) = fixture_context();
        let experts: Vec<Box<dyn RepoExpert>> = vec![
            Box::new(CodeOrganization),
            Box::new(Security),
            Box::new(Documentation),
            Box::new(Dependency),
            Box::new(CodeStyle),
        ];
        for expert in &experts {
            let score = evaluate(expert.as_ref(), &context).await;
            for d in &score.details {
                let rec = d
                    .recommendation
                    .as_deref()
                    .unwrap_or_else(|| panic!("{} detail missing recommendation: {}", score.expert_name, d.message));
                assert!(
                    !rec.trim().is_empty(),
                    "{} detail has empty recommendation: {}",
                    score.expert_name,
                    d.message
                );
                let effort = d
                    .effort
                    .as_deref()
                    .unwrap_or_else(|| panic!("{} detail missing effort: {}", score.expert_name, d.message));
                assert!(
                    ["trivial", "small", "medium", "large"].contains(&effort),
                    "{} detail has unexpected effort {effort:?}: {}",
                    score.expert_name,
                    d.message
                );
            }
        }
    }

    #[tokio::test]
    async fn security_clean_repo_has_no_findings() {
        // A context with no credentials must score 100 and emit no details —
        // including no synthetic "0 security patterns detected" banner.
        let context = ctx(vec![entry("src/main.rs", "Rust", 10)]);
        let score = evaluate(&Security, &context).await;
        assert_eq!(score.score, 100);
        assert!(score.details.is_empty());
        assert_eq!(score.summary, "0 security findings");
    }

    // ─── CodeStyle normalisation ───────────────────────────────

    #[tokio::test]
    async fn code_style_full_score_when_all_applicable_configs_present() {
        // Single-language Rust repo with every applicable config: the
        // always-applicable .editorconfig plus the rustfmt and clippy tool
        // groups (`.rustfmt.toml` satisfies the rustfmt group — aliases
        // count once, not twice).
        let context = ctx(vec![
            entry("src/main.rs", "Rust", 10),
            entry(".editorconfig", "Other", 5),
            entry(".rustfmt.toml", "Config", 3),
            entry("clippy.toml", "Config", 3),
        ]);
        let score = evaluate(&CodeStyle, &context).await;
        assert_eq!(score.score, 100, "summary: {}", score.summary);
        assert!(
            score.details.is_empty(),
            "no findings expected, got: {:?}",
            score.details.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        assert!(score.summary.contains("3/3"), "summary: {}", score.summary);
    }

    #[tokio::test]
    async fn code_style_missing_configs_emit_note_findings() {
        // Rust repo with only rustfmt configured: 1 of 3 applicable items.
        let context = ctx(vec![
            entry("src/main.rs", "Rust", 10),
            entry("rustfmt.toml", "Config", 3),
        ]);
        let score = evaluate(&CodeStyle, &context).await;
        assert_eq!(score.score, 33, "summary: {}", score.summary);
        assert_eq!(score.details.len(), 2);
        let messages: Vec<&str> = score.details.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains(".editorconfig")),
            "expected a missing-.editorconfig finding: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("clippy.toml")),
            "expected a missing-clippy finding: {messages:?}"
        );
        for d in &score.details {
            assert_eq!(d.severity, "note");
            assert!(
                d.recommendation.is_some(),
                "finding must say how to fix it: {}",
                d.message
            );
        }
    }

    #[tokio::test]
    async fn code_style_normalises_across_languages_and_groups_aliases() {
        // Rust + Python; ruff configured via the `.ruff.toml` alias, mypy
        // missing. Applicable: editorconfig + rustfmt + clippy + ruff +
        // mypy = 5; satisfied = 4.
        let context = ctx(vec![
            entry("src/main.rs", "Rust", 10),
            entry("app.py", "Python", 20),
            entry(".editorconfig", "Other", 5),
            entry("rustfmt.toml", "Config", 3),
            entry("clippy.toml", "Config", 3),
            entry(".ruff.toml", "Config", 3),
        ]);
        let score = evaluate(&CodeStyle, &context).await;
        assert_eq!(score.score, 80, "summary: {}", score.summary);
        assert_eq!(score.details.len(), 1);
        assert!(
            score.details[0].message.contains("mypy.ini"),
            "the only finding should be the missing mypy config: {}",
            score.details[0].message
        );
    }
}
