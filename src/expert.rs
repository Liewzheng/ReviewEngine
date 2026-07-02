//! Single-expert execution logic for LLM-based reviewers.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//! [`run_single_expert`] drives the full lifecycle of one expert:
//! building the prompt via [`PromptEngine`], selecting the LLM
//! configuration, calling the LLM via [`LLMClient`], and parsing
//! the response into an [`ExpertReport`]. [`run_aggregator_expert`]
//! performs the same flow for the aggregator role, which merges
//! multiple expert reports into a single consolidated assessment.

use crate::llm::client::LLMClient;
use crate::models::*;
use crate::output::parser;
use crate::prompt::PromptEngine;
use anyhow::Result;

/// Execute a single LLM-based expert review against a diff.
///
/// Builds the expert's prompt (system + user) via [`PromptEngine`],
/// selects the appropriate LLM config for this expert, calls the LLM
/// with automatic fallback, and parses the YAML response into an
/// [`ExpertReport`].
pub async fn run_single_expert(
    expert: &ExpertDef,
    mr_info: &MRInfo,
    diff_text: &str,
    llm_configs: &[LLMConfig],
    settings: &AppConfig,
    prompt_engine: &PromptEngine,
    llm_client: &LLMClient,
) -> Result<ExpertReport> {
    let lang = "Unknown";

    let (system, user) = prompt_engine.build_review_prompt(expert, mr_info, diff_text, lang, settings)?;

    let config = crate::llm::select_llm_config(expert, llm_configs);
    let result = llm_client.complete_with_fallback(&config, &system, &user).await?;

    Ok(parser::parse_llm_response(&expert.name, &result.content))
}

/// Execute the aggregator expert to merge multiple expert reports.
///
/// Builds the aggregator prompt (including all per-expert reports and
/// optional global context), calls the LLM with fallback, and parses
/// the YAML response into an [`AggregatedReport`].
pub async fn run_aggregator_expert(
    aggregator: &ExpertDef,
    reports: &[ExpertReport],
    llm_configs: &[LLMConfig],
    mr_info: &MRInfo,
    global_context: Option<&GlobalReviewContext>,
    prompt_engine: &PromptEngine,
    llm_client: &LLMClient,
) -> Result<AggregatedReport> {
    let lang = "zh";

    let (system, user) = prompt_engine.build_aggregator_prompt(reports, mr_info, global_context, lang)?;

    let config = crate::llm::select_llm_config(aggregator, llm_configs);
    let result = llm_client.complete_with_fallback(&config, &system, &user).await?;

    parser::parse_aggregator_response(&result.content)
}
