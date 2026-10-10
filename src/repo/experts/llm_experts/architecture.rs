//! Architecture Lead expert: Pass 1 repository structure analysis.

use anyhow::Result;
use async_trait::async_trait;

use super::scoring::{
    append_facts_block, call_scoring, parse_expert_yaml, scoring_sample_count, ARCHITECTURE_LEAD_KEYS,
};
use crate::llm::client::LLMClient;
use crate::prompt::templates;
use crate::repo::experts::{ExpertScore, RepoContext, RepoExpert};

pub(crate) fn architecture_user_prompt(ctx: &RepoContext) -> String {
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

pub struct ArchitectureLead;

#[async_trait]
impl RepoExpert for ArchitectureLead {
    fn name(&self) -> &str {
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

        let value = parse_expert_yaml("architecture", &call.content, ARCHITECTURE_LEAD_KEYS);
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
            .unwrap_or("Architecture assessment completed")
            .to_string();
        let risk_items: Vec<serde_yaml_ng::Value> = value["risk_areas"].as_sequence().cloned().unwrap_or_default();
        let details = crate::repo::experts::parse_yaml_findings(&risk_items);

        // M18-W2: the by-expert sample-fallback rate (`samples_failed /
        // samples_attempted`) only has a denominator when sampling was
        // actually active. The single-shot path (`score_samples = 1`, the
        // default) leaves both fields at `None` so the JSON omits them —
        // writing `Some(1) / Some(0)` would publish a misleading 0% rate
        // for a fallback expert (and was the P2-2 doc/code contradiction
        // surfaced in M26 r1 review). Mirrors the `samples` field's own
        // serialisation rule.
        let samples = call.samples;
        let samples_attempted = if samples.is_some() {
            Some(call.samples_attempted)
        } else {
            None
        };
        let samples_failed = if samples.is_some() {
            Some(call.samples_failed)
        } else {
            None
        };

        Ok(ExpertScore {
            expert_name: self.name().to_string(),
            weight: self.weight(),
            score,
            summary,
            details,
            fallback,
            evaluated_loc: Some(ctx.stats.total_loc as u64),
            samples,
            samples_attempted,
            samples_failed,
            // M38: see the matching comment in `code_quality.rs` — same
            // plumb for ArchitectureLead. The single-shot path is the
            // only one that ever carries a code; multi-sample runs leave
            // it at `None` because the per-sample drop counter is the
            // signal that surfaces, not a clarifying retry.
            reason_code: call.reason_code,
        })
    }
}
