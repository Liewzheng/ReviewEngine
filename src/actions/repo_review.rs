use crate::llm::client::LLMClient;
use crate::models::*;
use crate::output::markdown::{close_unclosed_code_fences, strip_markdown_fences};
use crate::progress::{ProgressMap, StageWeight};
use crate::repo::experts::llm_experts;
use crate::repo::experts::static_experts;
use crate::repo::experts::{self, ExpertScore, RepoContext, RepoExpert};
use crate::repo::{FileEntry, RepoScanner};
use anyhow::Result;
use std::sync::Arc;

/// Output from the repo-review command.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RepoReviewOutput {
    pub overview: ReportOverview,
    pub expert_scores: Vec<ExpertScoreOutput>,
    pub risk_categories: Vec<RiskCategory>,
    pub action_items: Vec<ActionItem>,
    pub conclusion: ReportConclusion,
    /// Code-quality findings dropped by the optional verification pass, with
    /// reasons. Empty when the pass was disabled or kept everything.
    #[serde(default)]
    pub dropped_findings: Vec<crate::team::verifier::DroppedFinding>,
    /// Whether the verification pass actually executed during this review.
    /// `false` when the pass was disabled, when there were no code_quality
    /// findings to verify, or on the local-only path. Drives the honest
    /// "ran / skipped" wording in the Markdown appendix.
    #[serde(default)]
    pub verification_ran: bool,
    /// Provenance of the run that produced this report: what was scanned
    /// (git SHA / tree hash / source), when, and with which model. Lets a
    /// consumer trace a report back to the exact snapshot it describes and
    /// decide whether two reports are comparable before contrasting scores.
    /// `#[serde(default)]` keeps JSON written before the field existed
    /// deserializable.
    #[serde(default)]
    pub metadata: ReviewMetadata,
}

/// Provenance metadata for a repo-review report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewMetadata {
    /// Git HEAD commit SHA of the scanned worktree; `None` when the scanned
    /// path is not a Git repository.
    pub head_sha: Option<String>,
    /// Stable hash of the scanned file tree: FNV-1a (64-bit, self-contained —
    /// no hashing dependency) over the sorted path+size+LOC records, rendered
    /// as 16 lowercase hex chars. Identical scan input always yields the same
    /// hash; adding / removing a file or changing a file's size or LOC
    /// changes it.
    pub tree_hash: String,
    /// RFC 3339 (UTC) timestamp of when the review ran.
    pub reviewed_at: String,
    /// Model identifier: comma-separated `provider/model` pairs when LLM
    /// experts ran, `"local-only"` on the static-only path.
    pub model: String,
    /// Effective `scoring.score_samples` sampling parameter for this run
    /// (1 = sampling disabled, each expert scored once).
    pub score_samples: usize,
    /// One-line description of what was scanned (the local workspace on
    /// disk).
    pub scan_source: String,
}

impl Default for ReviewMetadata {
    fn default() -> Self {
        Self {
            head_sha: None,
            tree_hash: String::new(),
            reviewed_at: String::new(),
            model: "local-only".to_string(),
            score_samples: 1,
            scan_source: String::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportOverview {
    pub health_score: u8,
    /// Unified risk level; serialized lowercase to keep the JSON contract
    /// (e.g. `"healthy"`, `"medium"`).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_level: RiskLevel,
    pub total_experts: usize,
    pub total_files: usize,
    pub total_loc: usize,
    pub languages: Vec<String>,
    pub lead_summary: Option<String>,
    pub score_breakdown: Vec<ScoreRow>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreRow {
    pub area: String,
    pub score: u8,
    pub weight: u8,
    pub weighted_contrib: f64,
    /// Unified risk level; serialized lowercase (see [`ReportOverview::risk_level`]).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_label: RiskLevel,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RiskCategory {
    pub area: String,
    pub score: u8,
    /// Unified risk level; serialized lowercase (see [`ReportOverview::risk_level`]).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_level: RiskLevel,
    pub finding_count: usize,
    pub findings: Vec<ScoreItemDetail>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionItem {
    pub area: String,
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub recommendation: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportConclusion {
    pub aggregated_score: u8,
    /// Unified risk level; serialized lowercase (see [`ReportOverview::risk_level`]).
    #[serde(with = "crate::models::risk_level_lowercase")]
    pub risk_level: RiskLevel,
    pub top_risks: Vec<(String, u8)>,
    pub recommendation: String,
}

/// A single finding rendered in the report output.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoreItemDetail {
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub evidence: Option<String>,
    pub impact: Option<String>,
    pub recommendation: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExpertScoreOutput {
    pub name: String,
    pub weight: u8,
    pub score: u8,
    pub summary: String,
    pub details: Vec<ScoreItemDetail>,
    /// `true` when `score` is an explicit fallback (LLM call failed after all
    /// retries, the response was unparseable, or a static expert errored) —
    /// not a genuine assessment. Fallback experts still occupy their weight
    /// in the total, so consumers need this flag to interpret the score.
    #[serde(default)]
    pub fallback: bool,
    /// Raw per-sample scores when `scoring.score_samples > 1` was active for
    /// this expert; `score` is their median. Absent when sampling was
    /// disabled (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<Vec<u8>>,
    /// Smallest sample score; present exactly when `samples` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_min: Option<u8>,
    /// Largest sample score; present exactly when `samples` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_max: Option<u8>,
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
                });
            }
        }
    }
    scores
}

/// Result of converting `ExpertScore` slices into their output representations.
struct ConvertedScores {
    expert_scores: Vec<ExpertScoreOutput>,
    lead_summary: Option<String>,
}

/// Shared: convert `ExpertScore` → `ExpertScoreOutput` and extract lead summary.
fn convert_scores(scores: &[ExpertScore]) -> ConvertedScores {
    let mut expert_scores = Vec::with_capacity(scores.len());
    let mut lead_summary = None;
    for s in scores {
        let details: Vec<ScoreItemDetail> = s
            .details
            .iter()
            .map(|d| ScoreItemDetail {
                severity: d.severity.clone(),
                message: d.message.clone(),
                file: d.file.clone(),
                evidence: d.evidence.clone(),
                impact: d.impact.clone(),
                recommendation: d.recommendation.clone(),
                effort: d.effort.clone(),
            })
            .collect();
        if s.expert_name == "architecture" {
            lead_summary = Some(s.summary.clone());
        }
        let (sample_min, sample_max) = match &s.samples {
            Some(samples) if !samples.is_empty() => (samples.iter().min().copied(), samples.iter().max().copied()),
            _ => (None, None),
        };
        expert_scores.push(ExpertScoreOutput {
            name: s.expert_name.clone(),
            weight: s.weight,
            score: s.score,
            summary: s.summary.clone(),
            details,
            fallback: s.fallback,
            samples: s.samples.clone(),
            sample_min,
            sample_max,
        });
    }
    ConvertedScores {
        expert_scores,
        lead_summary,
    }
}

/// Compute the normalised total weight used for score-breakdown contributions.
fn total_weight_f(expert_scores: &[ExpertScoreOutput]) -> f64 {
    expert_scores.iter().map(|s| s.weight as u32).sum::<u32>().max(1) as f64
}

/// Map a 0–100 score to the unified [`RiskLevel`] using the default
/// thresholds — the same bands the retired repo-local mapping used
/// (≤40 Critical, 41–60 High, 61–80 Medium, 81–90 LowMedium, 91+ Healthy).
fn repo_risk_level(score: u8) -> RiskLevel {
    crate::scoring::review::score_to_risk_level_with_config(score, &RiskThresholdConfig::default())
}

/// Build the per-expert score breakdown table rows.
fn build_score_breakdown(expert_scores: &[ExpertScoreOutput], divisor: f64) -> Vec<ScoreRow> {
    expert_scores
        .iter()
        .map(|s| ScoreRow {
            area: s.name.clone(),
            score: s.score,
            weight: s.weight,
            weighted_contrib: s.score as f64 * s.weight as f64 / divisor,
            risk_label: repo_risk_level(s.score),
        })
        .collect()
}

/// Build risk categories from expert scores, skipping experts with no findings.
fn build_risk_categories(expert_scores: &[ExpertScoreOutput]) -> Vec<RiskCategory> {
    expert_scores
        .iter()
        .filter(|s| !s.details.is_empty())
        .map(|s| RiskCategory {
            area: s.name.clone(),
            score: s.score,
            risk_level: repo_risk_level(s.score),
            finding_count: s.details.len(),
            findings: s.details.clone(),
        })
        .collect()
}

/// Build action items from expert scores, emitting entries for high/critical findings.
fn build_action_items(expert_scores: &[ExpertScoreOutput]) -> Vec<ActionItem> {
    expert_scores
        .iter()
        .flat_map(|s| {
            s.details.iter().filter_map(|d| {
                if d.severity == "high" || d.severity == "critical" {
                    Some(ActionItem {
                        area: s.name.clone(),
                        severity: d.severity.clone(),
                        message: d.message.clone(),
                        file: d.file.clone(),
                        recommendation: d.recommendation.clone().unwrap_or_default(),
                        effort: d.effort.clone(),
                    })
                } else {
                    None
                }
            })
        })
        .collect()
}

/// Build the top-3 language list sorted by file count descending.
fn build_languages(stats: &crate::repo::RepoStats) -> Vec<String> {
    let mut lang_list: Vec<(&str, usize)> = stats.languages.iter().map(|(k, v)| (k.as_str(), v.files)).collect();
    lang_list.sort_by_key(|b| std::cmp::Reverse(b.1));
    lang_list
        .into_iter()
        .take(3)
        .map(|(name, _)| name.to_string())
        .collect()
}

/// Return the 5 risk areas with the lowest (worst) scores, sorted ascending.
fn pick_top_risks(risk_categories: &[RiskCategory]) -> Vec<(String, u8)> {
    let mut top: Vec<(String, u8)> = risk_categories.iter().map(|rc| (rc.area.clone(), rc.score)).collect();
    if top.len() > 5 {
        top.select_nth_unstable_by_key(4, |x| x.1);
        top.truncate(5);
    }
    top.sort_by_key(|(_, s)| *s);
    top
}

// ── Provenance helpers ──
// Every helper here is fail-open: provenance annotates a report, it must
// never abort one.

/// Return the HEAD commit SHA of the Git repository at `root`, or `None`
/// when `root` is not a Git repository or `git rev-parse` fails.
fn git_head_sha(root: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(sha)
    }
}

