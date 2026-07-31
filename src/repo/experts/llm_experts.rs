use anyhow::Result;
use async_trait::async_trait;

use super::{ExpertScore, RepoContext, RepoExpert};
use crate::llm::client::LLMClient;
use crate::prompt::templates;

/// Maximum bytes of a raw LLM response kept in a parse-failure warning.
/// Bounded so a huge or runaway response cannot flood the log line, while
/// still leaving enough to identify the shape the model actually returned.
const EXCERPT_MAX_BYTES: usize = 300;

/// Keys that mark a CodeQuality response as schema-conforming. A mapping that
/// has none of these (e.g. an unexpected `verdicts:` shape) is treated as a
/// schema drift and falls back, because every field would default silently.
const CODE_QUALITY_KEYS: &[&str] = &["score", "summary", "findings"];

/// Keys that mark an ArchitectureLead response as schema-conforming.
const ARCHITECTURE_LEAD_KEYS: &[&str] = &["score", "summary", "risk_areas", "guidance", "focus_modules"];

/// Parse an LLM expert's YAML response into a [`serde_yaml_ng::Value`].
///
/// Mirrors `crate::output::parser`'s extraction strategy: strip code fences
/// via [`crate::output::parser::clean_yaml`], attempt a strict parse, and on
/// failure retry with the first fenced YAML block in isolation. When neither
/// yields a schema-conforming mapping, logs a `warn!` (with a truncated
/// excerpt of the raw response) and returns `Null`, so the caller's field
/// fallbacks apply.
///
/// The warning distinguishes three failure modes so operators can tell them
/// apart without a full response dump:
/// 1. **Empty response** — the API returned zero bytes (observed with
///    reasoning models whose whole `max_tokens` budget is consumed by
///    `reasoning_tokens`, leaving `content` empty).
/// 2. **Unparseable** — non-empty text that is not valid YAML (prose prefix,
///    broken fences, truncated document).
/// 3. **Schema drift** — valid YAML that is a mapping but has none of
///    `expected_keys`, so `score`/`findings` would all silently fall back.
///
/// A `Null` return means the caller is about to use fallback values — never
/// model-provided ones — which lets `score`/`findings` fallbacks be told apart
/// from genuine model output in the logs.
fn parse_expert_yaml(expert: &str, raw: &str, expected_keys: &[&str]) -> serde_yaml_ng::Value {
    if raw.trim().is_empty() {
        tracing::warn!(
            expert_name = expert,
            "LLM returned empty response; using fallback score and empty findings"
        );
        return serde_yaml_ng::Value::Null;
    }

    let cleaned = crate::output::parser::clean_yaml(raw);
    let parsed = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned).ok();
    if let Some(v) = &parsed {
        if v.is_mapping() && expected_keys.iter().any(|k| !v[*k].is_null()) {
            return v.clone();
        }
    }
    if let Some(fenced) = crate::output::parser::extract_first_fenced_yaml(raw) {
        if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&fenced) {
            if v.is_mapping() && expected_keys.iter().any(|k| !v[*k].is_null()) {
                return v;
            }
        }
    }

    if parsed.as_ref().is_some_and(|v| v.is_mapping()) {
        tracing::warn!(
            expert_name = expert,
            raw_len = raw.len(),
            excerpt = %truncate_excerpt(raw),
            "LLM response parsed as YAML but missing expected keys {expected_keys:?}; using fallback score and empty findings"
        );
    } else {
        tracing::warn!(
            expert_name = expert,
            raw_len = raw.len(),
            excerpt = %truncate_excerpt(raw),
            "LLM response failed YAML parse; using fallback score and empty findings"
        );
    }
    serde_yaml_ng::Value::Null
}

/// First ~300 bytes of a raw response, with newlines collapsed, for logs.
fn truncate_excerpt(raw: &str) -> String {
    let mut excerpt = String::new();
    for ch in raw.chars() {
        if excerpt.len() + ch.len_utf8() > EXCERPT_MAX_BYTES {
            break;
        }
        excerpt.push(ch);
    }
    excerpt.replace('\n', "\\n")
}

/// Architecture Lead: Pass 1 expert that examines the file tree and produces a
/// high-level assessment of the repository structure and risks.
pub struct ArchitectureLead;

#[async_trait]
impl RepoExpert for ArchitectureLead {
    fn name(&self) -> &str {
        "architecture_lead"
    }
    fn weight(&self) -> u8 {
        15
    }
    fn requires_llm(&self) -> bool {
        true
    }

