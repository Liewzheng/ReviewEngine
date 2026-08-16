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

/// Temperature for repo-review scoring calls (architecture / code_quality).
/// Deterministic-first: scoring is not generation, so it runs cold at 0.
/// Hard cap [`SCORING_TEMPERATURE_MAX`] — any future raise beyond it
/// reintroduces the score drift the rubric anchoring removed.
const SCORING_TEMPERATURE: f32 = 0.0;

/// Upper bound ever allowed for a scoring-call temperature.
const SCORING_TEMPERATURE_MAX: f32 = 0.2;

/// Per-call temperature override for scoring calls: clone the fallback
/// chain with the scoring temperature applied. The caller's configs — and
/// the global default (0.3) — are untouched; only these scoring calls run
/// cold. Applied once in [`call_scoring`], so both experts and every
/// concurrent sample inherit it.
fn scoring_configs(configs: &[crate::models::LLMConfig]) -> Vec<crate::models::LLMConfig> {
    configs
        .iter()
        .cloned()
        .map(|mut c| {
            c.temperature = SCORING_TEMPERATURE.clamp(0.0, SCORING_TEMPERATURE_MAX);
            c
        })
        .collect()
}

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

// ─── Score sampling ──────────────────────────

/// Median of a sample set: the middle score after sorting, or the
/// round-half-up mean of the two middle scores for an even count. Returns
/// `None` for an empty set (all samples failed).
fn median_score(samples: &[u8]) -> Option<u8> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Some(sorted[mid])
    } else {
        Some(((u16::from(sorted[mid - 1]) + u16::from(sorted[mid])).div_ceil(2)) as u8)
    }
}

/// Effective sample count from config. `0` is meaningless and treated as
/// `1` — the single-call status quo.
fn scoring_sample_count(config: Option<&crate::models::AppConfig>) -> usize {
    config.map(|c| c.scoring.score_samples).unwrap_or(1).max(1)
}

/// Outcome of the (possibly sampled) scoring call shared by both LLM
/// experts.
#[derive(Debug)]
struct ScoringCall {
    /// Response content to parse summary/findings from. With sampling, this
    /// is the lower-middle sample's response (by score ordering); without,
    /// the single call's response. Either way it is one real model
    /// response, so summary/findings stay coherent with a genuine output.
    content: String,
    /// `Some(median)` when sampling was active — the score to report.
    /// `None` means "parse the score from `content`" (single-call path).
    median: Option<u8>,
    /// Raw successful sample scores in completion order; `Some` only when
    /// sampling was active.
    samples: Option<Vec<u8>>,
}

/// Run an expert's scoring call, optionally sampling `n` times concurrently
/// and reporting the median (`scoring.score_samples`).
///
/// Sampling semantics:
/// - all N calls run CONCURRENTLY (never serially);
/// - a sample counts only when the call succeeds AND parses to a genuine
///   `score` — empty / unparseable / schema-drifted responses are dropped
///   (each drop is warn-logged);
/// - if every sample fails this returns `Err`, which the orchestration
///   layer turns into the explicit flagged fallback score — exactly the
///   same landing path as a failed single call;
/// - with an even number of surviving samples the reported median is the
///   round-half-up mean of the two middle scores, while the content comes
///   from the lower-middle sample (deterministic).
async fn call_scoring(
    llm: &LLMClient,
    configs: &[crate::models::LLMConfig],
    system: &str,
    user: &str,
    n: usize,
    expert: &str,
    expected_keys: &[&str],
) -> Result<ScoringCall> {
    // Per-call temperature override: scoring runs cold. Cloning keeps the
    // caller's fallback chain — and the global default — untouched.
    let overridden = scoring_configs(configs);
    let configs = overridden.as_slice();

    if n <= 1 {
        let response = llm.complete_with_fallback(configs, system, user).await?;
        return Ok(ScoringCall {
            content: response.content,
            median: None,
            samples: None,
        });
    }

    let calls: Vec<_> = (0..n)
        .map(|_| llm.complete_with_fallback(configs, system, user))
        .collect();
    let results = futures::future::join_all(calls).await;

    let mut scored: Vec<(u8, String)> = Vec::new();
    for result in results {
        match result {
            Ok(response) => {
                let value = parse_expert_yaml(expert, &response.content, expected_keys);
                match value["score"].as_u64() {
                    Some(raw) => scored.push((raw.min(100) as u8, response.content)),
                    None => tracing::warn!(
                        expert_name = expert,
                        "scoring sample dropped: no genuine score in response"
                    ),
                }
            }
            Err(e) => tracing::warn!(
                expert_name = expert,
                error = %e,
                "scoring sample dropped: call failed"
            ),
        }
    }
    if scored.is_empty() {
        anyhow::bail!("all {n} scoring samples failed for expert '{expert}'");
    }

    let samples: Vec<u8> = scored.iter().map(|(s, _)| *s).collect();
    // `samples` mirrors `scored`, which is non-empty past the bail above, so
    // a median always exists; the error arm is defensive, not reachable.
    let median = match median_score(&samples) {
        Some(median) => median,
        None => anyhow::bail!("no usable scoring samples for expert '{expert}'"),
    };
    // Deterministic representative: lower-middle by score.
    scored.sort_by_key(|(s, _)| *s);
    let representative = scored[(scored.len() - 1) / 2].1.clone();
    Ok(ScoringCall {
        content: representative,
        median: Some(median),
        samples: Some(samples),
    })
}

