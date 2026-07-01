//! Top-level review orchestration: running experts and aggregators.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! [`run_experts`] is the main entry point that initialises progress
//! tracking, detects large PRs to select appropriate stage weights,
//! and delegates to the team orchestrator for the full pipeline
//! (diff parsing, filtering, chunking, parallel expert dispatch, and
//! lead consolidation). [`run_aggregator`] runs a single aggregator
//! expert that merges all individual expert reports into a final
//! consolidated assessment.

use anyhow::Result;

use crate::models::*;
use crate::progress::{ProgressMap, ReviewProgress, StageWeight};

/// Run all applicable experts against the given MR diff.
///
/// Initialises progress tracking (auto-detecting small vs. large PR
/// stage weights), then delegates to the team orchestrator for the
/// full pipeline: diff parsing, filtering, chunking, lead overview
/// (Pass 1), rate-limited parallel expert dispatch, and consolidation.
///
/// Returns per-expert reports and an optional global review context.
pub async fn run_experts(
    experts: &[ExpertDef],
    mr_info: &MRInfo,
    diff_raw: &str,
    llm_configs: &[LLMConfig],
    settings: &AppConfig,
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> Result<(Vec<ExpertReport>, Option<GlobalReviewContext>)> {
    // Initialize progress (skip if already initialized by caller)
    if let Some(ref map) = progress_map {
        let exists = map.read().ok().map(|g| g.contains_key(review_id)).unwrap_or(false);
        if !exists {
            let stages = if diff_raw.len() > crate::diff::large_pr::pre_assess_bytes(&settings.diff) {
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

    // Delegate to team orchestrator which handles full pipeline:
    // diff parsing → filtering → large PR detection → compression →
    // chunking → lead overview (Pass 1) → rate limiting → parallel dispatch
    let (reports, _, _, _, global_context) = crate::team::orchestrator::run_experts_inner(
        experts,
        mr_info,
        diff_raw,
        llm_configs,
        settings,
        progress_map.as_ref(),
        review_id,
    )
    .await?;

    Ok((reports, global_context))
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
) -> Result<AggregatedReport> {
    let prompt_engine = crate::prompt::PromptEngine::new();
    let llm_client = crate::llm::client::LLMClient::new();

    let (system, user) = prompt_engine.build_aggregator_prompt(reports, mr_info, global_context, "zh")?;
    let config = crate::llm::select_llm_config(aggregator, llm_configs);
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