    async fn evaluate(&self, ctx: &RepoContext, llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let llm = llm.ok_or_else(|| anyhow::anyhow!("ArchitectureLead requires LLM"))?;

        // Build file tree and stats overview
        let file_tree: Vec<String> = ctx
            .entries
            .iter()
            .filter(|e| !e.is_binary && !e.is_generated)
            .map(|e| {
                let in_reports = e.path.contains("/review_reports/");
                if in_reports {
                    return String::new();
                }
                format!("{} ({} LOC, {})", e.path, e.loc, e.language)
            })
            .filter(|s| !s.is_empty())
            .collect();

        let lang_summary: Vec<String> = ctx
            .stats
            .languages
            .iter()
            .map(|(name, st)| format!("{}: {} files, {} LOC", name, st.files, st.loc))
            .collect();

        let system = templates::ARCHITECTURE_LEAD_SYSTEM_TEMPLATE;

        let user = format!(
            "## Repository File Tree\n\
             Total files: {} (source), {} total LOC, {} languages\n\n\
             ## Language Breakdown\n\
             {}\n\n\
             ## File Tree\n\
             {}",
            file_tree.len(),
            ctx.stats.total_loc,
            ctx.stats.languages.len(),
            lang_summary.join("\n"),
            file_tree.join("\n"),
        );

        let response = llm.complete_with_fallback(&ctx.llm_configs, system, &user).await?;

        // Parse YAML response
        let value = parse_expert_yaml("architecture_lead", &response.content, ARCHITECTURE_LEAD_KEYS);
        let score = value["score"].as_u64().unwrap_or(70).min(100) as u8;
        let summary = value["summary"]
            .as_str()
            .unwrap_or("Architecture assessment completed")
            .to_string();
        let risk_items: Vec<serde_yaml_ng::Value> = value["risk_areas"].as_sequence().cloned().unwrap_or_default();
        let details = super::parse_yaml_findings(&risk_items);

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary,
            details,
        })
    }
}

/// Render the CodeQuality system prompt by substituting the `{{ ... }}`
/// placeholders in [`templates::CODE_QUALITY_SYSTEM_TEMPLATE`]. The template
/// uses MiniJinja-style `{{ name }}` markers but is not routed through the
/// `PromptEngine`, so the substitution is done here with plain `str::replace`.
fn render_code_quality_system(module: &str, lang: &str, naming_hint: &str, error_hint: &str) -> String {
    templates::CODE_QUALITY_SYSTEM_TEMPLATE
        .replace("{{ module }}", module)
        .replace("{{ lang }}", lang)
        .replace("{{ naming_hint }}", naming_hint)
        .replace("{{ error_hint }}", error_hint)
}

/// CodeQuality: Pass 2 expert that evaluates code quality for a specific chunk.
/// Requires RepoGlobalContext injected via the prompt.
pub struct CodeQuality;

#[async_trait]
impl RepoExpert for CodeQuality {
    fn name(&self) -> &str {
        "code_quality"
    }
    fn weight(&self) -> u8 {
        10
    }
    fn requires_llm(&self) -> bool {
        true
    }

    async fn evaluate(&self, ctx: &RepoContext, llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let llm = llm.ok_or_else(|| anyhow::anyhow!("CodeQuality requires LLM"))?;
        let app_config = ctx.config.as_deref();

        // Read all non-binary, non-generated source files (language-agnostic)
        let source_files: Vec<String> = ctx
            .entries
            .iter()
            .filter(|e| !e.is_binary && !e.is_generated)
            .map(|e| {
                let content = match std::fs::read_to_string(&e.path) {
                    Ok(c) => c,
                    Err(err) => {
                        tracing::warn!("CodeQuality: failed to read {}: {:?}", e.path, err);
                        format!("// file {} could not be read: {err}", e.path)
                    }
                };
                format!("// --- {} ---\n{}\n", e.path, content)
            })
            .collect();

        // Module info from the chunk — extract from the first file path
        let first_file = ctx.entries.first();
        let module_name = first_file
            .and_then(|e| {
                let p = std::path::Path::new(&e.path);
                p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str())
            })
            .unwrap_or("unknown");

        // Use language profile of the first file for prompt hints
        let first_lang = first_file.map(|e| e.language.as_str()).unwrap_or("Rust");
        let profile = crate::language::get_profile(first_lang, app_config);

        let system = render_code_quality_system(module_name, first_lang, &profile.naming_hint, &profile.error_hint);

        let user = format!(
            "## Module: {module} ({lang})\n\
             Files in this module: {count}\n\n\
             ## Code\n\
             {code}",
            module = module_name,
            lang = first_lang,
            count = ctx.entries.len(),
            code = source_files.join("\n\n---\n\n"),
        );

        let response = llm.complete_with_fallback(&ctx.llm_configs, &system, &user).await?;

