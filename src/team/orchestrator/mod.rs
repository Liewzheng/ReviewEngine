mod pipeline;
#[cfg(test)]
mod tests;
mod validation;

pub(crate) use pipeline::run_experts_inner;

use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Instant;

use crate::actions::registry::CommandRegistry;
use crate::actions::registry::ExpertSelection;
use crate::llm::client::LLMClient;
use crate::llm::select_llm_config;
use crate::models::*;
use crate::progress::{ProgressMap, ReviewProgress, StageWeight};
use crate::prompt::PromptEngine;

use crate::team::lead_consolidator::ConsolidatedReport;
use crate::team::verifier::DroppedFinding;

use super::{TeamOrchestrator, TeamReport};

/// Default implementation of [`TeamOrchestrator`].
///
/// Runs all selected experts in parallel with concurrency limited by
/// `max_concurrent_llm_calls` via a `tokio::sync::Semaphore`,
/// then optionally runs the aggregator expert to consolidate results.
pub struct DefaultOrchestrator {
    pub max_team_size: usize,
    pub max_concurrent_llm_calls: usize,
    pub progress_map: Option<ProgressMap>,
    pub review_id: String,
}

impl DefaultOrchestrator {
    pub fn new() -> Self {
        Self {
            max_team_size: 6,
            max_concurrent_llm_calls: 6,
            progress_map: None,
            review_id: String::new(),
        }
    }
}

impl Default for DefaultOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

fn select_experts_for_command<'a>(
    command: &str,
    experts: &'a [ExpertDef],
    registry: &HashMap<String, bool>,
) -> ExpertSelection<'a> {
    let cmd_registry = CommandRegistry::new(registry.clone());
    cmd_registry.select_experts_for_command(command, experts)
}

#[async_trait]
impl TeamOrchestrator for DefaultOrchestrator {
    fn select_experts<'a>(
        &self,
        command: &str,
        experts: &'a [ExpertDef],
        registry: &HashMap<String, bool>,
    ) -> Vec<&'a ExpertDef> {
        match select_experts_for_command(command, experts, registry) {
            ExpertSelection::Selected(v) => v,
            _ => vec![],
        }
    }

    async fn run(
        &self,
        command: &Command,
        input: &ReviewInput,
        config: &AppConfig,
        llm_configs: &[LLMConfig],
    ) -> anyhow::Result<TeamReport> {
        let start = Instant::now();
        let request_id = uuid::Uuid::new_v4().to_string();

        // Phase 1: Briefing - resolve diff from input
        let (diff_raw, mr_info) = resolve_input(command, input).await?;

        // Initialize progress
        if let Some(ref map) = self.progress_map {
            let stages = if diff_raw.len() > crate::diff::large_pr::pre_assess_bytes(&config.diff) {
                StageWeight::large_pr()
            } else {
                StageWeight::small_pr()
            };
            let progress = ReviewProgress::new(self.review_id.clone(), &stages);
            if let Ok(mut g) = map.write() {
                g.insert(self.review_id.clone(), progress);
            }
        }

        let experts = config.build_expert_defs();
        let registry = &config.commands;
        let cmd_str = format!("{:?}", command).to_lowercase();
        let selected = match select_experts_for_command(&cmd_str, &experts, registry) {
            ExpertSelection::Selected(v) => v,
            ExpertSelection::CommandDisabled => {
                anyhow::bail!(
                    "Command '{}' is disabled in the config. Set [commands]\n{} = true to enable it, or run review-engine init.",
                    cmd_str, cmd_str
                );
            }
            ExpertSelection::NoMatchingExperts => {
                anyhow::bail!(
                    "No experts are configured for command '{}'. Check each expert's 'commands' list.",
                    cmd_str
                );
            }
        };

        // Enforce max_team_size
        let max_size = config.max_team_size.unwrap_or(self.max_team_size);
        if max_size == 0 {
            anyhow::bail!(
                "max_team_size is 0, no experts can be selected. Set max_team_size to at least 1 in your config."
            );
        }
        let selected: Vec<&ExpertDef> = selected.into_iter().take(max_size).collect();

        let selected_defs: Vec<ExpertDef> = selected.iter().map(|e| (*e).clone()).collect();

        // Mark parse stage complete
        if let Some(ref map) = self.progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(&self.review_id) {
                    progress.complete_stage("parse");
                }
            }
        }

        let (base_ref, head_ref) = match input {
            ReviewInput::LocalRepo { base_ref, head_ref, .. } => (base_ref.clone(), head_ref.clone()),
            _ => (None, None),
        };

        let (reports, metrics, total_tokens, errors, _global_context, dropped_findings, consolidated) =
            run_experts_inner(
                &selected_defs,
                &mr_info,
                &diff_raw,
                llm_configs,
                config,
                self.progress_map.as_ref(),
                &self.review_id,
                base_ref.as_deref(),
                head_ref.as_deref(),
                None,
            )
            .await?;

        // Optional aggregator expert (LLM consolidation) on top of the
        // deterministic lead consolidation already computed in `run_experts_inner`.
        let aggregated = if config.report.aggregated && experts.iter().any(|e| e.name == "aggregator") {
            if let Some(aggregator) = experts.iter().find(|e| e.name == "aggregator") {
                let prompt_engine = PromptEngine::new();
                let llm_client = LLMClient::new();
                let (system, user) =
                    prompt_engine.build_aggregator_prompt(&reports, &mr_info, _global_context.as_ref(), "en")?;
                let llm_config = select_llm_config(aggregator, llm_configs);
                let result = llm_client.complete_with_fallback(&llm_config, &system, &user).await?;
                let agg_report = crate::output::parser::parse_aggregator_response(&result.content)?;
                Some(agg_report)
            } else {
                None
            }
        } else {
            None
        };

        // Mark report stage complete and overall completed
        if let Some(ref map) = self.progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(&self.review_id) {
                    progress.complete_stage("aggregate");
                    progress.complete_stage("report");
                    progress.mark_completed();
                }
            }
        }

        let elapsed = start.elapsed();

        crate::metrics::REVIEW_DURATION.observe(elapsed.as_secs_f64());
        crate::metrics::REVIEW_REQUESTS.inc();

        Ok(TeamReport {
            request_id,
            team_size: selected_defs.len(),
            total_duration_ms: elapsed.as_millis() as u64,
            total_tokens,
            reports,
            aggregated,
            errors,
            metrics,
            consolidated: Some(consolidated),
            dropped_findings,
        })
    }
}

