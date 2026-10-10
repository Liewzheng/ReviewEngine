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

/// Concurrency the LLM enhancement pass (pass 2) may use.
///
/// Goes through [`crate::team::orchestrator::concurrent_llm_calls`], the sink
/// guard that refuses a limit of 0 — a 0-permit semaphore leaves every chunk
/// task waiting forever, which is the silent-hang class the CLI clears a 0 cap
/// for. If the guard fires, the warning names this site.
fn llm_enhance_concurrency(config: Option<&AppConfig>) -> usize {
    crate::team::orchestrator::concurrent_llm_calls(config, "repo-review LLM enhancement")
}

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
                    ..Default::default()
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
        // Local-only path: no chunk scheduling, so the summary stays at
        // its `Default::default()` (total = 0). The renderer's `Chunks
        // Reviewed` appendix is gated on `total > 0` so this never shows.
        chunk_summary: ChunkSummary::default(),
        metadata,
    }
}

/// Build output from aggregated (deduplicated, filtered) scores.
///
/// The conclusion's `top_risks` (and the matching "Prioritize:" line in
/// `recommendation`) is sourced from the same `pick_top_risks(
/// &risk_categories)` the local path uses. `agg.conclusion.top_risks`
/// — the aggregator's own "5 lowest merged experts" list — is
/// fallback-filtered but **not** hygiene-filtered and would
/// reintroduce a 100/100 hygiene-only expert (e.g. `code_style` on an
/// unconfigured repo) into the Top Risks list while the same
/// report's Risk Map omits it. The aggregator's list is preserved
/// for whatever other consumer wants "5 lowest experts"; only the
/// report's conclusion switches sources so both risk surfaces stay
/// consistent.
///
/// `chunk_summary` (M26) is the per-chunk coverage signal the orchestrator
/// collected during Pass 2 — total / reviewed / not-reviewed counts plus
/// the per-failure module + LOC + reason. Renderers expose it in a
/// `Chunks Reviewed` appendix; consumers that ignore it (e.g. the
/// local-only path) get a `Default::default()` empty summary.
fn build_output_from_aggregated(
    agg: &crate::repo::experts::aggregator::AggregatedResult,
    stats: &crate::repo::RepoStats,
    dropped_findings: Vec<crate::team::verifier::DroppedFinding>,
    verification_ran: bool,
    metadata: ReviewMetadata,
    chunk_summary: ChunkSummary,
) -> RepoReviewOutput {
    let (health_score, risk_level) = experts::weighted_total(&agg.scores);
    let conv = convert_scores(&agg.scores);
    let divisor = total_weight_f(&conv.expert_scores);

    // Build all report sections from converted scores
    let score_breakdown = build_score_breakdown(&conv.expert_scores, divisor);
    let languages = build_languages(stats);
    let risk_categories = build_risk_categories(&conv.expert_scores);
    let action_items = build_action_items(&conv.expert_scores);

    // Conclusion Top Risks / Prioritize line MUST match the Risk Map.
    // Pick the same surface the local path uses (`pick_top_risks` over
    // the freshly-built `risk_categories`) so a hygiene-only expert or a
    // fallback expert that survived `aggregator::aggregate` cannot claim
    // a Top-Risk slot in one part of the report while being absent
    // from another.
    let top_risks = pick_top_risks(&risk_categories);
    let recommendation = build_conclusion_recommendation(&top_risks);

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
        top_risks,
        recommendation,
    };

    RepoReviewOutput {
        overview,
        expert_scores: conv.expert_scores,
        risk_categories,
        action_items,
        conclusion,
        dropped_findings,
        verification_ran,
        chunk_summary,
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
    // M26: per-chunk coverage so a report can say "X of Y chunks were not
    // reviewed by the model" instead of forcing the reader to grep the
    // per-expert `fallback: true` rows. Built up inside the chunk loop
    // and handed to `build_output_from_aggregated` below.
    let mut chunk_summary = ChunkSummary {
        total: 0,
        reviewed: 0,
        not_reviewed: 0,
        failures: Vec::new(),
    };
    // Counter Arcs declared here so they survive the `llm_configs.is_empty()`
    // branch — when no LLM configs are present, no chunks are scheduled,
    // the counters stay zero, and the orchestrator's post-loop read
    // confirms the empty summary it started with.
    let reviewed_chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let not_reviewed_chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let not_reviewed_reasons: Arc<std::sync::Mutex<Vec<ChunkFailure>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
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
                    ..Default::default()
                });
            }
        }

        // ── Pass 2: Chunk-based CodeQuality ──
        let root = std::path::Path::new(local_path);
        let chunks = crate::repo::experts::chunk::chunk_by_module(entries, root);
        // M26: the chunk-summary totals — populated by the chunk task
        // below. Total is known synchronously; reviewed/not_reviewed and the
        // per-failure list come back from the futures that ran each chunk.
        chunk_summary.total = chunks.len();

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

        let max_concurrent = llm_enhance_concurrency(ctx.config.as_deref());
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let completed_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let total_chunks = chunks.len();
        let scanner_ref = &scanner;
        // M26: per-chunk counters shared by the concurrent chunk tasks so
        // the final `chunk_summary` records the model's real coverage. The
        // counters are declared in the enclosing scope so the post-loop
        // read below can pull the final values even when the `if !llm_configs
        // .is_empty()` block was bypassed (local-only path: no chunks).

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
                let reviewed_chunks = reviewed_chunks.clone();
                let not_reviewed_chunks = not_reviewed_chunks.clone();
                let not_reviewed_reasons = not_reviewed_reasons.clone();
                // M26: chunk-task closure. Mutex poisoning here means a
                // sibling task panicked while holding it — propagate as a
                // panic rather than as a `Result` that hides the cause
                // chain. Mirrors the `#[allow(clippy::expect_used)]`
                // pattern used in the metrics / HTTP layer.
                #[allow(clippy::expect_used)]
                fn push_chunk_failure(mutex: &std::sync::Mutex<Vec<ChunkFailure>>, failure: ChunkFailure) {
                    mutex.lock().expect("chunk-failure mutex poisoned").push(failure);
                }
                async move {
                    let _permit = match semaphore.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(e) => {
                            // Practically unreachable (the semaphore is never
                            // closed), but the chunk must still land.
                            tracing::warn!("Chunk {} semaphore acquire failed: {:?}", chunk.module, e);
                            let score = chunk_fallback_score(chunk, format!("scheduler unavailable: {e}"));
                            not_reviewed_chunks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            push_chunk_failure(
                                &not_reviewed_reasons,
                                ChunkFailure {
                                    module: chunk.module.clone(),
                                    loc: chunk.total_loc as u64,
                                    reason: format!("scheduler unavailable: {e}"),
                                    // Scheduler failure is not a parser
                                    // classification — no reason code.
                                    reason_code: None,
                                },
                            );
                            return score;
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
                        Ok(mut s) => {
                            tracing::info!("Chunk {} scored {}", chunk.module, s.score);
                            if !s.fallback {
                                reviewed_chunks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            } else {
                                // The expert returned successfully but its
                                // score is itself a placeholder (parse failure,
                                // empty parse, etc.). Land it in the
                                // not-reviewed bucket — the model never
                                // produced findings for this chunk. The
                                // `chunk not reviewed` prefix in the
                                // `chunk_summary` table is the reader's
                                // primary signal; mirror it into the
                                // per-expert summary so the "Detailed
                                // Findings" surface carries the same word
                                // and grep -F 'chunk not reviewed' finds
                                // every unassessed chunk in either render.
                                not_reviewed_chunks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let prefixed_summary = if s.summary.starts_with(CHUNK_NOT_REVIEWED_PREFIX) {
                                    s.summary.clone()
                                } else {
                                    format!("{CHUNK_NOT_REVIEWED_PREFIX} — module {}: {}", chunk.module, s.summary)
                                };
                                // M38: surface the schema-broken reason
                                // code (when one exists) into
                                // `ChunkFailure.reason_code` so the
                                // `Chunks Reviewed` table renders it
                                // inline. `None` on every other failure
                                // class (LLM call error, M26 empty-retry
                                // exhausting) — those never reach
                                // `parse_expert_yaml` and carry no code.
                                let reason_code = s.reason_code.map(|c| c.as_str().to_string());
                                push_chunk_failure(
                                    &not_reviewed_reasons,
                                    ChunkFailure {
                                        module: chunk.module.clone(),
                                        loc: chunk.total_loc as u64,
                                        reason: format!("LLM response not actionable: {}", s.summary),
                                        reason_code,
                                    },
                                );
                                s.summary = prefixed_summary;
                            }
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
                            not_reviewed_chunks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            push_chunk_failure(
                                &not_reviewed_reasons,
                                ChunkFailure {
                                    module: chunk.module.clone(),
                                    loc: chunk.total_loc as u64,
                                    reason: format!("LLM assessment unavailable: {e}"),
                                    // LLM transport / chain failure never
                                    // reaches `parse_expert_yaml` — the
                                    // parser never saw a response, so no
                                    // reason code can be classified.
                                    reason_code: None,
                                },
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
                    llm_provider: None,
                    llm_model: None,
                    llm_fp: None,
                }];
                let dropped = crate::team::verifier::verify_findings(
                    &mut reports,
                    &[],
                    local_path,
                    llm_configs,
                    max_file_bytes,
                    // RENG-57: the repo-scan path has no review-scoped sample
                    // store behind it (its reports carry no provider identity
                    // either), so it records nothing.
                    None,
                )
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
    // M26: surface the chunk-level coverage the orchestrator collected so
    // the `Chunks Reviewed` appendix can show "X of Y chunks reviewed"
    // rather than forcing the reader to grep the per-expert `fallback: true`
    // rows. Counts are also published at INFO so the logs carry the same
    // signal the JSON / Markdown do.
    //
    // `expect` is the right shape here: a poisoned mutex means another
    // task panicked while holding it, and propagating that as a `?` would
    // hide the original panic in a chain that ends at "all chunks
    // failed". Mirror the `#[allow(clippy::expect_used)]` pattern the
    // metrics / HTTP layer use so this site stays consistent.
    #[allow(clippy::expect_used)]
    let collect_failures = || {
        Arc::try_unwrap(not_reviewed_reasons)
            .map(|m| m.into_inner().expect("chunk-failure mutex poisoned"))
            .unwrap_or_else(|arc| arc.lock().expect("chunk-failure mutex poisoned").clone())
    };
    let reviewed = reviewed_chunks.load(std::sync::atomic::Ordering::Relaxed);
    let not_reviewed = not_reviewed_chunks.load(std::sync::atomic::Ordering::Relaxed);
    let mut failures = collect_failures();
    // Keep `failures` in the same order as `chunks.iter().enumerate()` so
    // the report's table is deterministic — the orchestrator records them
    // in completion order, but the user reads the report top-to-bottom.
    failures.sort_by(|a, b| a.module.cmp(&b.module));
    chunk_summary.reviewed = reviewed;
    chunk_summary.not_reviewed = not_reviewed;
    chunk_summary.failures = failures;
    if chunk_summary.not_reviewed > 0 {
        tracing::warn!(
            reviewed = chunk_summary.reviewed,
            not_reviewed = chunk_summary.not_reviewed,
            total = chunk_summary.total,
            "CodeQuality chunks: some chunks were not reviewed by the model \
             (see report's Chunks Reviewed section for module-level reasons)"
        );
    } else if chunk_summary.total > 0 {
        tracing::info!(
            reviewed = chunk_summary.reviewed,
            total = chunk_summary.total,
            "CodeQuality chunks: all reviewed successfully"
        );
    }
    let output = build_output_from_aggregated(
        &aggregated,
        &ctx.stats,
        dropped_findings,
        verification_ran,
        metadata,
        chunk_summary,
    );

    // Mark progress complete
    crate::progress::complete_repo_progress(progress_map.as_ref(), review_id);

    Ok(output)
}