/// Append the deterministic repo-facts block to a user prompt. Shared by
/// both LLM experts; no-op on the local-only path (`facts_block: None`),
/// where no LLM prompt is built anyway. The block ends with a trailing
/// newline (see [`crate::repo::experts::facts::RepoFacts::to_prompt_block`]).
fn append_facts_block(user: &mut String, facts_block: Option<&str>) {
    if let Some(facts) = facts_block {
        user.push_str("\n\n## Repository Facts (deterministic static analysis)\n");
        user.push_str(facts);
    }
}

/// Build the Architecture Lead user prompt: repo overview, language
/// breakdown, file tree, plus the deterministic facts block when present.
/// Extracted from `evaluate` so the prompt shape is testable without an LLM.
fn architecture_user_prompt(ctx: &RepoContext) -> String {
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

    let mut user = format!(
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
    append_facts_block(&mut user, ctx.facts_block.as_deref());
    user
}

/// Architecture Lead: Pass 1 expert that examines the file tree and produces a
/// high-level assessment of the repository structure and risks.
pub struct ArchitectureLead;

#[async_trait]
impl RepoExpert for ArchitectureLead {
    fn name(&self) -> &str {
        // Canonical area name shared with `DEFAULT_WEIGHTS` and the report
        // pipeline (`convert_scores` extracts the lead summary under this
        // exact key). Naming this expert anything else silently drops the
        // lead summary from the report.
        "architecture"
    }
    fn weight(&self) -> u8 {
        15
    }
    fn requires_llm(&self) -> bool {
        true
    }

    async fn evaluate(&self, ctx: &RepoContext, llm: Option<&LLMClient>) -> Result<ExpertScore> {
        let llm = llm.ok_or_else(|| anyhow::anyhow!("ArchitectureLead requires LLM"))?;

        let system = templates::ARCHITECTURE_LEAD_SYSTEM_TEMPLATE;
        let user = architecture_user_prompt(ctx);

        let n = scoring_sample_count(ctx.config.as_deref());
        let call = call_scoring(
            llm,
            &ctx.llm_configs,
            system,
            &user,
            n,
            "architecture",
            ARCHITECTURE_LEAD_KEYS,
        )
        .await?;

        // Parse YAML response. A missing score (unparseable / drifted /
        // empty response) means the 70 below is a synthetic fallback —
        // flag it so reports do not present it as a model assessment. Under
        // sampling the median is always a genuine model score.
        let value = parse_expert_yaml("architecture", &call.content, ARCHITECTURE_LEAD_KEYS);
        let score_raw = value["score"].as_u64();
        let (score, fallback) = match call.median {
            Some(median) => (median, false),
            None => match score_raw {
                Some(raw) => (raw.min(100) as u8, false),
                None => (super::LLM_FALLBACK_SCORE, true),
            },
        };
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
            fallback,
            evaluated_loc: Some(ctx.stats.total_loc as u64),
            samples: call.samples,
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

/// Build the CodeQuality user prompt: module header, concatenated file
/// contents, plus the deterministic facts block when present. Extracted for
/// testability (no LLM needed).
fn code_quality_user_prompt(ctx: &RepoContext, module_name: &str, first_lang: &str, source_files: &[String]) -> String {
    let mut user = format!(
        "## Module: {module} ({lang})\n\
         Files in this module: {count}\n\n\
         ## Code\n\
         {code}",
        module = module_name,
        lang = first_lang,
        count = ctx.entries.len(),
        code = source_files.join("\n\n---\n\n"),
    );
    append_facts_block(&mut user, ctx.facts_block.as_deref());
    user
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
        let user = code_quality_user_prompt(ctx, module_name, first_lang, &source_files);

        let n = scoring_sample_count(ctx.config.as_deref());
        let call = call_scoring(
            llm,
            &ctx.llm_configs,
            &system,
            &user,
            n,
            "code_quality",
            CODE_QUALITY_KEYS,
        )
        .await?;

        let value = parse_expert_yaml("code_quality", &call.content, CODE_QUALITY_KEYS);
        let score_raw = value["score"].as_u64();
        let (score, fallback) = match call.median {
            Some(median) => (median, false),
            None => match score_raw {
                Some(raw) => (raw.min(100) as u8, false),
                None => (super::LLM_FALLBACK_SCORE, true),
            },
        };
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
            fallback,
            // Real LOC of this chunk, so the aggregator can LOC-weight the
            // merge with truth instead of the findings-count heuristic.
            evaluated_loc: Some(ctx.entries.iter().map(|e| e.loc as u64).sum()),
            samples: call.samples,
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
        // Canonical area name: `convert_scores` keys the lead summary off
        // "architecture" and `DEFAULT_WEIGHTS` lists the same name.
        assert_eq!(expert.name(), "architecture");
        assert!(expert.requires_llm());
    }

    #[test]
    fn test_code_quality_metadata() {
        let expert = CodeQuality;
        assert_eq!(expert.weight(), 10);
        assert_eq!(expert.name(), "code_quality");
        assert!(expert.requires_llm());
    }

    // ─── facts-block injection ─────

    fn prompt_ctx(facts_block: Option<String>) -> RepoContext {
        RepoContext {
            entries: vec![],
            stats: crate::repo::RepoStats::default(),
            llm_configs: vec![],
            config: None,
            facts_block,
        }
    }

    #[test]
    fn test_architecture_prompt_injects_facts_block() {
        let ctx = prompt_ctx(Some("repo_facts:\n  test_files: 3\n".to_string()));
        let prompt = architecture_user_prompt(&ctx);
        assert!(prompt.contains("## Repository Facts (deterministic static analysis)"));
        assert!(prompt.contains("repo_facts:"));
        assert!(prompt.contains("test_files: 3"));
    }

    #[test]
    fn test_code_quality_prompt_injects_facts_block() {
        let ctx = prompt_ctx(Some(
            "repo_facts:\n  ci_configs:\n    - \".gitlab-ci.yml\"\n".to_string(),
        ));
        let prompt = code_quality_user_prompt(&ctx, "auth", "Rust", &["// code".to_string()]);
        assert!(prompt.contains("## Repository Facts (deterministic static analysis)"));
        assert!(prompt.contains("repo_facts:"));
        assert!(prompt.contains(".gitlab-ci.yml"));
    }

    #[test]
    fn test_prompts_omit_facts_block_when_absent() {
        // Local-only path (`facts_block: None`): no residue in either prompt.
        let ctx = prompt_ctx(None);
        assert!(!architecture_user_prompt(&ctx).contains("repo_facts"));
        assert!(!code_quality_user_prompt(&ctx, "m", "Rust", &[]).contains("repo_facts"));
    }

    #[test]
    fn test_fully_annotated_python_facts_reach_prompt_verbatim() {
        // The anti-"Missing type hints" chain: static full-annotation
        // coverage of 1.00 must be visible verbatim in the scored prompt.
        let dir = tempfile::tempdir().expect("tempdir");
        let py = dir.path().join("a.py");
        std::fs::write(&py, "def f(x: int) -> int:\n    return x\n").expect("write fixture");
        let entries = vec![crate::repo::FileEntry {
            path: py.to_string_lossy().into_owned(),
            language: "Python".to_string(),
            loc: 2,
            is_binary: false,
            is_generated: false,
        }];
        let block = crate::repo::experts::facts::compute(&entries).to_prompt_block();
        assert!(block.contains("full_param_annotation_coverage: 1.00"));

        let ctx = prompt_ctx(Some(block));
        let prompt = code_quality_user_prompt(&ctx, "m", "Python", &[]);
        assert!(prompt.contains("full_param_annotation_coverage: 1.00"));
        let arch_prompt = architecture_user_prompt(&ctx);
        assert!(arch_prompt.contains("full_param_annotation_coverage: 1.00"));
    }

    // ─── score sampling ─────

    #[test]
    fn test_median_score_odd_even_empty() {
        assert_eq!(median_score(&[]), None);
        assert_eq!(median_score(&[80]), Some(80));
        assert_eq!(median_score(&[70, 90, 80]), Some(80));
        // Even count: round-half-up mean of the two middle scores.
        assert_eq!(median_score(&[70, 80, 80, 91]), Some(80));
        assert_eq!(median_score(&[70, 71]), Some(71)); // 70.5 rounds up
        assert_eq!(median_score(&[70, 72]), Some(71));
        // Input order does not matter.
        assert_eq!(median_score(&[95, 40, 60]), Some(60));
    }

    #[test]
    fn test_scoring_sample_count_defaults_and_guards() {
        assert_eq!(scoring_sample_count(None), 1);
        let config: crate::models::AppConfig = toml::from_str("").unwrap();
        assert_eq!(scoring_sample_count(Some(&config)), 1);
        // 0 is meaningless: treated as the single-call status quo.
        let config: crate::models::AppConfig = toml::from_str("[scoring]\nscore_samples = 0\n").unwrap();
        assert_eq!(scoring_sample_count(Some(&config)), 1);
        let config: crate::models::AppConfig = toml::from_str("[scoring]\nscore_samples = 5\n").unwrap();
        assert_eq!(scoring_sample_count(Some(&config)), 5);
    }

    use crate::llm::provider::{CompletionParams, CompletionResult, LLMProvider, ProviderRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Mock provider serving scripted response bodies in poll order (the
    /// last body repeats). Tracks peak in-flight concurrency so tests can
    /// prove sampling is concurrent, and records the temperature of every
    /// request so tests can prove the scoring override reaches the wire.
    struct ScriptedProvider {
        bodies: Vec<String>,
        calls: AtomicUsize,
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        temperatures: Arc<std::sync::Mutex<Vec<f32>>>,
    }

    #[async_trait::async_trait]
    impl LLMProvider for ScriptedProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(&self, _params: &CompletionParams) -> Result<CompletionResult> {
            self.temperatures.lock().unwrap().push(_params.temperature);
            // Assign the body at first poll (join_all polls in input order,
            // so this is deterministic) BEFORE parking on the timer.
            let i = self.calls.fetch_add(1, Ordering::SeqCst).min(self.bodies.len() - 1);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            // Park so concurrent samples overlap; under `start_paused` the
            // timer auto-advances once every sample is in flight.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(CompletionResult {
                content: self.bodies[i].clone(),
                total_tokens: 1,
                model: "mock".to_string(),
            })
        }
    }

    fn mock_client(bodies: Vec<String>) -> (LLMClient, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<f32>>>) {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let temperatures = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ScriptedProvider {
            bodies,
            calls: AtomicUsize::new(0),
            in_flight: in_flight.clone(),
            peak: peak.clone(),
            temperatures: temperatures.clone(),
        }));
        (LLMClient::new().with_registry(Arc::new(registry)), peak, temperatures)
    }

    fn mock_configs() -> Vec<crate::models::LLMConfig> {
        vec![crate::models::LLMConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            api_key: "k".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }]
    }

    #[tokio::test(start_paused = true)]
    async fn test_sampling_runs_concurrently_and_reports_median() {
        let bodies = vec![
            "score: 70\nsummary: \"a\"\nfindings: []".to_string(),
            "score: 90\nsummary: \"b\"\nfindings: []".to_string(),
            "score: 80\nsummary: \"c\"\nfindings: []".to_string(),
        ];
        let (client, peak, _temps) = mock_client(bodies);
        let call = call_scoring(
            &client,
            &mock_configs(),
            "sys",
            "user",
            3,
            "code_quality",
            CODE_QUALITY_KEYS,
        )
        .await
        .unwrap();
        // join_all preserves input order; scores land in poll order.
        assert_eq!(call.samples, Some(vec![70, 90, 80]));
        assert_eq!(call.median, Some(80));
        assert_eq!(peak.load(Ordering::SeqCst), 3, "samples must overlap, not run serially");
        // Representative content is the lower-middle sample's real response.
        assert!(call.content.contains("summary: \"c\""));
    }

    #[tokio::test(start_paused = true)]
    async fn test_sampling_drops_unparseable_and_scoreless_samples() {
        let bodies = vec![
            "score: 90\nsummary: \"good\"\nfindings: []".to_string(),
            "this is not yaml at all".to_string(), // unparseable → dropped
            "summary: \"no score here\"\nfindings: []".to_string(), // schema-conforming but no score → dropped
            "score: 70\nsummary: \"also good\"\nfindings: []".to_string(),
        ];
        let (client, _peak, _temps) = mock_client(bodies);
        let call = call_scoring(
            &client,
            &mock_configs(),
            "sys",
            "user",
            4,
            "code_quality",
            CODE_QUALITY_KEYS,
        )
        .await
        .unwrap();
        assert_eq!(call.samples, Some(vec![90, 70]));
        assert_eq!(call.median, Some(80));
    }

    #[tokio::test(start_paused = true)]
    async fn test_sampling_all_failed_is_error_for_fallback_path() {
        // No registry → direct HTTP to an unreachable endpoint (connection
        // refused, offline, fail-fast). All samples fail → Err, which the
        // orchestration layer turns into the explicit flagged fallback.
        let client = LLMClient::new();
        let configs = vec![crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: "unreachable".to_string(),
            api_key: "sk-test".to_string(),
            api_base: "http://127.0.0.1:1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }];
        let err = call_scoring(
            &client,
            &configs,
            "sys",
            "user",
            3,
            "architecture",
            ARCHITECTURE_LEAD_KEYS,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("all 3 scoring samples failed"));
    }

    // ─── scoring temperature override ─────

    #[test]
    fn test_scoring_configs_runs_cold_and_preserves_input() {
        let mk = |temperature: f32| crate::models::LLMConfig {
            provider: "p".to_string(),
            model: "m".to_string(),
            api_key: "k".to_string(),
            api_base: "https://example.com".to_string(),
            max_tokens: 4096,
            temperature,
            disable_thinking: None,
        };
        let configs = vec![mk(0.3), mk(0.9)];
        let overridden = scoring_configs(&configs);
        assert!(overridden.iter().all(|c| c.temperature == 0.0));
        // The caller's chain keeps its temperatures — the override is
        // per-call, not a mutation of shared state.
        assert_eq!(configs[0].temperature, 0.3);
        assert_eq!(configs[1].temperature, 0.9);
        // The cap invariant: scoring temperature never exceeds 0.2.
        assert!(SCORING_TEMPERATURE <= SCORING_TEMPERATURE_MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn test_scoring_calls_reach_provider_at_zero_temperature() {
        let bodies = vec!["score: 80\nsummary: \"a\"\nfindings: []".to_string()];
        let (client, _peak, temps) = mock_client(bodies);
        let mut configs = mock_configs();
        configs[0].temperature = 0.9; // loud non-zero input to prove the override
        let call = call_scoring(&client, &configs, "sys", "user", 3, "code_quality", CODE_QUALITY_KEYS)
            .await
            .unwrap();
        assert_eq!(call.median, Some(80));
        let temps = temps.lock().unwrap();
        assert_eq!(temps.len(), 3, "every sample is its own provider call");
        assert!(temps.iter().all(|&t| t == 0.0), "scoring must run cold: {temps:?}");
        // Global default untouched: the original config keeps 0.9.
        assert_eq!(configs[0].temperature, 0.9);
    }
}
