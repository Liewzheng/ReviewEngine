mod metadata;
mod render;
mod scoring;
#[cfg(test)]
mod tests;
mod types;

pub use render::render_repo_review_output;
pub use types::*;

use metadata::*;
use scoring::*;

use crate::llm::client::LLMClient;
use crate::models::*;
use crate::progress::{ProgressMap, StageWeight};
use crate::repo::experts::llm_experts;
use crate::repo::experts::static_experts;
use crate::repo::experts::{self, ExpertScore, RepoContext, RepoExpert};
use crate::repo::{FileEntry, RepoScanner};
use anyhow::Result;
use std::sync::Arc;

/// Run the 6 static experts and produce a weighted score.
async fn run_static_experts(ctx: &RepoContext) -> Vec<ExpertScore> {
    let experts: Vec<Box<dyn RepoExpert>> = vec![
        Box::new(static_experts::CodeOrganization),
        Box::new(crate::repo::experts::test_coverage::TestCoverage),
        Box::new(static_experts::Security),
        Box::new(static_experts::Documentation),
        Box::new(static_experts::Dependency),
        Box::new(static_experts::CodeStyle),
    ];

    let mut scores = Vec::with_capacity(experts.len());
    for e in &experts {
        match e.evaluate(ctx, None).await {
            Ok(s) => scores.push(s),
            Err(err) => {
                // Never drop a failed expert silently: the score must land so
                // the weight normalisation keeps its shape, and the fallback
                // flag keeps the synthetic 50 visible in the report.
                tracing::warn!("Expert {} failed: {:?}", e.name(), err);
                eprintln!(
                    "WARN: static expert '{}' failed: {err:#}; recording explicit fallback score",
                    e.name()
                );
                scores.push(ExpertScore {
                    expert_name: e.name().to_string(),
                    weight: e.weight(),
                    score: 50,
                    summary: format!("Evaluation failed: {err}"),
                    details: Vec::new(),
                    fallback: true,
                    evaluated_loc: None,
                    samples: None,
                });
            }
        }
    }
    scores
}

/// Build a RepoReviewOutput from expert scores for the local-only path.
fn build_output(scores: &[ExpertScore], stats: &crate::repo::RepoStats, metadata: ReviewMetadata) -> RepoReviewOutput {
    let (health_score, risk_level) = experts::weighted_total(scores);
    let conv = convert_scores(scores);
    let divisor = total_weight_f(&conv.expert_scores);

    // Build all report sections from converted scores
    let score_breakdown = build_score_breakdown(&conv.expert_scores, divisor);
    let languages = build_languages(stats);
    let risk_categories = build_risk_categories(&conv.expert_scores);
    let action_items = build_action_items(&conv.expert_scores);

    let overview = ReportOverview {
        health_score,
        risk_level: risk_level.clone(),
        total_experts: scores.len(),
        total_files: stats.total_files,
        total_loc: stats.total_loc,
        languages,
        lead_summary: conv.lead_summary,
        score_breakdown,
    };

    let conclusion = ReportConclusion {
        aggregated_score: health_score,
        risk_level,
        top_risks: pick_top_risks(&risk_categories),
        recommendation: "Local analysis complete. Run with LLM for enhanced findings.".to_string(),
    };

    RepoReviewOutput {
        overview,
        expert_scores: conv.expert_scores,
        risk_categories,
        action_items,
        conclusion,
        dropped_findings: Vec::new(),
        verification_ran: false,
        metadata,
    }
}

