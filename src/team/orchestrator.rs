use async_trait::async_trait;
use futures::future::join_all;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
use tracing::info;

use crate::actions::registry::CommandRegistry;
use crate::actions::registry::ExpertSelection;
use crate::diff::chunker;
use crate::diff::filter;
use crate::diff::large_pr;
use crate::diff::large_pr::LargePrThresholds;
use crate::diff::parser as diff_parser;
use crate::diff::processor;
use crate::llm::client::LLMClient;
use crate::llm::rate_limiter::RateLimiter;
use crate::llm::select_llm_config;
use crate::models::*;
use crate::progress::{ProgressMap, ReviewProgress, StageWeight};
use crate::prompt::PromptEngine;

use crate::output::parser::validate_findings;
use crate::team::lead_consolidator::ConsolidatorConfig;

use super::{ExpertMetrics, TeamOrchestrator, TeamReport};

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

        let (reports, metrics, total_tokens, errors, _global_context) = run_experts_inner(
            &selected_defs,
            &mr_info,
            &diff_raw,
            llm_configs,
            config,
            self.progress_map.as_ref(),
            &self.review_id,
            base_ref.as_deref(),
            head_ref.as_deref(),
        )
        .await?;

        // Lead consolidation: merge and filter validated findings.
        let consolidated = {
            let consolidator_config = ConsolidatorConfig {
                min_confidence: config.report.min_confidence,
                drop_low_confidence: config.report.drop_low_confidence,
                scoring: Some(config.scoring.clone()),
                ..Default::default()
            };
            Some(consolidator_config.consolidate(&reports, None))
        };

        // Phase 3-4: Cross-check & Lead Consolidation (placeholder for future)
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
            consolidated,
        })
    }
}

/// Parse the raw unified diff and filter out ignored files.
fn parse_and_filter_diff(diff_raw: &str) -> Vec<DiffFile> {
    let mut files = diff_parser::parse_unified_diff(diff_raw);
    files.retain(|f| !filter::should_ignore(f));
    files
}

/// Assess whether the diff constitutes a large PR and, if so, apply compression
/// and build chunk assignments for the expert team.
#[allow(clippy::type_complexity)]
fn assess_and_chunk_diff(
    files: &mut Vec<DiffFile>,
    experts: &[ExpertDef],
    config: &AppConfig,
) -> (
    Vec<ExpertDef>,
    Option<(Vec<chunker::DiffChunk>, Vec<(ExpertDef, Vec<DiffFile>)>)>,
) {
    let non_aggregators: Vec<ExpertDef> = experts.iter().filter(|e| e.name != "aggregator").cloned().collect();

    let thresholds = LargePrThresholds {
        max_files: config.diff.large_pr_file_threshold,
        max_total_changes: config.diff.large_pr_line_threshold as u32,
        max_tokens: config.diff.max_input_tokens,
    };
    let assessment = large_pr::assess_large_pr(files, &thresholds);

    let chunked_mode = if assessment.is_large && !non_aggregators.is_empty() {
        info!(
            "Large PR detected: {} files, {} changes, compressing at {:?} level",
            assessment.file_count, assessment.total_changes, assessment.compression_level
        );

        large_pr::apply_compression(files, &assessment.compression_level);

        let chunks = match config.diff.chunking_strategy.as_str() {
            "files" => chunker::chunk_by_files(files, config.diff.max_tokens_per_chunk),
            "hunks" => chunker::chunk_by_hunks(files, config.diff.max_tokens_per_chunk),
            _ => chunker::adaptive_chunk(files, config.diff.max_tokens_per_chunk),
        };

        info!("Split into {} chunks", chunks.len());

        let assignments: Vec<(ExpertDef, Vec<DiffFile>)> = large_pr::route_chunks(&chunks, &non_aggregators)
            .into_iter()
            .map(|(e, files)| (e.clone(), files))
            .collect();
        Some((chunks, assignments))
    } else {
        None
    };

    (non_aggregators, chunked_mode)
}

/// Run Pass 1 Lead Overview — produces a `GlobalReviewContext` that is
/// appended to every expert's prompt, regardless of PR size.
async fn build_lead_overview(
    mr_info: &MRInfo,
    files: &[DiffFile],
    non_aggregators: &[ExpertDef],
    llm_configs: &[LLMConfig],
    project_config: Option<&crate::models::ProjectConfig>,
    project_context: &crate::context::ProjectContext,
) -> Option<GlobalReviewContext> {
    let lead_expert = non_aggregators
        .iter()
        .find(|e| e.name.to_lowercase().contains("lead"))
        .or_else(|| non_aggregators.first());

    let lead = match lead_expert {
        Some(l) => l,
        None => return None,
    };

    let overview_diff = processor::render_diff_text(files);
    let overview_config = select_llm_config(lead, llm_configs);
    let prompt_engine = PromptEngine::new();
    let llm_client = LLMClient::new();

    let (system, user) =
        match prompt_engine.build_overview_prompt(mr_info, project_config, project_context, &overview_diff) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to build overview prompt: {:?}", e);
                return None;
            }
        };

    match llm_client
        .complete_with_fallback(&overview_config, &system, &user)
        .await
    {
        Ok(result) => match serde_yaml_ng::from_str::<GlobalReviewContext>(&result.content) {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                tracing::warn!("Failed to parse GlobalReviewContext: {:?}", e);
                None
            }
        },
        Err(e) => {
            tracing::warn!("Pass 1 Overview failed: {:?}", e);
            None
        }
    }
}