        let value = parse_expert_yaml("code_quality", &response.content, CODE_QUALITY_KEYS);
        let score = value["score"].as_u64().unwrap_or(70).min(100) as u8;
        let summary = value["summary"]
            .as_str()
            .unwrap_or("Code quality assessment completed")
            .to_string();

        let details = if let Some(findings) = value["findings"].as_sequence() {
            super::parse_yaml_findings(findings)
        } else {
            Vec::new()
        };

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary,
            details,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::experts::ScoreItem;

    // ─── YAML parsing fallback patterns ──────────
    // These test the same serde_yaml_ng::Value accessor chains used by
    // ArchitectureLead::evaluate and CodeQuality::evaluate.

    fn parse_score(yaml: &str) -> u8 {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        value["score"].as_u64().unwrap_or(70).min(100) as u8
    }

    fn parse_summary(yaml: &str, fallback: &str) -> String {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        value["summary"].as_str().unwrap_or(fallback).to_string()
    }

    fn parse_risk_areas(yaml: &str) -> Vec<String> {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        value["risk_areas"]
            .as_sequence()
            .map(|seq| seq.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    fn parse_findings(yaml: &str) -> Vec<ScoreItem> {
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        let mut details = Vec::new();
        if let Some(findings) = value["findings"].as_sequence() {
            for f in findings {
                details.push(ScoreItem {
                    severity: f["severity"].as_str().unwrap_or("medium").to_string(),
                    message: f["message"].as_str().unwrap_or("").to_string(),
                    file: f["file"].as_str().map(String::from),
                    ..Default::default()
                });
            }
        }
        details
    }

    #[test]
    fn test_yaml_score_parsed() {
        assert_eq!(parse_score("score: 85"), 85);
    }

    #[test]
    fn test_yaml_score_missing_fallback() {
        assert_eq!(parse_score("summary: \"No score\""), 70);
    }

    #[test]
    fn test_yaml_score_clamped_max() {
        assert_eq!(parse_score("score: 150"), 100);
    }

    #[test]
    fn test_yaml_score_zero() {
        assert_eq!(parse_score("score: 0"), 0);
    }

    #[test]
    fn test_yaml_score_non_numeric() {
        assert_eq!(parse_score("score: \"abc\""), 70);
    }

    #[test]
    fn test_yaml_summary_parsed() {
        assert_eq!(
            parse_summary("summary: \"Custom arch\"", "Architecture assessment completed"),
            "Custom arch"
        );
    }

    #[test]
    fn test_yaml_summary_missing_arch_fallback() {
        assert_eq!(
            parse_summary("score: 80", "Architecture assessment completed"),
            "Architecture assessment completed"
        );
    }

    #[test]
    fn test_yaml_summary_missing_quality_fallback() {
        assert_eq!(
            parse_summary("score: 80", "Code quality assessment completed"),
            "Code quality assessment completed"
        );
    }

    #[test]
    fn test_yaml_risk_areas_parsed() {
        let areas = parse_risk_areas("risk_areas:\n  - \"Tight coupling\"\n  - \"Missing errors\"");
        assert_eq!(areas.len(), 2);
        assert!(areas[0].contains("Tight coupling"));
    }

    #[test]
    fn test_yaml_risk_areas_missing() {
        let areas = parse_risk_areas("score: 90");
        assert!(areas.is_empty());
    }

    #[test]
    fn test_yaml_guidance_fallback() {
        let yaml = "score: 80";
        let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap_or(serde_yaml_ng::Value::Null);
        let guidance = value["guidance"].as_str().unwrap_or("").to_string();
        assert_eq!(guidance, "");
    }

    #[test]
    fn test_yaml_findings_parsed() {
        let yaml = r#"
findings:
  - severity: "high"
    message: "Unsafe code"
    file: "src/main.rs"
  - severity: "low"
    message: "Missing docs"
"#;
        let details = parse_findings(yaml);
        assert_eq!(details.len(), 2);
        assert_eq!(details[0].severity, "high");
        assert_eq!(details[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(details[1].file, None);
    }

    #[test]
    fn test_yaml_findings_missing() {
        let details = parse_findings("score: 95");
        assert!(details.is_empty());
    }

    #[test]
    fn test_yaml_findings_missing_fields() {
        let yaml = "findings:\n  - severity: \"high\"\n";
        let details = parse_findings(yaml);
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].severity, "high");
        assert_eq!(details[0].message, "");
    }

    #[test]
    fn test_yaml_null_value() {
        let value = serde_yaml_ng::Value::Null;
        assert_eq!(value["score"].as_u64().unwrap_or(70).min(100) as u8, 70);
        assert_eq!(
            value["summary"].as_str().unwrap_or("Architecture assessment completed"),
            "Architecture assessment completed"
        );
    }

    #[test]
    fn test_yaml_empty_document() {
        assert_eq!(parse_score(""), 70);
    }

    // ─── parse_expert_yaml fallback / robustness ──────────
    // These exercise the shared entry point used by both LLM experts: on any
    // failure it returns `Null` (so score falls back to 70 and findings to
    // empty) and logs a distinct warn — never panics.

    #[test]
    fn test_parse_expert_yaml_plain_yaml() {
        let v = parse_expert_yaml("code_quality", "score: 85\nsummary: \"ok\"", CODE_QUALITY_KEYS);
        assert_eq!(v["score"].as_u64(), Some(85));
    }

    #[test]
    fn test_parse_expert_yaml_fenced_yaml_recovers() {
        let raw = "Here is my assessment:\n```yaml\nscore: 88\nfindings: []\n```\n";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert_eq!(v["score"].as_u64(), Some(88));
    }

    #[test]
    fn test_parse_expert_yaml_architecture_keys_accepted() {
        let raw = "summary: \"ok\"\nscore: 72\nrisk_areas: []\n";
        let v = parse_expert_yaml("architecture_lead", raw, ARCHITECTURE_LEAD_KEYS);
        assert_eq!(v["score"].as_u64(), Some(72));
    }

    #[test]
    fn test_parse_expert_yaml_empty_response_falls_back() {
        // Observed failure mode: reasoning models exhaust their max_tokens
        // budget and return zero bytes. Must fall back, not panic.
        let v = parse_expert_yaml("code_quality", "", CODE_QUALITY_KEYS);
        assert!(v.is_null());
        let score = v["score"].as_u64().unwrap_or(70).min(100) as u8;
        assert_eq!(score, 70);
    }

    #[test]
    fn test_parse_expert_yaml_malformed_yaml_falls_back_without_panic() {
        let raw = "score: [unclosed\n  findings:\n    - severity: \"high\"\n";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert!(v.is_null());
        // Fallback score and empty findings, mirroring the evaluate path.
        let score = v["score"].as_u64().unwrap_or(70).min(100) as u8;
        assert_eq!(score, 70);
        let details = if let Some(findings) = v["findings"].as_sequence() {
            crate::repo::experts::parse_yaml_findings(findings)
        } else {
            Vec::new()
        };
        assert!(details.is_empty());
    }

    #[test]
    fn test_parse_expert_yaml_schema_drift_falls_back() {
        // Model returned valid YAML with the wrong shape (no expected keys):
        // must fall back rather than silently report 70/0 as model output.
        let raw = "verdicts: []\n";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert!(v.is_null());
    }

    #[test]
    fn test_parse_expert_yaml_prose_without_fence_falls_back() {
        let raw = "The module looks fine overall. No issues worth flagging.";
        let v = parse_expert_yaml("code_quality", raw, CODE_QUALITY_KEYS);
        assert!(v.is_null());
    }

    #[test]
    fn test_truncate_excerpt_bounds_length_and_collapses_newlines() {
        let long = "x".repeat(500);
        let ex = truncate_excerpt(&long);
        assert!(ex.len() <= EXCERPT_MAX_BYTES);
        assert!(!ex.is_empty());
        assert!(!ex.contains('\n'));
        assert_eq!(truncate_excerpt("line1\nline2"), "line1\\nline2");
        assert_eq!(truncate_excerpt(""), "");
    }

    #[test]
    fn test_render_code_quality_system_substitutes_placeholders() {
        let rendered = render_code_quality_system("auth", "Rust", "use snake_case names", "prefer Result");
        assert!(rendered.contains("**auth**"));
        assert!(rendered.contains("Primary language: Rust"));
        assert!(rendered.contains("use snake_case names"));
        assert!(rendered.contains("prefer Result"));
    }

    #[test]
    fn test_render_code_quality_system_leaves_no_placeholder_residue() {
        // Regression guard: every `{{ ... }}` marker in the template must be
        // substituted — a literal marker reaching the LLM means the replace
        // targets drifted from the template again.
        let rendered = render_code_quality_system("m", "l", "n", "e");
        assert!(
            !rendered.contains("{{"),
            "unsubstituted placeholder in prompt:\n{rendered}"
        );
        assert!(
            !rendered.contains("}}"),
            "unsubstituted placeholder in prompt:\n{rendered}"
        );
    }

    #[test]
    fn test_architecture_lead_metadata() {
        let expert = ArchitectureLead;
        assert_eq!(expert.weight(), 15);
        assert_eq!(expert.name(), "architecture_lead");
        assert!(expert.requires_llm());
    }

    #[test]
    fn test_code_quality_metadata() {
        let expert = CodeQuality;
        assert_eq!(expert.weight(), 10);
        assert_eq!(expert.name(), "code_quality");
        assert!(expert.requires_llm());
    }
}
