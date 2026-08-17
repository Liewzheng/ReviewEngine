//! CodeQuality expert: Pass 2 per-chunk code quality analysis.

use anyhow::Result;
use async_trait::async_trait;

use super::scoring::{append_facts_block, call_scoring, parse_expert_yaml, scoring_sample_count, CODE_QUALITY_KEYS};
use crate::llm::client::LLMClient;
use crate::prompt::templates;
use crate::repo::experts::{ExpertScore, RepoContext, RepoExpert};

pub(crate) fn render_code_quality_system(module: &str, lang: &str, naming_hint: &str, error_hint: &str) -> String {
    templates::CODE_QUALITY_SYSTEM_TEMPLATE
        .replace("{{ module }}", module)
        .replace("{{ lang }}", lang)
        .replace("{{ naming_hint }}", naming_hint)
        .replace("{{ error_hint }}", error_hint)
}

pub(crate) fn code_quality_user_prompt(
    ctx: &RepoContext,
    module_name: &str,
    first_lang: &str,
    source_files: &[String],
) -> String {
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

        let first_file = ctx.entries.first();
        let module_name = first_file
            .and_then(|e| {
                let p = std::path::Path::new(&e.path);
                p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str())
            })
            .unwrap_or("unknown");

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
                None => (crate::repo::experts::LLM_FALLBACK_SCORE, true),
            },
        };
        let summary = value["summary"]
            .as_str()
            .unwrap_or("Code quality assessment completed")
            .to_string();

        let details = if let Some(findings) = value["findings"].as_sequence() {
            crate::repo::experts::parse_yaml_findings(findings)
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
            evaluated_loc: Some(ctx.entries.iter().map(|e| e.loc as u64).sum()),
            samples: call.samples,
        })
    }
}