/// Set up concurrency-control infrastructure: semaphore, rate limiter, and
/// completion counter.
fn setup_concurrency_control(config: &AppConfig) -> (Arc<Semaphore>, Arc<RateLimiter>, Arc<AtomicUsize>) {
    let max_concurrent = config.max_concurrent_llm_calls.unwrap_or(6);
    let semaphore = Arc::new(Semaphore::new(max_concurrent));
    let rate_limiter = Arc::new(RateLimiter::new(
        config.rate_limit.max_rpm,
        config.rate_limit.max_tpm,
        config.rate_limit.window_seconds,
    ));
    let completed_count = Arc::new(AtomicUsize::new(0));
    (semaphore, rate_limiter, completed_count)
}

type Task = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<(ExpertReport, u64, u64)>> + Send>>;

/// Create a boxed future for a single expert review task, shared by both
/// the chunked and non-chunked execution paths.
fn create_expert_task(
    expert: ExpertDef,
    mr_info: MRInfo,
    diff_text: String,
    lang: String,
    llm_configs: Vec<LLMConfig>,
    config: AppConfig,
    semaphore: Arc<Semaphore>,
    rate_limiter: Arc<RateLimiter>,
    completed_count: Arc<AtomicUsize>,
    total_tasks: usize,
    progress_map: Option<ProgressMap>,
    review_id: String,
    global_context: Option<GlobalReviewContext>,
) -> Task {
    Box::pin(async move {
        let task_start = std::time::Instant::now();
        let _permit = semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("Semaphore error: {}", e))?;

        let estimated_tokens = crate::tokenizer::count_tokens(&diff_text, "gpt-4").unwrap_or(0);
        if let Err(e) = rate_limiter.acquire(estimated_tokens).await {
            tracing::warn!("RateLimiter::acquire failed (proceeding anyway): {:?}", e);
        }

        let prompt_engine = PromptEngine::new();
        let llm_client = LLMClient::new();
        let (system, user) = prompt_engine.build_review_prompt(
            &expert,
            &mr_info,
            &diff_text,
            &lang,
            &config,
            global_context.as_ref(),
        )?;
        let llm_config = select_llm_config(&expert, &llm_configs);
        let result = llm_client.complete_with_fallback(&llm_config, &system, &user).await?;
        let report = crate::output::parser::parse_llm_response(&expert.name, &result.content);
        let latency_ms = task_start.elapsed().as_millis() as u64;

        crate::progress::update_expert_progress(progress_map.as_ref(), &review_id, &completed_count, total_tasks);

        info!(
            "Expert '{}' completed {} findings in {}ms ({} tokens)",
            expert.name,
            report.findings.len(),
            latency_ms,
            result.total_tokens
        );
        Ok::<(ExpertReport, u64, u64), anyhow::Error>((report, latency_ms, result.total_tokens))
    })
}

/// Mark the expert_review stage as complete in the progress map.
fn mark_expert_stage_complete(progress_map: Option<&ProgressMap>, review_id: &str) {
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("expert_review");
            }
        }
    }
}

/// Iterate over task results and split them into reports, metrics, and errors.
fn collect_expert_results(
    results: Vec<anyhow::Result<(ExpertReport, u64, u64)>>,
) -> (Vec<ExpertReport>, Vec<ExpertMetrics>, u64, Vec<String>) {
    let mut reports = Vec::new();
    let mut total_tokens: u64 = 0;
    let mut metrics = Vec::new();
    let mut errors = Vec::new();

    for r in results {
        match r {
            Ok((report, latency_ms, tokens_used)) => {
                metrics.push(ExpertMetrics {
                    name: report.expert_name.clone(),
                    latency_ms,
                    tokens_used,
                });
                total_tokens += tokens_used;
                reports.push(report);
            }
            Err(e) => {
                let msg = format!("Expert task failed: {:?}", e);
                tracing::error!("{}", msg);
                errors.push(msg);
            }
        }
    }

    (reports, metrics, total_tokens, errors)
}