/// Run all applicable experts against the given MR diff.
///
/// Initialises progress tracking (auto-detecting small vs. large PR
/// stage weights), then runs the full pipeline: diff parsing, filtering,
/// chunking, lead overview (Pass 1), rate-limited parallel expert
/// dispatch, and consolidation.
///
/// Returns per-expert reports, an optional global review context, any
/// findings dropped by the optional verification pass, and the lead
/// consolidation summary (always computed — pure post-processing).
pub async fn run_experts(
    experts: &[ExpertDef],
    mr_info: &MRInfo,
    diff_raw: &str,
    llm_configs: &[LLMConfig],
    config: &AppConfig,
    progress_map: Option<ProgressMap>,
    review_id: &str,
    dump_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<(
    Vec<ExpertReport>,
    Option<GlobalReviewContext>,
    Vec<DroppedFinding>,
    ConsolidatedReport,
)> {
    // Initialize progress (skip if already initialized by caller)
    if let Some(ref map) = progress_map {
        let exists = map.read().ok().map(|g| g.contains_key(review_id)).unwrap_or(false);
        if !exists {
            let stages = if diff_raw.len() > crate::diff::large_pr::pre_assess_bytes(&config.diff) {
                StageWeight::large_pr()
            } else {
                StageWeight::small_pr()
            };
            let progress = ReviewProgress::new(review_id.to_string(), &stages);
            if let Ok(mut g) = map.write() {
                g.insert(review_id.to_string(), progress);
            }
        }
    }

    let (reports, _, _, errors, global_context, dropped_findings, consolidated) = run_experts_inner(
        experts,
        mr_info,
        diff_raw,
        llm_configs,
        config,
        progress_map.as_ref(),
        review_id,
        Some(&mr_info.target_branch),
        Some(&mr_info.source_branch),
        dump_dir,
    )
    .await?;

    // Zero output must not be recorded as success: when every expert task
    // failed (e.g. no valid LLM provider), surface the run as an error instead
    // of returning an empty report set. A legitimately empty team (no experts
    // configured) produces no errors and is left untouched, and a successful
    // review with zero findings still has non-empty reports.
    if reports.is_empty() && !errors.is_empty() {
        let sample = errors.first().map(|s| s.as_str()).unwrap_or("");
        anyhow::bail!(
            "all experts failed: {} expert task(s) errored (first error: {})",
            errors.len(),
            sample
        );
    }

    Ok((reports, global_context, dropped_findings, consolidated))
}

/// Run the aggregator expert to merge individual expert reports.
///
/// Builds an aggregator prompt from all per-expert reports (plus
/// optional global context), calls the LLM, and parses the result
/// into a consolidated [`AggregatedReport`]. Updates progress tracking
/// along the way.
pub async fn run_aggregator(
    aggregator: &ExpertDef,
    reports: &[ExpertReport],
    llm_configs: &[LLMConfig],
    mr_info: &MRInfo,
    global_context: Option<&GlobalReviewContext>,
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> anyhow::Result<AggregatedReport> {
    let prompt_engine = PromptEngine::new();
    let llm_client = LLMClient::new();

    let (system, user) = prompt_engine.build_aggregator_prompt(reports, mr_info, global_context, "en")?;
    let config = select_llm_config(aggregator, llm_configs);
    let result = llm_client.complete_with_fallback(&config, &system, &user).await?;

    // Mark aggregate stage as running
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.set_stage("aggregate", 0.5, "Aggregating expert reports...".to_string());
            }
        }
    }

    // Complete aggregate stage
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("aggregate");
            }
        }
    }

    crate::output::parser::parse_aggregator_response(&result.content)
}

/// Resolve the review input into raw diff text and MR info.
async fn resolve_input(_command: &Command, input: &ReviewInput) -> anyhow::Result<(String, MRInfo)> {
    match input {
        ReviewInput::GitLabMR { .. } => {
            anyhow::bail!(
                "GitLab MR review not yet supported via TeamOrchestrator. Use the existing GitLab client path."
            );
        }
        ReviewInput::GitHubPR { .. } => {
            anyhow::bail!("GitHub PR review not yet supported.");
        }
        ReviewInput::LocalRepo {
            path,
            base_ref,
            head_ref,
            ..
        } => {
            let diff = crate::input::resolve_diff(input).await?;
            let base = base_ref.as_deref().unwrap_or("main");
            let mr_info = MRInfo::new(
                path.clone(),
                format!("Local review: {}", path),
                head_ref.clone().unwrap_or_else(|| "HEAD".to_string()),
                base.to_string(),
            );
            Ok((diff, mr_info))
        }
    }
}