/// Build output from aggregated (deduplicated, filtered) scores.
fn build_output_from_aggregated(
    agg: &crate::repo::experts::aggregator::AggregatedResult,
    stats: &crate::repo::RepoStats,
    dropped_findings: Vec<crate::team::verifier::DroppedFinding>,
    verification_ran: bool,
    metadata: ReviewMetadata,
) -> RepoReviewOutput {
    let (health_score, risk_level) = experts::weighted_total(&agg.scores);
    let conv = convert_scores(&agg.scores);
    let divisor = total_weight_f(&conv.expert_scores);

    // Build all report sections from converted scores
    let score_breakdown = build_score_breakdown(&conv.expert_scores, divisor);
    let languages = build_languages(stats);
    let risk_categories = build_risk_categories(&conv.expert_scores);
    let action_items = build_action_items(&conv.expert_scores);

    let overview = ReportOverview {
        health_score,
        risk_level: risk_level.clone(),
        total_experts: agg.scores.len(),
        total_files: stats.total_files,
        total_loc: stats.total_loc,
        languages,
        lead_summary: conv.lead_summary,
        score_breakdown,
    };

    let conclusion = ReportConclusion {
        aggregated_score: agg.conclusion.aggregated_score,
        risk_level: agg.conclusion.risk_level.clone(),
        top_risks: agg.conclusion.top_risks.clone(),
        recommendation: agg.conclusion.recommendation.clone(),
    };

    RepoReviewOutput {
        overview,
        expert_scores: conv.expert_scores,
        risk_categories,
        action_items,
        conclusion,
        dropped_findings,
        verification_ran,
        metadata,
    }
}

/// Run a full local repository health review using the expert system (no LLM).
pub async fn run_local_repo_review(
    local_path: &str,
    progress_map: Option<ProgressMap>,
    review_id: &str,
    config: Option<Arc<AppConfig>>,
) -> Result<RepoReviewOutput> {
    // Initialize progress
    if let Some(ref map) = progress_map {
        let stages = StageWeight::repo_review();
        let progress = crate::progress::ReviewProgress::new(review_id.to_string(), &stages);
        if let Ok(mut g) = map.write() {
            g.insert(review_id.to_string(), progress);
        }
    }

    let scanner = RepoScanner::new(local_path);
    let entries = scanner.scan()?;
    let stats = scanner.compute_stats(&entries);
    // Provenance is captured at scan time so the timestamp, tree hash and
    // git SHA all describe the same snapshot the experts then scored.
    let metadata = build_metadata(local_path, &entries, &[], config.as_deref());

    // Track scan progress
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("scan");
            }
        }
    }

    let ctx = RepoContext {
        entries,
        stats,
        llm_configs: vec![],
        config,
        // No LLM prompt is built on the local-only path — no facts to inject.
        facts_block: None,
    };

    // Run static experts
    let scores = run_static_experts(&ctx).await;

    // Track local_analysis progress
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("local_analysis");
            }
        }
    }

    let result = build_output(&scores, &ctx.stats, metadata);

    // Mark progress complete
    crate::progress::complete_repo_progress(progress_map.as_ref(), review_id);

    Ok(result)
}

