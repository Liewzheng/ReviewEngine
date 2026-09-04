use futures::future::join_all;
use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::info;

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
use crate::progress::ProgressMap;
use crate::prompt::PromptEngine;

use crate::output::parser::validate_findings;
use crate::team::adjudicator;
use crate::team::lead_consolidator::{ConsolidatedReport, FileCoverage};
use crate::team::verifier::{self, DroppedFinding};

use super::validation::{apply_feedback_filter, build_consolidated_report, build_coverage_ledger};
use crate::team::ExpertMetrics;

/// Gather project context for the lead overview.
///
/// `mr_info.project_path` is a local filesystem path only for CLI local
/// reviews; for webhook/API-triggered reviews it is the provider slug
/// (`group/project`) and the server never clones the repository, so a
/// missing path is the expected case there — not a failure worth a
/// warning. When no local checkout exists (or gathering from it fails),
/// fall back to the reviewed diff's file list so first-time reviews of
/// repositories with no local cache still get a real (partial) context
/// instead of an empty default.
fn gather_lead_project_context(
    mr_info: &MRInfo,
    files: &[DiffFile],
    base_ref: Option<&str>,
    head_ref: Option<&str>,
) -> crate::context::ProjectContext {
    let diff_fallback = || crate::context::ProjectContext::from_diff_paths(files.iter().map(|f| f.path.as_str()));
    let repo_path = std::path::Path::new(&mr_info.project_path);
    if !repo_path.is_dir() {
        return diff_fallback();
    }
    match crate::context::gather_project_context(repo_path, base_ref, head_ref) {
        Ok(ctx) => ctx,
        Err(err) => {
            tracing::warn!("failed to gather project context: {}", err);
            diff_fallback()
        }
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
///
/// `chunked_mode` carries per-expert **chunk-grouped** file lists
/// (`Vec<Vec<DiffFile>>`): each inner `Vec` is one chunk, preserving chunk
/// boundaries so the per-expert chunk quota is enforced by chunk count, not
/// file count (root cause A).
#[allow(clippy::type_complexity)]
fn assess_and_chunk_diff(
    files: &mut Vec<DiffFile>,
    experts: &[ExpertDef],
    config: &AppConfig,
) -> (
    Vec<ExpertDef>,
    Option<(Vec<chunker::DiffChunk>, Vec<(ExpertDef, Vec<Vec<DiffFile>>)>)>,
) {
    let non_aggregators: Vec<ExpertDef> = experts.iter().filter(|e| e.name != "aggregator").cloned().collect();

    let thresholds = LargePrThresholds {
        max_files: config.diff.large_pr_file_threshold,
        max_total_changes: config.diff.large_pr_line_threshold as u32,
        max_tokens: config.diff.max_input_tokens,
    };
    let assessment = large_pr::assess_large_pr(files, &thresholds);

    let chunked_mode = if assessment.is_large && !non_aggregators.is_empty() {
        let (effective_level, compression_actions) = large_pr::apply_configured_compression(
            files,
            &config.diff.compression_level,
            &assessment.compression_level,
        );
        info!(
            "Large PR detected: {} files, {} changes, compressing at {:?} level ({} actions)",
            assessment.file_count,
            assessment.total_changes,
            effective_level,
            compression_actions.len()
        );

        let chunks = match config.diff.chunking_strategy.as_str() {
            "files" => chunker::chunk_by_files(files, config.diff.max_tokens_per_chunk),
            "hunks" => chunker::chunk_by_hunks(files, config.diff.max_tokens_per_chunk),
            "semantic" => chunker::semantic_chunk(files, config.diff.max_tokens_per_chunk),
            _ => chunker::adaptive_chunk(files, config.diff.max_tokens_per_chunk),
        };

        info!("Split into {} chunks", chunks.len());

        let assignments: Vec<(ExpertDef, Vec<Vec<DiffFile>>)> =
            large_pr::route_chunks(&chunks, &non_aggregators, config.diff.max_chunks_per_expert)
                .into_iter()
                .map(|(e, groups)| (e.clone(), groups))
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
        Ok(result) => parse_global_review_context(&result.content),
        Err(e) => {
            tracing::warn!("Pass 1 Overview failed: {:?}", e);
            None
        }
    }
}

/// Parse the lead overview's YAML response into a [`GlobalReviewContext`],
/// tolerating the formatting quirks LLMs commonly emit.
///
/// LLMs routinely wrap the document in a ```` ```yaml ```` fence, surround it
/// with prose, or indent with tabs. A backtick at line start is a reserved
/// YAML indicator and tabs are illegal indentation, so the strict scanner
/// aborts with "found character that cannot start any token" (RENG-26).
///
/// Attempts, in order, first success wins:
/// 1. strict parse of the raw response;
/// 2. parse after stripping code fences and normalizing tab indentation;
/// 3. parse of the first fenced YAML block only (drops surrounding prose and
///    any additional fenced blocks).
///
/// Returns `None` when every attempt fails; the caller then runs the expert
/// pass without a global context — the degradation path is unchanged.
fn parse_global_review_context(raw: &str) -> Option<GlobalReviewContext> {
    let strict_err = match serde_yaml_ng::from_str::<GlobalReviewContext>(raw) {
        Ok(ctx) => return Some(ctx),
        Err(e) => e,
    };

    let sanitized = sanitize_overview_yaml(raw);
    if let Ok(ctx) = serde_yaml_ng::from_str::<GlobalReviewContext>(&sanitized) {
        return Some(ctx);
    }

    if let Some(fenced) = crate::output::parser::extract_first_fenced_yaml(raw) {
        if let Ok(ctx) = serde_yaml_ng::from_str::<GlobalReviewContext>(&fenced) {
            return Some(ctx);
        }
    }

    tracing::warn!(
        "Failed to parse GlobalReviewContext: {:?} (fence/tab sanitization and fenced-block fallback also failed)",
        strict_err
    );
    None
}

/// Normalize LLM YAML quirks that trip the strict scanner: strip markdown
/// code fences (reusing the shared output-parser helper) and replace leading
/// tab indentation — illegal in YAML — with two spaces per tab. Tabs inside
/// scalar content are left untouched.
fn sanitize_overview_yaml(text: &str) -> String {
    let stripped = crate::output::parser::clean_yaml(text);
    stripped
        .lines()
        .map(|line| {
            let tabs = line.bytes().take_while(|&b| b == b'\t').count();
            if tabs == 0 {
                line.to_string()
            } else {
                format!("{}{}", "  ".repeat(tabs), &line[tabs..])
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
///
/// `file_contents` is the rendered "Full File Contents" section for the
/// files this task reviews (empty when unavailable, e.g. remote reviews).
fn create_expert_task(
    expert: ExpertDef,
    mr_info: MRInfo,
    diff_text: String,
    file_contents: String,
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
    dump_dir: Option<std::path::PathBuf>,
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
            if file_contents.is_empty() {
                None
            } else {
                Some(file_contents.as_str())
            },
        )?;
        let llm_config = select_llm_config(&expert, &llm_configs);
        let result = llm_client.complete_with_fallback(&llm_config, &system, &user).await?;
        let report = crate::output::parser::parse_llm_response(&expert.name, &result.content);
        let mut report = report;
        // `--verbose`: persist the raw LLM prompt + response to the dump dir so
        // a zero-finding or mis-parsed run can be debugged from the actual LLM
        // exchange, and reference the file path on the report (the renderer
        // shows a truncated excerpt + the full path).
        if let Some(dir) = &dump_dir {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("warning: [verbose] failed to create dump dir {}: {e}", dir.display());
            } else {
                let seq = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let safe: String = expert
                    .name
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                let prompt_path = dir.join(format!("{safe}.{seq}.prompt.txt"));
                let resp_path = dir.join(format!("{safe}.{seq}.response.txt"));
                let prompt_text = format!("=== SYSTEM ===\n{system}\n\n=== USER ===\n{user}\n");
                if let Err(e) = std::fs::write(&prompt_path, &prompt_text) {
                    eprintln!("warning: [verbose] failed to write prompt dump: {e}");
                } else if let Err(e) = std::fs::write(&resp_path, &result.content) {
                    eprintln!("warning: [verbose] failed to write response dump: {e}");
                } else {
                    report.raw_dump_path = Some(resp_path.display().to_string());
                    eprintln!(
                        "[verbose] {}: prompt -> {}, response -> {}",
                        expert.name,
                        prompt_path.display(),
                        resp_path.display()
                    );
                }
            }
        }
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
/// Returns (reports, per-expert metrics, total_tokens, error_messages, global_context,
/// dropped_findings, consolidated).
#[allow(clippy::type_complexity)]
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
    dump_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<(
    Vec<ExpertReport>,
    Vec<ExpertMetrics>,
    u64,
    Vec<String>,
    Option<GlobalReviewContext>,
    Vec<DroppedFinding>,
    ConsolidatedReport,
)> {
    let mut files = parse_and_filter_diff(diff_raw);

    // Assess large PR and set up chunking if needed
    let (non_aggregators, chunked_mode) = assess_and_chunk_diff(&mut files, experts, config);

    // Lead expert for the adjudication pass's model selection — captured
    // before `non_aggregators` is consumed by the task-building loops.
    let lead_for_adjudication = non_aggregators
        .iter()
        .find(|e| e.name.to_lowercase().contains("lead"))
        .or_else(|| non_aggregators.first())
        .cloned();

    // Gather lightweight project context for the lead overview
    let project_context = gather_lead_project_context(mr_info, &files, base_ref, head_ref);

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

    // A2: inject the current full contents of changed files into expert
    // prompts (local reviews only, budget-capped). Files unreadable from
    // `mr_info.project_path` — e.g. remote GitLab/GitHub reviews — are
    // skipped silently, leaving the prompt section absent as before.
    let max_context_file_bytes = config.diff.max_context_file_bytes;

    info!(
        "Team review: {} experts on {} files",
        non_aggregators.len(),
        files.len(),
    );

    let (semaphore, rate_limiter, completed_count) = setup_concurrency_control(config);

    let total_experts = non_aggregators.len();
    let total_tasks = chunked_mode.as_ref().map(|(_, a)| a.len()).unwrap_or(total_experts);

    // Build tasks: use chunked or non-chunked path. Alongside each task we
    // record the file paths it was assigned, so coverage accounting can tell
    // which files actually got a reviewer (root cause: files that end up in
    // no task, or only in failed tasks, must be surfaced, never silently
    // dropped from the report).
    let mut tasks: Vec<Task> = Vec::new();
    let mut task_files: Vec<Vec<String>> = Vec::new();

    if let Some((_chunks, assignments)) = chunked_mode {
        // Root cause A: `route_chunks` already bounded each expert to
        // `max_chunks_per_expert` CHUNKS (preserving chunk boundaries), so no
        // further per-file truncation happens here — flattening the chunk
        // groups is all that remains. A defensively empty task is skipped.
        for (expert, chunk_groups) in assignments {
            let files_for_task: Vec<DiffFile> = chunk_groups.into_iter().flatten().collect();
            if files_for_task.is_empty() {
                continue;
            }
            task_files.push(files_for_task.iter().map(|f| f.path.clone()).collect());

            let task_diff_text = processor::render_diff_text(&files_for_task);
            let task_lang = filter::detect_language(&files_for_task);
            // Chunked mode: each task injects only the contents of the
            // files in its own chunk assignment.
            let task_file_contents = crate::context::file_contents::build_file_contents_section(
                &files_for_task,
                &mr_info.project_path,
                max_context_file_bytes,
            );

            tasks.push(create_expert_task(
                expert,
                mr_info.clone(),
                task_diff_text,
                task_file_contents,
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
                dump_dir.clone(),
            ));
        }
    } else {
        // Non-chunked mode: all experts share one injection built from the
        // full changed-file list.
        let file_contents = crate::context::file_contents::build_file_contents_section(
            &files,
            &mr_info.project_path,
            max_context_file_bytes,
        );
        for expert in non_aggregators {
            task_files.push(files.iter().map(|f| f.path.clone()).collect());
            tasks.push(create_expert_task(
                expert,
                mr_info.clone(),
                diff_text.clone(),
                file_contents.clone(),
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
                dump_dir.clone(),
            ));
        }
    }

    let results: Vec<anyhow::Result<(ExpertReport, u64, u64)>> = join_all(tasks).await;

    // Coverage accounting (anti-cheat): a file counts as reviewed only if it
    // was assigned to a task that produced a report. Files assigned to no
    // task at all (quota shortfall in `route_chunks`) or only to failed
    // tasks are reported here and later cap the score, so under-coverage can
    // never inflate the result (the 4-of-29-files / fake-85 regression).
    let mut reviewed: HashSet<String> = HashSet::new();
    for (result, paths) in results.iter().zip(task_files.iter()) {
        if result.is_ok() {
            reviewed.extend(paths.iter().cloned());
        }
    }
    let coverage = FileCoverage {
        total_files: files.len(),
        reviewed_files: reviewed.len(),
        unreviewed_files: files
            .iter()
            .map(|f| f.path.clone())
            .filter(|p| !reviewed.contains(p))
            .collect(),
    };
    if coverage.unreviewed_files.is_empty() {
        info!(
            "Coverage: {} of {} files reviewed (complete)",
            coverage.reviewed_files, coverage.total_files
        );
    } else {
        tracing::warn!(
            "Coverage: {} of {} files reviewed; {} files not covered by any expert: {}",
            coverage.reviewed_files,
            coverage.total_files,
            coverage.unreviewed_files.len(),
            coverage.unreviewed_files.join(", ")
        );
    }

    // Mark expert_review complete
    mark_expert_stage_complete(progress_map, review_id);

    let (mut reports, metrics, total_tokens, errors) = collect_expert_results(results);

    // Validate each expert's findings against the parsed diff.
    let diff_files: Vec<(String, Vec<DiffHunk>)> = files.iter().map(|f| (f.path.clone(), f.hunks.clone())).collect();
    for report in &mut reports {
        let before = report.findings.len();
        report.findings = validate_findings(&report.findings, &diff_files);
        // Re-render the per-expert markdown from the validated findings so
        // validation outcomes surface in the report — e.g. the keep-with-note
        // annotation on findings whose line lies outside the diff hunk.
        report.markdown = crate::output::renderer::render_expert_markdown(&report.expert_name, &report.findings);
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

    // Optional LLM verification pass: re-check findings against the diff
    // hunks, the referenced files' full content, and the changed-file list.
    // Fail-open by construction — `verify_findings` never returns an error.
    let mut dropped_findings = if config.report.verification_pass {
        let dropped = verifier::verify_findings(
            &mut reports,
            &files,
            &mr_info.project_path,
            llm_configs,
            config.report.verification_max_file_bytes,
        )
        .await;
        // Log unconditionally so a run that dropped nothing is still visible.
        let checked = reports.iter().map(|r| r.findings.len()).sum::<usize>() + dropped.len();
        info!(
            "Verification pass: checked {} findings, dropped {}",
            checked,
            dropped.len()
        );
        dropped
    } else {
        Vec::new()
    };

    // Feedback-driven filtering (A9): drop findings the user previously
    // marked as false positives, matched by stable fingerprint. Runs after
    // the verification pass and before lead consolidation; fail-open.
    let feedback_dropped = apply_feedback_filter(&mut reports, config.report.feedback_filtering);
    if !feedback_dropped.is_empty() {
        info!(
            "Feedback filtering: dropped {} finding(s) marked as false positives by user feedback",
            feedback_dropped.len()
        );
        dropped_findings.extend(feedback_dropped);
    }

    // Lead consolidation over the validated findings: confidence filtering,
    // deduplication, conflict detection, and overall scoring. Pure
    // computation, so it always runs. Coverage is threaded in so an
    // under-covered run is scored honestly (capped), never inflated. The
    // hunk-level ledger (changed vs. demonstrably-touched ranges) feeds the
    // coverage-insufficient / unverified marking.
    let coverage_ledger = build_coverage_ledger(&diff_files, &reports);
    let mut consolidated = build_consolidated_report(&reports, config, &coverage, Some(&coverage_ledger));

    // Final adjudication pass (false-positive reduction, phase 3): the
    // lead-model LLM re-examines each consolidated finding at or above
    // `adjudicate_min_severity` against the FULL content of the cited file —
    // bypassing the expert-context and verification byte caps that hid
    // defensive code from earlier passes. Dropped false positives are
    // recorded on the report (`adjudicated_removed`), never silent. Runs
    // after consolidation so each surviving finding is adjudicated exactly
    // once; fail-open on any infrastructure problem.
    if config.report.adjudicate && !llm_configs.is_empty() {
        let min_severity = adjudicator::parse_min_severity(&config.report.adjudicate_min_severity);
        let candidates = consolidated
            .findings
            .iter()
            .filter(|f| adjudicator::severity_rank(&f.severity) >= adjudicator::severity_rank(&min_severity))
            .count();
        // Select the lead/consolidation model role for the adjudicator, as
        // with the Pass 1 overview; fall back to the full config list.
        let adjudication_configs: Vec<LLMConfig> = match &lead_for_adjudication {
            Some(l) => select_llm_config(l, llm_configs),
            None => llm_configs.to_vec(),
        };
        if let Some(ref map) = progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(review_id) {
                    progress.set_stage("adjudicate", 0.5, format!("Adjudicating {} findings", candidates));
                }
            }
        }
        let removed = adjudicator::adjudicate_findings(
            &mut consolidated.findings,
            &mr_info.project_path,
            &adjudication_configs,
            &min_severity,
        )
        .await;
        info!(
            "Adjudication pass: examined {} finding(s) at or above {:?}, dropped {}",
            candidates,
            min_severity,
            removed.len()
        );
        consolidated.adjudicated_removed = removed;
    }

    // Adjudication stage is done (ran, skipped, or disabled — the static
    // stage list must still reach 100%).
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("adjudicate");
            }
        }
    }

    Ok((
        reports,
        metrics,
        total_tokens,
        errors,
        global_context,
        dropped_findings,
        consolidated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_file(path: &str) -> DiffFile {
        DiffFile {
            path: path.to_string(),
            old_path: path.to_string(),
            new_path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: Vec::new(),
        }
    }

    /// Regression (RENG-25): a webhook-triggered review of a freshly created
    /// repository carries the provider slug (`group/project`) in
    /// `mr_info.project_path`, and the server never clones the repo, so no
    /// local path exists. Context gathering must not fail/warn in that case;
    /// it must fall back to the diff's file list.
    #[test]
    fn test_gather_lead_project_context_slug_path_falls_back_to_diff() {
        let mr_info = MRInfo::new(
            "review-lab/review-engine-reng25-nonexistent".to_string(),
            "test".to_string(),
            "feat/x".to_string(),
            "main".to_string(),
        );
        assert!(
            !std::path::Path::new(&mr_info.project_path).is_dir(),
            "test prerequisite: slug path must not exist locally"
        );
        let files = vec![diff_file("src/lib.rs"), diff_file("README.md")];

        let ctx = gather_lead_project_context(&mr_info, &files, Some("main"), Some("feat/x"));

        assert_eq!(ctx.file_tree, vec!["README.md", "src/lib.rs"]);
        assert!(ctx.readme_excerpt.is_empty());
        assert!(ctx.recent_commits.is_empty());
        assert!(ctx.branch_commits.is_empty());
    }

    /// A real local checkout (CLI local review) must still gather the full
    /// git-backed context, unchanged from the pre-fix behaviour.
    #[test]
    fn test_gather_lead_project_context_local_checkout_uses_git() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };
        run(&["init"]);
        run(&["checkout", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
        std::fs::write(dir.path().join("README.md"), "# Local Project\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-m", "add readme"]);

        let mr_info = MRInfo::new(
            dir.path().to_string_lossy().to_string(),
            "test".to_string(),
            "main".to_string(),
            "main".to_string(),
        );
        let files = vec![diff_file("src/lib.rs")];

        let ctx = gather_lead_project_context(&mr_info, &files, Some("main"), Some("main"));

        assert!(ctx.readme_excerpt.contains("# Local Project"));
        assert!(ctx.file_tree.iter().any(|f| f == "README.md"));
        assert!(ctx.recent_commits.iter().any(|c| c.contains("add readme")));
    }
}

#[cfg(test)]
mod global_context_parse_tests {
    use super::*;

    const PLAIN_YAML: &str = "summary: \"Add auth\"\n\
                              risk_areas:\n  - \"Security: token handling\"\n\
                              focus_files:\n  - \"src/auth.rs\"\n\
                              guidance: \"Check token expiry\"\n";

    #[test]
    fn parse_global_context_plain_yaml() {
        let ctx = parse_global_review_context(PLAIN_YAML).expect("plain YAML should parse");
        assert_eq!(ctx.summary, "Add auth");
        assert_eq!(ctx.risk_areas, vec!["Security: token handling".to_string()]);
        assert_eq!(ctx.focus_files, vec!["src/auth.rs".to_string()]);
        assert_eq!(ctx.guidance, "Check token expiry");
    }

    /// Regression for RENG-26: a ```yaml fence alone used to abort the strict
    /// scanner ("found character that cannot start any token") and silently
    /// drop the global context.
    #[test]
    fn parse_global_context_fenced_yaml_with_prose() {
        let raw = "Here is the overview:\n\
                   ```yaml\n\
                   summary: \"Add auth\"\nrisk_areas: []\nfocus_files: []\nguidance: \"g\"\n\
                   ```\n\
                   Hope this helps.";
        let ctx = parse_global_review_context(raw).expect("fenced YAML should parse after sanitization");
        assert_eq!(ctx.summary, "Add auth");
    }

    #[test]
    fn parse_global_context_unclosed_fence() {
        let raw = "```yaml\nsummary: \"Add auth\"\nrisk_areas: []\nfocus_files: []\nguidance: \"g\"\n";
        let ctx = parse_global_review_context(raw).expect("unclosed fence should still parse");
        assert_eq!(ctx.summary, "Add auth");
    }

    /// Tab indentation is illegal in YAML and triggers the same scanner error
    /// family as fences; it must be normalized to spaces before parsing.
    #[test]
    fn parse_global_context_tab_indentation() {
        let raw = "summary: \"Add auth\"\nrisk_areas:\n\t- \"Security\"\nfocus_files:\n\t- \"src/auth.rs\"\nguidance: \"g\"\n";
        let ctx = parse_global_review_context(raw).expect("tab indentation should be normalized");
        assert_eq!(ctx.risk_areas, vec!["Security".to_string()]);
        assert_eq!(ctx.focus_files, vec!["src/auth.rs".to_string()]);
    }

    /// Special characters (colons, emoji) inside properly quoted scalars must
    /// survive the sanitize path unchanged.
    #[test]
    fn parse_global_context_quoted_special_characters() {
        let raw = "```yaml\n\
                   summary: \"Refactor auth: split middleware 🔒\"\n\
                   risk_areas:\n  - \"Security: auth middleware changes\"\n  - \"Perf: N+1 query ⚠️\"\n\
                   focus_files: []\n\
                   guidance: \"Note: check token expiry\"\n\
                   ```";
        let ctx = parse_global_review_context(raw).expect("quoted colons/emoji should parse");
        assert_eq!(ctx.summary, "Refactor auth: split middleware 🔒");
        assert_eq!(ctx.risk_areas.len(), 2);
        assert_eq!(ctx.guidance, "Note: check token expiry");
    }

    /// When sanitization still yields invalid YAML (e.g. multiple fenced
    /// blocks concatenated), fall back to parsing the first fenced block only.
    #[test]
    fn parse_global_context_first_fenced_block_fallback() {
        let raw = "```yaml\n\
                   summary: \"Add auth\"\nrisk_areas: []\nfocus_files: []\nguidance: \"g\"\n\
                   ```\n\
                   trailing prose\n\
                   ```yaml\n\
                   not: valid: yaml\n\
                   ```";
        let ctx = parse_global_review_context(raw).expect("first fenced block should be recovered");
        assert_eq!(ctx.summary, "Add auth");
    }

    /// Total garbage must still degrade to `None` — never panic, never guess.
    #[test]
    fn parse_global_context_garbage_returns_none() {
        assert!(parse_global_review_context("not yaml at all: [unclosed").is_none());
    }
}
