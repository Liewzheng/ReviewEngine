use anyhow::Result;
use async_trait::async_trait;

use super::super::facts::is_test_file;
use super::super::{ExpertScore, RepoContext, RepoExpert, ScoreItem};
use crate::llm::client::LLMClient;
use crate::repo::FileEntry;

// ─── CodeOrganization ─────────────────────────

/// Static expert that evaluates repository code organisation.
///
/// Checks directory nesting depth, file-count-to-volume ratio, and
/// identifies overly large source files. Does not require an LLM.
pub struct CodeOrganization;

/// Whether a language counts as a "logic language" for the large-file
/// statistic. `Documentation`/`Config` carry no logic, `Web`
/// (html/css/scss/less) is presentational, and `Other` covers unknown
/// extensions — none of them say anything about code complexity, so
/// their size must not feed the large-file deduction. Everything else
/// (Rust, TypeScript, JavaScript, Python, Go, Vue, …) counts.
///
/// Note the files are only excluded from THIS statistic: LLM experts
/// still review them in full.
fn is_logic_language(language: &str) -> bool {
    !matches!(language, "Documentation" | "Config" | "Web" | "Other")
}

/// LOC of the `<script>` block(s) of a Vue single-file component.
///
/// Simple marker scan: lines strictly between a `<script...>` opener
/// (`<script>`, `<script setup>`, `<script setup lang="ts">`, …) and the
/// next `</script>` closing tag count; template/style sections do not.
/// Multiple script blocks (plain `<script>` plus `<script setup>`) are
/// summed. Rationale: SFC template/style LOC is presentational; the
/// script block is what reflects logic complexity.
///
/// `pub(crate)` so the shared static-expert test module can unit-test the
/// marker scan directly.
pub(crate) fn vue_script_loc(content: &str) -> usize {
    let mut loc = 0usize;
    let mut in_script = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if !in_script {
            if trimmed.starts_with("<script") {
                in_script = true;
            }
            continue;
        }
        if trimmed.starts_with("</script") {
            in_script = false;
            continue;
        }
        loc += 1;
    }
    loc
}

/// Effective LOC of `entry` for the large-file statistic, or `None` when
/// the entry is out of scope and must not feed the deduction at all.
///
/// Out of scope: binary/generated entries, non-logic languages
/// (Documentation/Config/Web/Other — see [`is_logic_language`]), and test
/// files (shared [`is_test_file`] conventions: `tests.rs` / `*_tests.rs`
/// siblings, `tests/`/`test/`/`__tests__/` directory segments,
/// `*.test.*` / `*.spec.*` / `*_test.*` basenames).
///
/// Vue SFCs count only their `<script>` block LOC: the entry's `loc` is
/// the whole file (script + template + style), so the content is read
/// from disk via the entry's path (the same pattern `facts` and
/// `test_coverage` already use). Fail-open: if the file is unreadable
/// (deleted after scanning, permissions, …) the entry is EXCLUDED from
/// the statistic rather than counted at full-file LOC, which would
/// penalise presentational template/style lines the scope is meant to
/// ignore.
fn scoped_logic_loc(entry: &FileEntry) -> Option<usize> {
    if entry.is_binary || entry.is_generated || !is_logic_language(&entry.language) {
        return None;
    }
    let name = std::path::Path::new(&entry.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if is_test_file(name, &entry.path) {
        return None;
    }
    if entry.language == "Vue" {
        let content = std::fs::read_to_string(&entry.path).ok()?;
        return Some(vue_script_loc(&content));
    }
    Some(entry.loc)
}

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
        // Scope (see `scoped_logic_loc`): logic languages only, no test
        // files, and only the `<script>` block of Vue SFCs. Excluded files
        // are still fully reviewed by the LLM experts — they simply do not
        // feed this deduction.
        let mut excess: usize = 0;
        let mut large_count: usize = 0;
        for e in &ctx.entries {
            let Some(loc) = scoped_logic_loc(e) else { continue };
            if loc > 500 {
                large_count += 1;
                excess += loc - 500;
            }
        }
        let large_deduction = (excess / 100).min(40) as i32;
        if large_deduction > 0 {
            details.push(ScoreItem {
                severity: "medium".to_string(),
                message: format!(
                    "{} code files exceed 500 lines ({} excess LOC; tests, Web/Config/Documentation, and non-script Vue sections excluded)",
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