/// Run the repo-review command with LLM enhancement (3-pass architecture).
///
/// Pass 1: Architecture Lead evaluates file tree (1 LLM call)
/// Pass 2: CodeQuality evaluates each code chunk (N LLM calls, parallel)
/// Pass 3: Aggregator combines all scores
pub async fn run_repo_review(
    llm_client: &LLMClient,
    llm_configs: &[LLMConfig],
    local_path: &str,
    entries: &[FileEntry],
    progress_map: Option<ProgressMap>,
    review_id: &str,
    config: Option<Arc<AppConfig>>,
) -> Result<RepoReviewOutput> {
    // Initialize progress
    if let Some(ref map) = progress_map {
        let stages = StageWeight::repo_review();
        let progress = crate::progress::ReviewProgress::new(review_id.to_string(), &stages);
        if let Ok(mut g) = map.write() {
            g.insert(review_id.to_string(), progress);
        }
    }

    // Run static experts
    let scanner = crate::repo::RepoScanner::new(local_path);
    let stats = scanner.compute_stats(entries);
    // Provenance is captured before the expert passes so the timestamp, tree
    // hash and git SHA describe the scanned snapshot the experts then scored.
    let metadata = build_metadata(local_path, entries, llm_configs, config.as_deref());
    // Deterministic repo facts: computed once over the FULL entry set (never
    // per chunk) and shared with every LLM expert prompt via `facts_block`.
    let facts_block = Some(crate::repo::experts::facts::compute(entries).to_prompt_block());
    let ctx = RepoContext {
        entries: entries.to_vec(),
        stats,
        llm_configs: llm_configs.to_vec(),
        config,
        facts_block,
    };
    let mut scores = run_static_experts(&ctx).await;

    // Complete scan and local_analysis stages
    if let Some(ref map) = progress_map {
        if let Ok(mut p) = map.write() {
            if let Some(progress) = p.get_mut(review_id) {
                progress.complete_stage("scan");
                progress.complete_stage("local_analysis");
            }
        }
    }

    // ── 3-pass LLM architecture ──
    let mut dropped_findings: Vec<crate::team::verifier::DroppedFinding> = Vec::new();
    // True only when the verification pass below actually invokes
    // `verify_findings`; stays false when the pass is disabled or there were
    // no code_quality findings to hand to the verifier.
    let mut verification_ran = false;
    if !llm_configs.is_empty() {
        // ── Pass 1: Architecture Lead ──
        if let Some(ref map) = progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(review_id) {
                    progress.set_stage("llm_enhance", 0.1, "Pass 1: Architecture Lead".to_string());
                }
            }
        }
        let arch_lead = llm_experts::ArchitectureLead;
        match arch_lead.evaluate(&ctx, Some(llm_client)).await {
            Ok(s) => {
                tracing::info!("Architecture Lead scored {}", s.score);
                scores.push(s);
            }
            Err(e) => {
                // Results must land: a bare `tracing::warn!` here used to
                // drop the expert from the report entirely — total_experts
                // fell back to the 6 static ones and the total score was
                // normalised over 75 instead of 100, with no trace in the
                // JSON. Record an explicit, flagged fallback score instead.
                tracing::warn!("Architecture Lead failed: {:?}", e);
                eprintln!(
                    "WARN: LLM expert 'architecture' failed after all retries: {e:#}; \
                     recording explicit fallback score ({})",
                    experts::LLM_FALLBACK_SCORE
                );
                scores.push(ExpertScore {
                    expert_name: arch_lead.name().to_string(),
                    weight: arch_lead.weight(),
                    score: experts::LLM_FALLBACK_SCORE,
                    summary: format!("LLM architecture assessment unavailable: {e}"),
                    details: Vec::new(),
                    fallback: true,
                    evaluated_loc: Some(ctx.stats.total_loc as u64),
                    samples: None,
                });
            }
        }

        // ── Pass 2: Chunk-based CodeQuality ──
        let root = std::path::Path::new(local_path);
        let chunks = crate::repo::experts::chunk::chunk_by_module(entries, root);

        if let Some(ref map) = progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(review_id) {
                    progress.set_stage(
                        "llm_enhance",
                        0.4,
                        format!("Pass 2: CodeQuality × {} chunks", chunks.len()),
                    );
                }
            }
        }

        let max_concurrent = ctx
            .config
            .as_deref()
            .and_then(|c| c.max_concurrent_llm_calls)
            .unwrap_or(6);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let completed_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let total_chunks = chunks.len();
        let scanner_ref = &scanner;

        // Evaluate chunks concurrently (bounded by the semaphore), one future
        // per chunk. `join_all` polls them together and returns results in
        // input order, keeping chunk scores deterministic. Every chunk
        // returns an `ExpertScore` — a failed chunk yields an explicit,
        // flagged fallback, never a dropped `None`.
        let tasks: Vec<_> = chunks
            .iter()
            .enumerate()
            .map(|(i, chunk)| {
                let semaphore = semaphore.clone();
                let completed_count = completed_count.clone();
                let progress_map = progress_map.clone();
                let review_id = review_id.to_string();
                let llm_configs = llm_configs.to_vec();
                let config = ctx.config.clone();
                let facts_block = ctx.facts_block.clone();
                async move {
                    let _permit = match semaphore.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(e) => {
                            // Practically unreachable (the semaphore is never
                            // closed), but the chunk must still land.
                            tracing::warn!("Chunk {} semaphore acquire failed: {:?}", chunk.module, e);
                            return chunk_fallback_score(chunk, format!("scheduler unavailable: {e}"));
                        }
                    };
                    tracing::info!(
                        "CodeQuality chunk {}/{}: {} ({} files, {} LOC)",
                        i + 1,
                        total_chunks,
                        chunk.module,
                        chunk.files.len(),
                        chunk.total_loc
                    );

                    // Build per-chunk RepoContext
                    let chunk_entries: Vec<FileEntry> = entries
                        .iter()
                        .filter(|e| chunk.files.contains(&e.path))
                        .cloned()
                        .collect();
                    let chunk_stats = scanner_ref.compute_stats(&chunk_entries);
                    let chunk_ctx = RepoContext {
                        entries: chunk_entries,
                        stats: chunk_stats,
                        llm_configs,
                        config,
                        facts_block,
                    };

                    let result = match llm_experts::CodeQuality.evaluate(&chunk_ctx, Some(llm_client)).await {
                        Ok(s) => {
                            tracing::info!("Chunk {} scored {}", chunk.module, s.score);
                            s
                        }
                        Err(e) => {
                            // Same swallow fix as Pass 1: land the result,
                            // flag it, warn on stderr.
                            tracing::warn!("Chunk {} failed: {:?}", chunk.module, e);
                            eprintln!(
                                "WARN: LLM expert 'code_quality' chunk '{}' failed after all retries: {e:#}; \
                                 recording explicit fallback score ({})",
                                chunk.module,
                                experts::LLM_FALLBACK_SCORE
                            );
                            chunk_fallback_score(chunk, format!("LLM assessment unavailable: {e}"))
                        }
                    };

                    // Update progress per completed chunk
                    let done = completed_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if let Some(ref map) = progress_map {
                        if let Ok(mut p) = map.write() {
                            if let Some(progress) = p.get_mut(review_id.as_str()) {
                                let pct = 0.4 + done as f64 / total_chunks as f64 * 0.5;
                                progress.set_stage(
                                    "llm_enhance",
                                    pct,
                                    format!("Pass 2: CodeQuality chunk {}/{} ({})", done, total_chunks, chunk.module),
                                );
                            }
                        }
                    }

                    result
                }
            })
            .collect();

        let chunk_results: Vec<ExpertScore> = futures::future::join_all(tasks).await;
        scores.extend(chunk_results);

        // Complete llm_enhance stage
        if let Some(ref map) = progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(review_id) {
                    progress.complete_stage("llm_enhance");
                }
            }
        }

        // ── Optional verification pass (no-hunk mode) ──
        // Re-check the code_quality findings (mapped to the standard Finding
        // model) against the full file contents before Pass 3 consolidation.
        // Fail-open: `verify_findings` never aborts the review.
        let verification_enabled = ctx.config.as_deref().is_some_and(|c| c.report.verification_pass);
        if verification_enabled {
            let max_file_bytes = ctx
                .config
                .as_deref()
                .map(|c| c.report.verification_max_file_bytes)
                .unwrap_or(20000);
            let items: Vec<experts::ScoreItem> = scores
                .iter()
                .filter(|s| s.expert_name == "code_quality")
                .flat_map(|s| crate::repo::experts::aggregator::filter_noise(s.details.clone()))
                .collect();
            if !items.is_empty() {
                let findings: Vec<Finding> = items.iter().map(experts::score_item_to_finding).collect();
                let checked = findings.len();
                let mut reports = vec![ExpertReport {
                    expert_name: "code_quality".to_string(),
                    findings,
                    markdown: String::new(),
                    raw_llm_response: String::new(),
                    parse_error: None,
                    raw_dump_path: None,
                }];
                let dropped =
                    crate::team::verifier::verify_findings(&mut reports, &[], local_path, llm_configs, max_file_bytes)
                        .await;
                let kept = reports.into_iter().next().map(|r| r.findings).unwrap_or_default();
                tracing::info!(
                    "Verification pass: checked {} findings, dropped {}",
                    checked,
                    dropped.len()
                );
                if !dropped.is_empty() {
                    strip_dropped_from_scores(&mut scores, &kept);
                }
                dropped_findings = dropped;
                // The verifier genuinely ran, even if it dropped nothing. The
                // flag distinguishes this from the enabled-but-empty case so
                // the Markdown appendix says "ran" only when it did.
                verification_ran = true;
            }
        }
    }

    // ── Pass 3: Aggregator ──
    let aggregated = crate::repo::experts::aggregator::aggregate(scores, ctx.config.as_deref());
    let output = build_output_from_aggregated(&aggregated, &ctx.stats, dropped_findings, verification_ran, metadata);

    // Mark progress complete
    crate::progress::complete_repo_progress(progress_map.as_ref(), review_id);

    Ok(output)
}