/// Stable hash of the scanned file tree: FNV-1a (64-bit, self-contained — no
/// hashing dependency) over the sorted `path / size / LOC` records, rendered
/// as 16 lowercase hex chars. Paths are normalised relative to `root` so the
/// hash describes the tree itself, not where it happens to be checked out; a
/// file whose metadata is unreadable contributes size 0.
fn tree_hash(entries: &[FileEntry], root: &std::path::Path) -> String {
    let mut records: Vec<(String, u64, u64)> = entries
        .iter()
        .map(|e| {
            let rel = std::path::Path::new(&e.path)
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| e.path.clone());
            let size = std::fs::metadata(&e.path).map(|m| m.len()).unwrap_or(0);
            (rel, size, e.loc as u64)
        })
        .collect();
    records.sort();

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    };
    for (path, size, loc) in &records {
        feed(path.as_bytes());
        feed(&[0]);
        feed(&size.to_le_bytes());
        feed(&loc.to_le_bytes());
    }
    format!("{hash:016x}")
}

/// Model identifier for provenance: the `provider/model` pair(s) that scored
/// this run, or `"local-only"` when no LLM was involved.
fn model_label(llm_configs: &[LLMConfig]) -> String {
    if llm_configs.is_empty() {
        "local-only".to_string()
    } else {
        llm_configs
            .iter()
            .map(|c| format!("{}/{}", c.provider, c.model))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Assemble the provenance record for a review run.
fn build_metadata(
    local_path: &str,
    entries: &[FileEntry],
    llm_configs: &[LLMConfig],
    config: Option<&AppConfig>,
) -> ReviewMetadata {
    let root = std::path::Path::new(local_path);
    ReviewMetadata {
        head_sha: git_head_sha(root),
        tree_hash: tree_hash(entries, root),
        reviewed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        model: model_label(llm_configs),
        // The same effective sampling parameter the LLM experts resolve
        // (config value, floored at 1), so a report records whether `score`
        // is a single evaluation or a sample median.
        score_samples: config.map(|c| c.scoring.score_samples).unwrap_or(1).max(1),
        scan_source: format!("local workspace on disk ({local_path})"),
    }
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

/// Build the explicit fallback score for a failed CodeQuality chunk.
///
/// The score lands in the report (flagged `fallback`) instead of vanishing,
/// so the aggregate keeps the code_quality weight and consumers can see
/// that this chunk was not genuinely assessed. `evaluated_loc` uses the
/// chunk's true LOC so the LOC-weighted merge still weights it correctly.
fn chunk_fallback_score(chunk: &crate::repo::experts::chunk::CodeChunk, reason: String) -> ExpertScore {
    ExpertScore {
        expert_name: "code_quality".to_string(),
        weight: llm_experts::CodeQuality.weight(),
        score: experts::LLM_FALLBACK_SCORE,
        summary: format!("Module {}: {reason}", chunk.module),
        details: Vec::new(),
        fallback: true,
        evaluated_loc: Some(chunk.total_loc as u64),
        samples: None,
    }
}

/// Remove verification-dropped findings from the code_quality chunk scores.
///
/// `kept` holds the surviving standard findings, matched against the chunk
/// `ScoreItem`s by (file, title, severity, category), where `title`
/// corresponds to `ScoreItem.message` and `file` to `ScoreItem.file` (`None`
/// mapped to `""`). Items are mapped through `score_item_to_finding` so both
/// sides share the same severity/category normalisation; the extended key
/// keeps same-file/same-title findings with different severities distinct.
/// Matching is count-based, so identical findings reported by multiple
/// chunks survive independently. Items never sent to the verifier (e.g.
/// noise pre-filtered out of the standard findings) are also removed here;
/// the aggregator's `filter_noise` would drop them anyway.
fn strip_dropped_from_scores(scores: &mut [ExpertScore], kept: &[Finding]) {
    let mut remaining: std::collections::HashMap<(String, String, String, String), usize> =
        std::collections::HashMap::new();
    for f in kept {
        *remaining
            .entry((
                f.file.clone(),
                f.title.clone(),
                f.severity.to_string(),
                f.category.clone(),
            ))
            .or_insert(0) += 1;
    }
    for s in scores.iter_mut().filter(|s| s.expert_name == "code_quality") {
        s.details.retain(|d| {
            let f = experts::score_item_to_finding(d);
            let key = (f.file, f.title, f.severity.to_string(), f.category);
            match remaining.get_mut(&key) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            }
        });
    }
}

/// Render an expert-score detail line as markdown.
fn render_detail(d: &ScoreItemDetail) -> String {
    let mut buf = String::new();

    if d.message.trim().is_empty() {
        return buf;
    }
    buf.push_str(&format!("\n#### {} — {}\n", d.severity.to_uppercase(), d.message));

    if let Some(ref file) = d.file {
        buf.push_str(&format!("**File**: `{file}`\n"));
    }
    if let Some(ref evidence) = d.evidence {
        let evidence = strip_markdown_fences(evidence);
        if !evidence.is_empty() {
            let evidence = close_unclosed_code_fences(&evidence);
            buf.push_str(&format!("**Evidence**:\n```\n{evidence}\n```\n"));
        }
    }
    if let Some(ref impact) = d.impact {
        if !impact.is_empty() {
            buf.push_str(&format!("**Impact**: {impact}\n"));
        }
    }
    if let Some(ref rec) = d.recommendation {
        if !rec.is_empty() {
            buf.push_str(&format!("**Recommendation**: {rec}\n"));
        }
    }
    if let Some(ref effort) = d.effort {
        if !effort.is_empty() {
            buf.push_str(&format!("**Effort**: {effort}\n"));
        }
    }
    buf
}

/// Render a repo-review output in the requested format.
///
/// `verification_enabled` tells the Markdown renderer whether the finding
/// verification pass ran, so the "Dropped by verification" appendix can show
/// a run summary even when nothing was dropped (mirrors the review
/// pipeline's `format_output`).
pub fn render_repo_review_output(
    output: &RepoReviewOutput,
    format: &str,
    verification_enabled: bool,
) -> Result<String> {
    Ok(match format {
        "json" => serde_json::to_string_pretty(output)?,
        _ => {
            let mut md = String::new();

            // ── Header ──
            md.push_str("# Repository Health Report\n\n");

            // ── Provenance (compact, directly under the title) ──
            // Everything a consumer needs to trace this report back to the
            // exact snapshot that produced it.
            let m = &output.metadata;
            md.push_str("## Provenance\n");
            match m.head_sha.as_deref() {
                Some(sha) => md.push_str(&format!("- **Git HEAD**: `{sha}`\n")),
                None => md.push_str("- **Git HEAD**: (not a git repository)\n"),
            }
            md.push_str(&format!("- **Tree Hash**: `{}`\n", m.tree_hash));
            md.push_str(&format!("- **Reviewed At**: {}\n", m.reviewed_at));
            md.push_str(&format!("- **Model**: {}\n", m.model));
            md.push_str(&format!("- **Score Samples**: {}\n", m.score_samples));
            md.push_str(&format!("- **Scan Source**: {}\n", m.scan_source));
            md.push_str(
                "\n> Scores are a heuristic single-run / sampled assessment of this snapshot; \
                 compare across runs only against the same Git HEAD SHA and tree hash.\n\n",
            );

            // ── Overview (bullet list, no emoji) ──
            md.push_str("## Overview\n");
            md.push_str(&format!(
                "- **Health Score**: {}/100 ({})\n",
                output.overview.health_score, output.overview.risk_level
            ));
            md.push_str(&format!("- **Experts**: {}\n", output.overview.total_experts));
            md.push_str(&format!("- **Files**: {}\n", output.overview.total_files));
            md.push_str(&format!("- **LOC**: {}\n", output.overview.total_loc));
            let lang_str = output.overview.languages.join(", ");
            md.push_str(&format!("- **Languages**: {}\n\n", lang_str));

            // Score breakdown table
            md.push_str("### Score Breakdown\n");
            md.push_str("| Expert | Score | Weight | Contribution | Risk |\n");
            md.push_str("|--------|-------|--------|-------------|------|\n");
            let mut total_weighted = 0.0_f64;
            for row in &output.overview.score_breakdown {
                total_weighted += row.weighted_contrib;
                // A fallback row must not read as a genuine assessment.
                let fb = if output.expert_scores.iter().any(|s| s.name == row.area && s.fallback) {
                    " ⚠"
                } else {
                    ""
                };
                md.push_str(&format!(
                    "| {}{} | {}/100 | {}% | {:.1} | {} |\n",
                    row.area, fb, row.score, row.weight, row.weighted_contrib, row.risk_label
                ));
            }
            let total_risk = repo_risk_level(output.overview.health_score);
            md.push_str(&format!(
                "| **Total** | **{}/100** | **100%** | **{:.1}** | {} |\n\n",
                output.overview.health_score, total_weighted, total_risk
            ));

            if let Some(ref summary) = output.overview.lead_summary {
                md.push_str(&format!("> {}\n\n", summary));
            }

            md.push_str("---\n\n");

            // ── Detailed findings per expert ──
            md.push_str("## Detailed Findings\n");
            for s in &output.expert_scores {
                // Zero-finding experts still render their header + summary:
                // skipping them hid fallback scores (which carry no details)
                // and clean bills of health alike.
                let fb_marker = if s.fallback { " ⚠ fallback" } else { "" };
                md.push_str(&format!(
                    "\n### {} ({}/100){} — {} findings\n",
                    s.name,
                    s.score,
                    fb_marker,
                    s.details.len()
                ));
                if s.fallback {
                    md.push_str(
                        "> ⚠ **Fallback** — this score is a placeholder, not a genuine assessment; \
                         the summary below records why.\n\n",
                    );
                }
                md.push_str(&format!("**Summary**: {}\n\n", s.summary));
                for d in &s.details {
                    md.push_str(&render_detail(d));
                }
            }

            // ── Risk categories ──
            if !output.risk_categories.is_empty() {
                md.push_str("---\n\n## Risk Map\n");
                md.push_str("| Risk Level | Area | Score | Issues |\n");
                md.push_str("|-----------|------|-------|--------|\n");
                for rc in &output.risk_categories {
                    md.push_str(&format!(
                        "| {} | {} | {}/100 | {} |\n",
                        rc.risk_level, rc.area, rc.score, rc.finding_count
                    ));
                }
                md.push('\n');
            }

            // ── Action items ──
            if !output.action_items.is_empty() {
                md.push_str("---\n\n## Action Items\n");
                md.push_str("| # | Area | Severity | Issue | Recommendation | Effort |\n");
                md.push_str("|---|------|----------|-------|---------------|--------|\n");
                for (i, item) in output.action_items.iter().enumerate() {
                    let eff = item.effort.as_deref().unwrap_or("—");
                    md.push_str(&format!(
                        "| {} | {} | {} | {} | {} | {} |\n",
                        i + 1,
                        item.area,
                        item.severity,
                        item.message,
                        item.recommendation,
                        eff,
                    ));
                }
                md.push('\n');
            }

            // ── Conclusion ──
            md.push_str("---\n\n## Conclusion\n");
            md.push_str(&format!(
                "**Aggregated Score**: {}/100 (**{}**)\n\n",
                output.conclusion.aggregated_score, output.conclusion.risk_level
            ));
            md.push_str("**Top Risks**:\n");
            if output.conclusion.top_risks.is_empty() {
                md.push_str("None\n");
            } else {
                for (i, (area, score)) in output.conclusion.top_risks.iter().enumerate() {
                    md.push_str(&format!("{}. **{}** ({}/100)\n", i + 1, area, score));
                }
            }
            md.push('\n');
            md.push_str(&format!("**Recommendation**: {}\n", output.conclusion.recommendation));

            // ── Verification appendix ──
            // checked = surviving code_quality findings + dropped ones, the
            // same "kept + dropped" accounting the review pipeline uses. The
            // explicit ran-state keeps the wording honest: "skipped" when the
            // pass was enabled but had no code_quality findings to verify,
            // "ran" only when `verify_findings` actually executed.
            let checked = output
                .expert_scores
                .iter()
                .filter(|s| s.name == "code_quality")
                .map(|s| s.details.len())
                .sum::<usize>()
                + output.dropped_findings.len();
            let appendix = crate::output::renderer::render_dropped_findings_appendix_with_state(
                &output.dropped_findings,
                verification_enabled,
                output.verification_ran,
                checked,
            );
            if !appendix.is_empty() {
                md.push_str("\n---\n\n");
                md.push_str(&appendix);
            }

            md.push_str("\n---\n*Report generated by Review Engine*\n");
            md = close_unclosed_code_fences(&md);
            md
        }
    })
}

#[cfg(test)]
fn parse_repo_review_response(response: &str) -> Result<RepoReviewOutput> {
    let cleaned = crate::output::parser::clean_yaml(response);
    if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        let health_score = value["health_score"].as_u64().unwrap_or(50) as u8;
        let risk_level: RiskLevel = value["risk_level"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(RiskLevel::Medium);
        let lead_summary = value["summary"].as_str().map(|s| s.to_string());

        let overview = ReportOverview {
            health_score,
            risk_level: risk_level.clone(),
            total_experts: 0,
            total_files: 0,
            total_loc: 0,
            languages: vec![],
            lead_summary,
            score_breakdown: vec![],
        };

        let old_action_items: Vec<String> = value["action_items"]
            .as_sequence()
            .map(|seq| seq.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let action_items: Vec<ActionItem> = old_action_items
            .into_iter()
            .map(|msg| ActionItem {
                area: "".to_string(),
                severity: "medium".to_string(),
                message: msg,
                file: None,
                recommendation: String::new(),
                effort: None,
            })
            .collect();

        let conclusion = ReportConclusion {
            aggregated_score: health_score,
            risk_level,
            top_risks: vec![],
            recommendation: String::new(),
        };

        return Ok(RepoReviewOutput {
            overview,
            expert_scores: vec![],
            risk_categories: vec![],
            action_items,
            conclusion,
            dropped_findings: vec![],
            verification_ran: false,
            metadata: ReviewMetadata::default(),
        });
    }
    let overview = ReportOverview {
        health_score: 50,
        risk_level: RiskLevel::Medium,
        total_experts: 0,
        total_files: 0,
        total_loc: 0,
        languages: vec![],
        lead_summary: Some(response.to_string()),
        score_breakdown: vec![],
    };
    Ok(RepoReviewOutput {
        overview,
        expert_scores: vec![],
        risk_categories: vec![],
        action_items: vec![],
        conclusion: ReportConclusion {
            aggregated_score: 50,
            risk_level: RiskLevel::Medium,
            top_risks: vec![],
            recommendation: String::new(),
        },
        dropped_findings: vec![],
        verification_ran: false,
        metadata: ReviewMetadata::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::experts::ScoreItem;

    // ── convert_scores ──

    #[test]
    fn test_convert_scores_empty() {
        let conv = convert_scores(&[]);
        assert!(conv.expert_scores.is_empty());
        assert!(conv.lead_summary.is_none());
    }

    #[test]
    fn test_convert_scores_architecture_extracts_lead_summary() {
        let scores = vec![ExpertScore {
            expert_name: "architecture".to_string(),
            weight: 15,
            score: 80,
            summary: "Architecture looks good".to_string(),
            details: vec![],
            fallback: false,
            evaluated_loc: None,
            samples: None,
        }];
        let conv = convert_scores(&scores);
        assert_eq!(conv.expert_scores.len(), 1);
        assert_eq!(conv.lead_summary.as_deref(), Some("Architecture looks good"));
        assert_eq!(conv.expert_scores[0].name, "architecture");
        assert_eq!(conv.expert_scores[0].score, 80);
    }

    #[test]
    fn test_convert_scores_non_architecture_lead_summary_none() {
        let scores = vec![ExpertScore {
            expert_name: "code_quality".to_string(),
            weight: 10,
            score: 70,
            summary: "Good code".to_string(),
            details: vec![],
            fallback: false,
            evaluated_loc: None,
            samples: None,
        }];
        let conv = convert_scores(&scores);
        assert!(conv.lead_summary.is_none());
        assert_eq!(conv.expert_scores[0].name, "code_quality");
    }

    #[test]
    fn test_convert_scores_preserves_details() {
        let details = vec![ScoreItem {
            severity: "high".to_string(),
            message: "Issue".to_string(),
            file: Some("src/main.rs".to_string()),
            evidence: Some("bad code".to_string()),
            impact: Some("breaks things".to_string()),
            recommendation: Some("fix it".to_string()),
            effort: Some("medium".to_string()),
            confidence: None,
        }];
        let scores = vec![ExpertScore {
            expert_name: "security".to_string(),
            weight: 15,
            score: 60,
            summary: "Some issues".to_string(),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        }];
        let conv = convert_scores(&scores);
        assert_eq!(conv.expert_scores[0].details.len(), 1);
        let d = &conv.expert_scores[0].details[0];
        assert_eq!(d.severity, "high");
        assert_eq!(d.message, "Issue");
        assert_eq!(d.file.as_deref(), Some("src/main.rs"));
        assert_eq!(d.evidence.as_deref(), Some("bad code"));
        assert_eq!(d.impact.as_deref(), Some("breaks things"));
        assert_eq!(d.recommendation.as_deref(), Some("fix it"));
        assert_eq!(d.effort.as_deref(), Some("medium"));
    }

    #[test]
    fn test_convert_scores_multiple_experts() {
        let scores = vec![
            ExpertScore {
                expert_name: "architecture".to_string(),
                weight: 15,
                score: 85,
                summary: "Lead summary".to_string(),
                details: vec![],
                fallback: false,
                evaluated_loc: None,
                samples: None,
            },
            ExpertScore {
                expert_name: "code_quality".to_string(),
                weight: 10,
                score: 70,
                summary: "Quality report".to_string(),
                details: vec![],
                fallback: false,
                evaluated_loc: None,
                samples: None,
            },
        ];
        let conv = convert_scores(&scores);
        assert_eq!(conv.expert_scores.len(), 2);
        assert_eq!(conv.lead_summary.as_deref(), Some("Lead summary"));
        assert_eq!(conv.expert_scores[0].name, "architecture");
        assert_eq!(conv.expert_scores[1].name, "code_quality");
    }

    // ── pick_top_risks ──

    #[test]
    fn test_pick_top_risks_empty() {
        assert!(pick_top_risks(&[]).is_empty());
    }

    #[test]
    fn test_pick_top_risks_less_than_5() {
        let cats = vec![
            RiskCategory {
                area: "a".to_string(),
                score: 80,
                risk_level: RiskLevel::Low,
                finding_count: 1,
                findings: vec![],
            },
            RiskCategory {
                area: "b".to_string(),
                score: 60,
                risk_level: RiskLevel::Medium,
                finding_count: 1,
                findings: vec![],
            },
        ];
        let top = pick_top_risks(&cats);
        assert_eq!(top.len(), 2);
        // lowest score first (highest risk)
        assert_eq!(top[0].0, "b");
        assert_eq!(top[0].1, 60);
    }

    #[test]
    fn test_pick_top_risks_truncates_to_5() {
        let cats: Vec<RiskCategory> = (0..10)
            .map(|i| RiskCategory {
                area: format!("e{i}"),
                score: 50 + i as u8,
                risk_level: RiskLevel::Low,
                finding_count: 1,
                findings: vec![],
            })
            .collect();
        let top = pick_top_risks(&cats);
        assert_eq!(top.len(), 5);
        // first entry has lowest score
        assert_eq!(top[0].0, "e0");
        assert_eq!(top[4].0, "e4");
    }

    #[test]
    fn test_pick_top_risks_sorted_ascending() {
        let cats = vec![
            RiskCategory {
                area: "a".to_string(),
                score: 90,
                risk_level: RiskLevel::Healthy,
                finding_count: 0,
                findings: vec![],
            },
            RiskCategory {
                area: "b".to_string(),
                score: 40,
                risk_level: RiskLevel::Critical,
                finding_count: 3,
                findings: vec![],
            },
            RiskCategory {
                area: "c".to_string(),
                score: 70,
                risk_level: RiskLevel::Medium,
                finding_count: 2,
                findings: vec![],
            },
        ];
        let top = pick_top_risks(&cats);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].0, "b"); // 40 (critical - lowest score)
        assert_eq!(top[1].0, "c"); // 70 (medium)
        assert_eq!(top[2].0, "a"); // 90 (healthy - highest score)
    }

    // ── build_languages ──

    #[test]
    fn test_build_languages_top_3() {
        let mut languages = std::collections::HashMap::new();
        languages.insert("Rust".to_string(), crate::repo::LanguageStats { files: 50, loc: 5000 });
        languages.insert(
            "Python".to_string(),
            crate::repo::LanguageStats { files: 30, loc: 3000 },
        );
        languages.insert("Shell".to_string(), crate::repo::LanguageStats { files: 20, loc: 500 });
        languages.insert("Config".to_string(), crate::repo::LanguageStats { files: 10, loc: 200 });
        let stats = crate::repo::RepoStats {
            total_files: 110,
            total_loc: 8700,
            languages,
            large_files: vec![],
            generated_files: 0,
            binary_files: 0,
        };
        let langs = build_languages(&stats);
        assert_eq!(langs.len(), 3);
        assert_eq!(langs[0], "Rust");
        assert_eq!(langs[1], "Python");
        assert_eq!(langs[2], "Shell");
    }

    #[test]
    fn test_build_languages_less_than_3() {
        let mut languages = std::collections::HashMap::new();
        languages.insert("Rust".to_string(), crate::repo::LanguageStats { files: 10, loc: 1000 });
        let stats = crate::repo::RepoStats {
            total_files: 10,
            total_loc: 1000,
            languages,
            large_files: vec![],
            generated_files: 0,
            binary_files: 0,
        };
        let langs = build_languages(&stats);
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0], "Rust");
    }

    #[test]
    fn test_build_languages_empty() {
        let stats = crate::repo::RepoStats {
            total_files: 0,
            total_loc: 0,
            languages: std::collections::HashMap::new(),
            large_files: vec![],
            generated_files: 0,
            binary_files: 0,
        };
        let langs = build_languages(&stats);
        assert!(langs.is_empty());
    }

    // ── convert_scores edge cases ──

    #[test]
    fn test_convert_scores_optional_fields_none() {
        let details = vec![ScoreItem {
            severity: "high".to_string(),
            message: "Issue".to_string(),
            file: None,
            evidence: None,
            impact: None,
            recommendation: None,
            effort: None,
            confidence: None,
        }];
        let scores = vec![ExpertScore {
            expert_name: "test".to_string(),
            weight: 10,
            score: 70,
            summary: "".to_string(),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        }];
        let conv = convert_scores(&scores);
        let d = &conv.expert_scores[0].details[0];
        assert!(d.file.is_none());
        assert!(d.evidence.is_none());
        assert!(d.impact.is_none());
        assert!(d.recommendation.is_none());
        assert!(d.effort.is_none());
    }

    // ── build_score_breakdown ──

    #[test]
    fn test_build_score_breakdown_empty() {
        assert!(build_score_breakdown(&[], 1.0).is_empty());
    }

    #[test]
    fn test_build_score_breakdown_weighted_contrib() {
        let scores = vec![score_output("a", 80, 60), score_output("b", 60, 40)];
        let rows = build_score_breakdown(&scores, 100.0);
        assert_eq!(rows.len(), 2);
        // a: 80 * 60 / 100 = 48.0
        // b: 60 * 40 / 100 = 24.0
        assert!((rows[0].weighted_contrib - 48.0).abs() < 0.01);
        assert!((rows[1].weighted_contrib - 24.0).abs() < 0.01);
    }

    // ── build_risk_categories ──

    #[test]
    fn test_build_risk_categories_filters_empty_details() {
        let s = vec![
            score_output("a", 80, 10), // no details
            score_output("b", 60, 10), // no details
        ];
        assert!(build_risk_categories(&s).is_empty());
    }

    // ── build_action_items ──

    #[test]
    fn test_build_action_items_filters_by_severity() {
        let detail = |s: &str, m: &str| ScoreItemDetail {
            severity: s.to_string(),
            message: m.to_string(),
            file: None,
            evidence: None,
            impact: None,
            recommendation: None,
            effort: None,
        };
        let expert = ExpertScoreOutput {
            name: "test".to_string(),
            weight: 10,
            score: 70,
            summary: "".to_string(),
            details: vec![
                detail("critical", "Critical issue"),
                detail("high", "High issue"),
                detail("medium", "Medium issue"),
                detail("low", "Low issue"),
                detail("info", "Info note"),
            ],
            fallback: false,
            samples: None,
            sample_min: None,
            sample_max: None,
        };
        let items = build_action_items(&[expert]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].message, "Critical issue");
        assert_eq!(items[1].message, "High issue");
    }

    // ── render_detail ──

    #[test]
    fn test_render_detail_strips_fenced_evidence() {
        let detail = ScoreItemDetail {
            severity: "high".to_string(),
            message: "Unsafe pattern".to_string(),
            file: None,
            evidence: Some("```rust\nunsafe { *ptr }\n```".to_string()),
            impact: None,
            recommendation: None,
            effort: None,
        };
        let rendered = render_detail(&detail);
        // The outer fence should be stripped and re-wrapped in a single ``` block.
        assert!(rendered.contains("**Evidence**:\n```\nunsafe { *ptr }\n```\n"));
        // Should not contain nested fences from the original LLM output.
        assert!(!rendered.contains("```rust"));
        assert!(!rendered.contains("```\n```"));
    }

    fn score_output(name: &str, score: u8, weight: u8) -> ExpertScoreOutput {
        ExpertScoreOutput {
            name: name.to_string(),
            weight,
            score,
            summary: String::new(),
            details: vec![],
            fallback: false,
            samples: None,
            sample_min: None,
            sample_max: None,
        }
    }

    // ── parse_repo_review_response ──

    #[test]
    fn test_parse_repo_review_yaml() {
        let yaml = r#"
health_score: 75
risk_level: "low"
summary: "Project is healthy"
action_items:
  - "Add more tests"
"#;
        let output = parse_repo_review_response(yaml).unwrap();
        assert_eq!(output.overview.health_score, 75);
        assert_eq!(output.overview.risk_level, RiskLevel::Low);
        assert_eq!(output.action_items.len(), 1);
        assert_eq!(output.action_items[0].message, "Add more tests");
    }

    // ── dropped_findings serde compatibility ──

    fn minimal_output() -> RepoReviewOutput {
        RepoReviewOutput {
            overview: ReportOverview {
                health_score: 80,
                risk_level: RiskLevel::Low,
                total_experts: 1,
                total_files: 10,
                total_loc: 1000,
                languages: vec![],
                lead_summary: None,
                score_breakdown: vec![],
            },
            expert_scores: vec![],
            risk_categories: vec![],
            action_items: vec![],
            conclusion: ReportConclusion {
                aggregated_score: 80,
                risk_level: RiskLevel::Low,
                top_risks: vec![],
                recommendation: String::new(),
            },
            dropped_findings: vec![],
            verification_ran: false,
            metadata: ReviewMetadata::default(),
        }
    }

    fn make_dropped_finding(title: &str) -> crate::team::verifier::DroppedFinding {
        crate::team::verifier::DroppedFinding {
            finding: Finding {
                file: "src/a.rs".to_string(),
                line: None,
                line_end: None,
                severity: Severity::High,
                confidence: 7,
                category: "quality".to_string(),
                title: title.to_string(),
                summary: String::new(),
                evidence: String::new(),
                impact: String::new(),
                recommendation: String::new(),
                effort: Effort::Small,
                expert_name: "code_quality".to_string(),
                expert_role: "Code Quality".to_string(),
                agrees_with: vec![],
                references: vec![],
            },
            reason: "Disproven by file content".to_string(),
        }
    }

    #[test]
    fn test_repo_review_output_deserializes_without_dropped_findings() {
        // JSON produced before the field existed must still deserialize.
        let mut value = serde_json::to_value(minimal_output()).unwrap();
        value.as_object_mut().unwrap().remove("dropped_findings");
        let de: RepoReviewOutput = serde_json::from_value(value).unwrap();
        assert!(de.dropped_findings.is_empty());
    }

    #[test]
    fn test_repo_review_output_dropped_findings_roundtrip() {
        let mut output = minimal_output();
        output.dropped_findings.push(make_dropped_finding("False alarm"));
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("dropped_findings"));
        let de: RepoReviewOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(de.dropped_findings.len(), 1);
        assert_eq!(de.dropped_findings[0].finding.title, "False alarm");
        assert_eq!(de.dropped_findings[0].reason, "Disproven by file content");
    }

    // ── risk_level JSON contract ──

    #[test]
    fn test_risk_level_serializes_lowercase() {
        // The repo-review JSON contract uses lowercase risk labels; the
        // unified RiskLevel enum must keep that exact form.
        let output = minimal_output();
        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(value["overview"]["risk_level"], serde_json::json!("low"));
        assert_eq!(value["conclusion"]["risk_level"], serde_json::json!("low"));
    }

    #[test]
    fn test_risk_level_deserializes_legacy_lowercase() {
        // Every label the retired repo-side mapping could emit must still parse.
        for (label, expected) in [
            ("critical", RiskLevel::Critical),
            ("high", RiskLevel::High),
            ("medium", RiskLevel::Medium),
            ("low", RiskLevel::Low),
            ("healthy", RiskLevel::Healthy),
            ("low-medium", RiskLevel::LowMedium),
        ] {
            let value = serde_json::json!({
                "overview": {
                    "health_score": 80,
                    "risk_level": label,
                    "total_experts": 0,
                    "total_files": 0,
                    "total_loc": 0,
                    "languages": [],
                    "lead_summary": null,
                    "score_breakdown": []
                },
                "expert_scores": [],
                "risk_categories": [],
                "action_items": [],
                "conclusion": {
                    "aggregated_score": 80,
                    "risk_level": label,
                    "top_risks": [],
                    "recommendation": ""
                }
            });
            let de: RepoReviewOutput = serde_json::from_value(value).unwrap();
            assert_eq!(de.overview.risk_level, expected);
            assert_eq!(de.conclusion.risk_level, expected);
        }
    }

    // ── strip_dropped_from_scores ──

    fn chunk_score(details: Vec<ScoreItem>) -> ExpertScore {
        ExpertScore {
            expert_name: "code_quality".to_string(),
            weight: 10,
            score: 70,
            summary: String::new(),
            details,
            fallback: false,
            evaluated_loc: None,
            samples: None,
        }
    }

    fn item(message: &str, file: Option<&str>) -> ScoreItem {
        ScoreItem {
            severity: "high".to_string(),
            message: message.to_string(),
            file: file.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn test_strip_dropped_from_scores_removes_only_dropped() {
        let mut scores = vec![
            chunk_score(vec![item("Kept", Some("src/a.rs")), item("Dropped", Some("src/a.rs"))]),
            chunk_score(vec![item("Kept too", Some("src/b.rs"))]),
            ExpertScore {
                expert_name: "security".to_string(),
                weight: 15,
                score: 80,
                summary: String::new(),
                details: vec![item("Static finding", Some("src/c.rs"))],
                fallback: false,
                evaluated_loc: None,
                samples: None,
            },
        ];
        let kept: Vec<Finding> = vec![
            experts::score_item_to_finding(&item("Kept", Some("src/a.rs"))),
            experts::score_item_to_finding(&item("Kept too", Some("src/b.rs"))),
        ];
        strip_dropped_from_scores(&mut scores, &kept);
        assert_eq!(scores[0].details.len(), 1);
        assert_eq!(scores[0].details[0].message, "Kept");
        assert_eq!(scores[1].details.len(), 1);
        // Non-code_quality experts are untouched.
        assert_eq!(scores[2].details.len(), 1);
        assert_eq!(scores[2].details[0].message, "Static finding");
    }

    #[test]
    fn test_strip_dropped_from_scores_count_based_matching() {
        // Identical findings in two chunks: one surviving copy keeps one.
        let mut scores = vec![
            chunk_score(vec![item("Same", Some("src/a.rs"))]),
            chunk_score(vec![item("Same", Some("src/a.rs"))]),
        ];
        let kept: Vec<Finding> = vec![experts::score_item_to_finding(&item("Same", Some("src/a.rs")))];
        strip_dropped_from_scores(&mut scores, &kept);
        let total: usize = scores.iter().map(|s| s.details.len()).sum();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_strip_dropped_from_scores_distinguishes_severity() {
        // Same file + title but different severity: keeping the high-severity
        // copy must not retain the low-severity one (listed first, so a plain
        // (file, title) count match would keep the wrong item).
        let low = ScoreItem {
            severity: "low".to_string(),
            ..item("Same", Some("src/a.rs"))
        };
        let high = item("Same", Some("src/a.rs"));
        let mut scores = vec![chunk_score(vec![low, high.clone()])];
        let kept: Vec<Finding> = vec![experts::score_item_to_finding(&high)];
        strip_dropped_from_scores(&mut scores, &kept);
        assert_eq!(scores[0].details.len(), 1);
        assert_eq!(scores[0].details[0].severity, "high");
    }

    // ── verification appendix in markdown ──

    #[test]
    fn test_render_markdown_appends_verification_appendix() {
        let mut output = minimal_output();
        output.verification_ran = true;
        output.dropped_findings.push(make_dropped_finding("False alarm"));
        let md = render_repo_review_output(&output, "markdown", true).unwrap();
        assert!(md.contains("## Dropped by verification"));
        assert!(md.contains("False alarm"));
        assert!(md.contains("1 dropped"));
    }

    #[test]
    fn test_render_markdown_verification_ran_no_drops() {
        let mut output = minimal_output();
        output.verification_ran = true;
        let md = render_repo_review_output(&output, "markdown", true).unwrap();
        assert!(md.contains("## Dropped by verification"));
        assert!(md.contains("no findings were dropped (0 checked)"));
    }

    #[test]
    fn test_render_markdown_verification_enabled_but_skipped() {
        // The pass is configured on but the review had no code_quality
        // findings to verify: the appendix must say "skipped", never "ran".
        let output = minimal_output();
        let md = render_repo_review_output(&output, "markdown", true).unwrap();
        assert!(md.contains("## Dropped by verification"));
        assert!(md.contains("Verification pass skipped"));
        assert!(!md.contains("Verification pass ran"));
        assert!(!md.contains("no findings were dropped"));
    }

    #[test]
    fn test_render_markdown_verification_disabled_no_appendix() {
        let output = minimal_output();
        let md = render_repo_review_output(&output, "markdown", false).unwrap();
        assert!(!md.contains("Dropped by verification"));
    }

    // ── LLM failure fallback (regression: silently dropped LLM scores) ──

    #[test]
    fn test_convert_scores_propagates_fallback_flag() {
        let scores = vec![ExpertScore {
            expert_name: "architecture".to_string(),
            weight: 15,
            score: experts::LLM_FALLBACK_SCORE,
            summary: "LLM architecture assessment unavailable: boom".to_string(),
            details: vec![],
            fallback: true,
            evaluated_loc: Some(1234),
            samples: None,
        }];
        let conv = convert_scores(&scores);
        assert!(conv.expert_scores[0].fallback);
        // A flagged architecture fallback still feeds the lead summary slot —
        // the report must show *why* there is no genuine assessment.
        assert!(conv.lead_summary.as_deref().unwrap().contains("unavailable"));
        assert_eq!(conv.expert_scores[0].samples, None);
    }

    #[test]
    fn test_convert_scores_propagates_samples_min_max() {
        let scores = vec![ExpertScore {
            expert_name: "code_quality".to_string(),
            weight: 10,
            score: 80,
            summary: "s".to_string(),
            details: vec![],
            fallback: false,
            evaluated_loc: Some(500),
            samples: Some(vec![70, 90, 80]),
        }];
        let conv = convert_scores(&scores);
        assert_eq!(conv.expert_scores[0].samples, Some(vec![70, 90, 80]));
        assert_eq!(conv.expert_scores[0].sample_min, Some(70));
        assert_eq!(conv.expert_scores[0].sample_max, Some(90));
        // The sampling evidence serializes into the JSON contract; absent
        // when sampling was disabled.
        let json = serde_json::to_value(&conv.expert_scores[0]).unwrap();
        assert_eq!(json["sample_min"], serde_json::json!(70));
        assert_eq!(json["sample_max"], serde_json::json!(90));
        let plain = score_output("x", 80, 10);
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("samples").is_none());
        assert!(json.get("sample_min").is_none());
        assert_eq!(json["fallback"], serde_json::json!(false));
    }

    /// Every LLM call failing (unreachable endpoint) must still produce
    /// architecture + code_quality entries, flagged `fallback`, with the
    /// weight sum back at 100 — the old code dropped them on the floor and
    /// normalised over the 75 static-only weight.
    ///
    /// The endpoint is `127.0.0.1:1` (connection refused): fails fast,
    /// offline, non-retriable. `start_paused` makes any retry backoff
    /// instantaneous should the error text ever classify as retriable.
    #[tokio::test(start_paused = true)]
    async fn test_run_repo_review_llm_failure_lands_visible_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..4 {
            std::fs::write(src.join(format!("m{i}.rs")), format!("pub fn f{i}() -> u8 {{ {i} }}\n")).unwrap();
        }
        let root_str = root.to_str().unwrap();
        let scanner = crate::repo::RepoScanner::new(root_str);
        let entries = scanner.scan().unwrap();
        assert_eq!(entries.len(), 4);

        let llm_configs = vec![LLMConfig {
            provider: "openai".to_string(),
            model: "unreachable-model".to_string(),
            api_key: "sk-test".to_string(),
            api_base: "http://127.0.0.1:1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        }];
        let client = crate::llm::client::LLMClient::new();

        let output = run_repo_review(&client, &llm_configs, root_str, &entries, None, "test-rr", None)
            .await
            .unwrap();

        // 6 static + architecture + code_quality: nothing swallowed.
        assert_eq!(output.overview.total_experts, 8);
        let weight_sum: u32 = output.expert_scores.iter().map(|s| s.weight as u32).sum();
        assert_eq!(weight_sum, 100);

        let arch = output
            .expert_scores
            .iter()
            .find(|s| s.name == "architecture")
            .expect("architecture expert must appear in the report");
        assert!(arch.fallback, "failed LLM call must be flagged as fallback");
        assert!(arch.summary.contains("unavailable"));

        let cq = output
            .expert_scores
            .iter()
            .find(|s| s.name == "code_quality")
            .expect("code_quality expert must appear in the report");
        assert!(cq.fallback, "failed LLM call must be flagged as fallback");

        // Lead summary slot carries the fallback reason, not `None`.
        let lead = output
            .overview
            .lead_summary
            .as_deref()
            .expect("lead_summary must not be swallowed");
        assert!(lead.contains("unavailable"));

        // The fallback flags survive JSON serialisation — the contract a
        // consumer uses to tell whether LLM experts genuinely scored.
        let json = serde_json::to_value(&output).unwrap();
        let arch_json = json["expert_scores"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "architecture")
            .unwrap()
            .clone();
        assert_eq!(arch_json["fallback"], serde_json::json!(true));
        assert_eq!(arch_json["score"], serde_json::json!(experts::LLM_FALLBACK_SCORE));
    }

    // ── provenance metadata ──

    fn init_git_repo(path: &std::path::Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test User"]);
    }

    fn commit_all(path: &std::path::Path) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .expect("git command failed to run");
            assert!(status.success(), "git command {:?} failed", args);
        };
        run(&["add", "-A"]);
        run(&["commit", "-m", "test commit"]);
    }

    fn file_entry(path: &str, loc: usize) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: "Rust".to_string(),
            loc,
            is_binary: false,
            is_generated: false,
        }
    }

    #[test]
    fn test_tree_hash_deterministic_for_same_input() {
        let entries = vec![file_entry("repo/src/a.rs", 10), file_entry("repo/src/b.rs", 20)];
        let root = std::path::Path::new("repo");
        let h1 = tree_hash(&entries, root);
        let h2 = tree_hash(&entries, root);
        assert_eq!(h1, h2, "same input must hash identically");
        assert_eq!(h1.len(), 16, "16 lowercase hex chars: {h1}");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));

        // Record order must not matter (records are sorted before hashing).
        let mut shuffled = entries.clone();
        shuffled.swap(0, 1);
        assert_eq!(tree_hash(&shuffled, root), h1);

        // Paths are normalised relative to the root: the same tree checked
        // out elsewhere hashes alike — the hash describes the tree, not the
        // checkout location.
        let relocated: Vec<FileEntry> = entries
            .iter()
            .map(|e| FileEntry {
                path: format!("/elsewhere/{}", e.path.trim_start_matches("repo/")),
                ..e.clone()
            })
            .collect();
        assert_eq!(tree_hash(&relocated, std::path::Path::new("/elsewhere")), h1);
    }

    #[test]
    fn test_tree_hash_changes_with_input() {
        let root = std::path::Path::new("repo");
        let base = vec![file_entry("repo/src/a.rs", 10), file_entry("repo/src/b.rs", 20)];
        let h = tree_hash(&base, root);

        let loc_changed = vec![file_entry("repo/src/a.rs", 11), file_entry("repo/src/b.rs", 20)];
        assert_ne!(tree_hash(&loc_changed, root), h, "a LOC change must change the hash");

        let mut file_added = base.clone();
        file_added.push(file_entry("repo/src/c.rs", 5));
        assert_ne!(tree_hash(&file_added, root), h, "an added file must change the hash");

        let renamed = vec![file_entry("repo/src/z.rs", 10), file_entry("repo/src/b.rs", 20)];
        assert_ne!(tree_hash(&renamed, root), h, "a rename must change the hash");
    }

    #[test]
    fn test_tree_hash_size_sensitive_on_disk() {
        // Sizes come from the filesystem: a content change that keeps the LOC
        // count identical must still change the hash.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = root.join("a.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        let entry = || file_entry(&file.to_string_lossy(), 1);
        let h1 = tree_hash(&[entry()], root);
        assert_eq!(
            tree_hash(&[entry()], root),
            h1,
            "unchanged disk state must hash identically"
        );

        std::fs::write(&file, "fn aa() {}\n").unwrap(); // same 1 line, 2 bytes larger
        assert_ne!(
            tree_hash(&[entry()], root),
            h1,
            "a size-only change must change the hash"
        );
    }

    #[test]
    fn test_git_head_sha_reads_temp_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
        commit_all(dir.path());
        let sha = git_head_sha(dir.path()).expect("a git repo must yield its HEAD sha");
        assert_eq!(sha.len(), 40, "full SHA-1: {sha}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_git_head_sha_none_for_non_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(git_head_sha(dir.path()).is_none());
    }

    #[tokio::test]
    async fn test_run_local_repo_review_populates_metadata() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
        commit_all(dir.path());
        let expected_sha = git_head_sha(dir.path()).unwrap();
        let root = dir.path().to_str().unwrap();

        let output = run_local_repo_review(root, None, "test-meta", None).await.unwrap();
        let m = &output.metadata;
        assert_eq!(m.head_sha.as_deref(), Some(expected_sha.as_str()));
        assert_eq!(m.tree_hash.len(), 16);
        assert!(m.tree_hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(m.model, "local-only");
        assert_eq!(m.score_samples, 1);
        assert!(m.scan_source.contains("local workspace on disk"), "{}", m.scan_source);
        assert!(m.scan_source.contains(root), "{}", m.scan_source);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&m.reviewed_at).is_ok(),
            "reviewed_at must be RFC 3339: {}",
            m.reviewed_at
        );

        // The metadata lands in the JSON contract in the existing snake_case style.
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["metadata"]["head_sha"], serde_json::json!(expected_sha));
        assert_eq!(json["metadata"]["model"], serde_json::json!("local-only"));
        assert_eq!(json["metadata"]["score_samples"], serde_json::json!(1));
        assert!(json["metadata"]["tree_hash"].is_string());
        assert!(json["metadata"]["reviewed_at"].is_string());
        assert!(json["metadata"]["scan_source"].is_string());
    }

    #[tokio::test]
    async fn test_run_local_repo_review_metadata_non_git_and_score_samples() {
        // Non-git root: head_sha stays empty; a configured sampling parameter
        // is recorded as-is.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn f() -> u8 { 1 }\n").unwrap();
        let config = AppConfig {
            project: None,
            report: Default::default(),
            review_experts: Default::default(),
            commands: Default::default(),
            scoring: ScoringConfig {
                score_samples: 5,
                ..Default::default()
            },
            llm: vec![],
            max_team_size: None,
            max_concurrent_llm_calls: None,
            output_dir: String::new(),
            diff: Default::default(),
            rate_limit: Default::default(),
            languages: Default::default(),
        };
        let output = run_local_repo_review(
            dir.path().to_str().unwrap(),
            None,
            "test-meta-cfg",
            Some(std::sync::Arc::new(config)),
        )
        .await
        .unwrap();
        assert!(output.metadata.head_sha.is_none());
        assert_eq!(output.metadata.score_samples, 5);
    }

    #[test]
    fn test_repo_review_output_deserializes_without_metadata() {
        // JSON produced before the field existed must still deserialize.
        let mut value = serde_json::to_value(minimal_output()).unwrap();
        value.as_object_mut().unwrap().remove("metadata");
        let de: RepoReviewOutput = serde_json::from_value(value).unwrap();
        assert!(de.metadata.head_sha.is_none());
        assert_eq!(de.metadata.model, "local-only");
        assert_eq!(de.metadata.score_samples, 1);
    }

    // ── markdown: provenance section ──

    #[test]
    fn test_render_markdown_provenance_section() {
        let mut output = minimal_output();
        output.metadata = ReviewMetadata {
            head_sha: Some("abc123def".to_string()),
            tree_hash: "0123456789abcdef".to_string(),
            reviewed_at: "2026-01-02T03:04:05Z".to_string(),
            model: "openai/gpt-5".to_string(),
            score_samples: 3,
            scan_source: "local workspace on disk (/repo)".to_string(),
        };
        let md = render_repo_review_output(&output, "markdown", false).unwrap();
        // Compact section directly under the title, before the Overview.
        let title = md.find("# Repository Health Report").unwrap();
        let prov = md.find("## Provenance").unwrap();
        let overview = md.find("## Overview").unwrap();
        assert!(title < prov && prov < overview);
        assert!(md.contains("- **Git HEAD**: `abc123def`"));
        assert!(md.contains("- **Tree Hash**: `0123456789abcdef`"));
        assert!(md.contains("- **Reviewed At**: 2026-01-02T03:04:05Z"));
        assert!(md.contains("- **Model**: openai/gpt-5"));
        assert!(md.contains("- **Score Samples**: 3"));
        assert!(md.contains("- **Scan Source**: local workspace on disk (/repo)"));
        // Score-nature note: heuristic single-run / sampled assessment,
        // same SHA + tree hash as the baseline for cross-run comparison.
        assert!(md.contains("heuristic single-run / sampled assessment"));
        assert!(md.contains("same Git HEAD SHA and tree hash"));
    }

    #[test]
    fn test_render_markdown_provenance_non_git() {
        let output = minimal_output(); // default metadata: no git repo
        let md = render_repo_review_output(&output, "markdown", false).unwrap();
        assert!(md.contains("- **Git HEAD**: (not a git repository)"));
    }

    // ── markdown: zero-finding experts & fallback annotation ──

    fn expert_output(name: &str, score: u8, summary: &str, fallback: bool) -> ExpertScoreOutput {
        ExpertScoreOutput {
            name: name.to_string(),
            weight: 15,
            score,
            summary: summary.to_string(),
            details: vec![],
            fallback,
            samples: None,
            sample_min: None,
            sample_max: None,
        }
    }

    #[test]
    fn test_render_markdown_renders_zero_finding_expert_summary() {
        let mut output = minimal_output();
        output
            .expert_scores
            .push(expert_output("documentation", 95, "Docs are comprehensive", false));
        let md = render_repo_review_output(&output, "markdown", false).unwrap();
        // The whole section used to be skipped; the summary line must render.
        assert!(md.contains("### documentation (95/100) — 0 findings"), "{md}");
        assert!(md.contains("**Summary**: Docs are comprehensive"), "{md}");
        // A clean expert is NOT marked as fallback.
        assert!(!md.contains("### documentation (95/100) ⚠ fallback"), "{md}");
    }

    #[test]
    fn test_render_markdown_marks_fallback_experts() {
        let mut output = minimal_output();
        output.overview.score_breakdown.push(ScoreRow {
            area: "architecture".to_string(),
            score: experts::LLM_FALLBACK_SCORE,
            weight: 15,
            weighted_contrib: 10.5,
            risk_label: repo_risk_level(experts::LLM_FALLBACK_SCORE),
        });
        output.expert_scores.push(expert_output(
            "architecture",
            experts::LLM_FALLBACK_SCORE,
            "LLM architecture assessment unavailable: boom",
            true,
        ));
        let md = render_repo_review_output(&output, "markdown", false).unwrap();
        // Section header + callout carry the ⚠ fallback marker, and the
        // reason stays visible in the summary line.
        let header = format!(
            "### architecture ({}/100) ⚠ fallback — 0 findings",
            experts::LLM_FALLBACK_SCORE
        );
        assert!(md.contains(&header), "{md}");
        assert!(md.contains("> ⚠ **Fallback**"), "{md}");
        assert!(
            md.contains("**Summary**: LLM architecture assessment unavailable: boom"),
            "{md}"
        );
        // The score-breakdown table marks the row too — a placeholder score
        // must not read as a genuine assessment anywhere in the report.
        let row = format!("| architecture ⚠ | {}/100 |", experts::LLM_FALLBACK_SCORE);
        assert!(md.contains(&row), "{md}");
    }
}