/// Run the core expert pipeline: diff parsing → large PR handling → parallel LLM execution.
///
/// Returns (reports, per-expert metrics, total_tokens, error_messages, global_context).
pub(crate) async fn run_experts_inner(
    experts: &[ExpertDef],
    mr_info: &MRInfo,
    diff_raw: &str,
    llm_configs: &[LLMConfig],
    config: &AppConfig,
    progress_map: Option<&ProgressMap>,
    review_id: &str,
    base_ref: Option<&str>,
    head_ref: Option<&str>,
) -> anyhow::Result<(
    Vec<ExpertReport>,
    Vec<ExpertMetrics>,
    u64,
    Vec<String>,
    Option<GlobalReviewContext>,
)> {
    let mut files = parse_and_filter_diff(diff_raw);

    // Assess large PR and set up chunking if needed
    let (non_aggregators, chunked_mode) = assess_and_chunk_diff(&mut files, experts, config);

    // Gather lightweight project context for the lead overview
    let project_context =
        match crate::context::gather_project_context(std::path::Path::new(&mr_info.project_path), base_ref, head_ref) {
            Ok(ctx) => ctx,
            Err(err) => {
                tracing::warn!("failed to gather project context: {}", err);
                crate::context::ProjectContext::default()
            }
        };

    // Pass 1: Lead Overview (now runs for all PR sizes)
    let global_context: Option<GlobalReviewContext> = build_lead_overview(
        mr_info,
        &files,
        &non_aggregators,
        llm_configs,
        config.project.as_ref(),
        &project_context,
    )
    .await;

    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("lead_overview");
            }
        }
    }

    processor::apply_token_budget(&mut files, 0);

    let diff_text = processor::render_diff_text(&files);
    let lang = filter::detect_language(&files);

    info!(
        "Team review: {} experts on {} files",
        non_aggregators.len(),
        files.len(),
    );

    let (semaphore, rate_limiter, completed_count) = setup_concurrency_control(config);

    let total_experts = non_aggregators.len();
    let total_tasks = chunked_mode.as_ref().map(|(_, a)| a.len()).unwrap_or(total_experts);

    // Build tasks: use chunked or non-chunked path
    let tasks: Vec<Task> = if let Some((_chunks, assignments)) = chunked_mode {
        let max_chunks_per_expert = config.diff.max_chunks_per_expert;
        assignments
            .into_iter()
            .map(|(expert, files)| {
                let files_for_task: Vec<DiffFile> = if max_chunks_per_expert > 0 && files.len() > max_chunks_per_expert
                {
                    files.into_iter().take(max_chunks_per_expert).collect()
                } else {
                    files
                };

                let task_diff_text = processor::render_diff_text(&files_for_task);
                let task_lang = filter::detect_language(&files_for_task);

                create_expert_task(
                    expert,
                    mr_info.clone(),
                    task_diff_text,
                    task_lang,
                    llm_configs.to_vec(),
                    config.clone(),
                    semaphore.clone(),
                    rate_limiter.clone(),
                    completed_count.clone(),
                    total_tasks,
                    progress_map.cloned(),
                    review_id.to_string(),
                    global_context.clone(),
                )
            })
            .collect()
    } else {
        non_aggregators
            .into_iter()
            .map(|expert| {
                create_expert_task(
                    expert,
                    mr_info.clone(),
                    diff_text.clone(),
                    lang.clone(),
                    llm_configs.to_vec(),
                    config.clone(),
                    semaphore.clone(),
                    rate_limiter.clone(),
                    completed_count.clone(),
                    total_tasks,
                    progress_map.cloned(),
                    review_id.to_string(),
                    global_context.clone(),
                )
            })
            .collect()
    };

    let results: Vec<anyhow::Result<(ExpertReport, u64, u64)>> = join_all(tasks).await;

    // Mark expert_review complete
    mark_expert_stage_complete(progress_map, review_id);

    let (mut reports, metrics, total_tokens, errors) = collect_expert_results(results);

    // Validate each expert's findings against the parsed diff.
    let diff_files: Vec<(String, Vec<DiffHunk>)> = files.iter().map(|f| (f.path.clone(), f.hunks.clone())).collect();
    for report in &mut reports {
        let before = report.findings.len();
        report.findings = validate_findings(&report.findings, &diff_files);
        let dropped = before.saturating_sub(report.findings.len());
        if dropped > 0 {
            tracing::warn!(
                "Expert '{}': {} findings dropped after validation",
                report.expert_name,
                dropped
            );
        } else {
            tracing::info!("Expert '{}': all findings passed validation", report.expert_name);
        }
    }

    Ok((reports, metrics, total_tokens, errors, global_context))
}

/// Run all applicable experts against the given MR diff.
///
/// Initialises progress tracking (auto-detecting small vs. large PR
/// stage weights), then runs the full pipeline: diff parsing, filtering,
/// chunking, lead overview (Pass 1), rate-limited parallel expert
/// dispatch, and consolidation.
///
/// Returns per-expert reports and an optional global review context.
pub async fn run_experts(
    experts: &[ExpertDef],
    mr_info: &MRInfo,
    diff_raw: &str,
    llm_configs: &[LLMConfig],
    config: &AppConfig,
    progress_map: Option<ProgressMap>,
    review_id: &str,
) -> anyhow::Result<(Vec<ExpertReport>, Option<GlobalReviewContext>)> {
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

    let (reports, _, _, _, global_context) = run_experts_inner(
        experts,
        mr_info,
        diff_raw,
        llm_configs,
        config,
        progress_map.as_ref(),
        review_id,
        Some(&mr_info.target_branch),
        Some(&mr_info.source_branch),
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
