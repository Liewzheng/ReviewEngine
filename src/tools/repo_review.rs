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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReportOverview {
    pub health_score: u8,
    pub risk_level: String,
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
    pub risk_label: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RiskCategory {
    pub area: String,
    pub score: u8,
    pub risk_level: String,
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
    pub risk_level: String,
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
                tracing::warn!("Expert {} failed: {:?}", e.name(), err);
                scores.push(ExpertScore {
                    expert_name: e.name().to_string(),
                    weight: e.weight(),
                    score: 50,
                    summary: format!("Evaluation failed: {err}"),
                    details: Vec::new(),
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
        expert_scores.push(ExpertScoreOutput {
            name: s.expert_name.clone(),
            weight: s.weight,
            score: s.score,
            summary: s.summary.clone(),
            details,
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

/// Build the per-expert score breakdown table rows.
fn build_score_breakdown(expert_scores: &[ExpertScoreOutput], divisor: f64) -> Vec<ScoreRow> {
    expert_scores
        .iter()
        .map(|s| ScoreRow {
            area: s.name.clone(),
            score: s.score,
            weight: s.weight,
            weighted_contrib: s.score as f64 * s.weight as f64 / divisor,
            risk_label: crate::repo::experts::score_to_risk_level(s.score).to_string(),
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
            risk_level: crate::repo::experts::score_to_risk_level(s.score).to_string(),
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

/// Build a RepoReviewOutput from expert scores for the local-only path.
fn build_output(scores: &[ExpertScore], stats: &crate::repo::RepoStats) -> RepoReviewOutput {
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
    }
}

/// Build output from aggregated (deduplicated, filtered) scores.
fn build_output_from_aggregated(
    agg: &crate::repo::experts::aggregator::AggregatedResult,
    stats: &crate::repo::RepoStats,
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
    }
}

/// Run a full local repository health review using the expert system (no LLM).
pub async fn run_local_repo_review(
    local_path: &str,
    progress_map: Option<ProgressMap>,
    review_id: &str,
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
        config: None,
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

    let result = build_output(&scores, &ctx.stats);

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
    let ctx = RepoContext {
        entries: entries.to_vec(),
        stats,
        llm_configs: llm_configs.to_vec(),
        config: None,
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
            Err(e) => tracing::warn!("Architecture Lead failed: {:?}", e),
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

        let semaphore = Arc::new(tokio::sync::Semaphore::new(6));
        let cq = llm_experts::CodeQuality;

        for (i, chunk) in chunks.iter().enumerate() {
            let _permit = semaphore.clone().acquire_owned().await?;
            tracing::info!(
                "CodeQuality chunk {}/{}: {} ({} files, {} LOC)",
                i + 1,
                chunks.len(),
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
            let chunk_stats = scanner.compute_stats(&chunk_entries);
            let chunk_ctx = RepoContext {
                entries: chunk_entries,
                stats: chunk_stats,
                llm_configs: llm_configs.to_vec(),
                config: None,
            };

            match cq.evaluate(&chunk_ctx, Some(llm_client)).await {
                Ok(s) => {
                    tracing::info!("Chunk {} scored {}", chunk.module, s.score);
                    scores.push(s);
                }
                Err(e) => tracing::warn!("Chunk {} failed: {:?}", chunk.module, e),
            }

            // Update progress per chunk
            if let Some(ref map) = progress_map {
                if let Ok(mut p) = map.write() {
                    if let Some(progress) = p.get_mut(review_id) {
                        let pct = 0.4 + (i + 1) as f64 / chunks.len() as f64 * 0.5;
                        progress.set_stage(
                            "llm_enhance",
                            pct,
                            format!(
                                "Pass 2: CodeQuality chunk {}/{} ({})",
                                i + 1,
                                chunks.len(),
                                chunk.module
                            ),
                        );
                    }
                }
            }
        }

        // Complete llm_enhance stage
        if let Some(ref map) = progress_map {
            if let Ok(mut p) = map.write() {
                if let Some(progress) = p.get_mut(review_id) {
                    progress.complete_stage("llm_enhance");
                }
            }
        }
    }

    // ── Pass 3: Aggregator ──
    let aggregated = crate::repo::experts::aggregator::aggregate(scores);
    let output = build_output_from_aggregated(&aggregated, &ctx.stats);

    // Mark progress complete
    crate::progress::complete_repo_progress(progress_map.as_ref(), review_id);

    Ok(output)
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
pub fn render_repo_review_output(output: &RepoReviewOutput, format: &str) -> Result<String> {
    Ok(match format {
        "json" => serde_json::to_string_pretty(output)?,
        _ => {
            let mut md = String::new();

            // ── Header ──
            md.push_str("# Repository Health Report\n\n");

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
                md.push_str(&format!(
                    "| {} | {}/100 | {}% | {:.1} | {} |\n",
                    row.area, row.score, row.weight, row.weighted_contrib, row.risk_label
                ));
            }
            let total_risk = crate::repo::experts::score_to_risk_level(output.overview.health_score);
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
                if s.details.is_empty() {
                    continue;
                }
                md.push_str(&format!(
                    "\n### {} ({}/100) — {} findings\n",
                    s.name,
                    s.score,
                    s.details.len()
                ));
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

            md.push_str("\n---\n*Report generated by Review Engine*\n");
            md = close_unclosed_code_fences(&md);
            md
        }
    })
}

#[allow(dead_code)]
fn parse_repo_review_response(response: &str) -> Result<RepoReviewOutput> {
    let cleaned = crate::output::parser::clean_yaml(response);
    if let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&cleaned) {
        let health_score = value["health_score"].as_u64().unwrap_or(50) as u8;
        let risk_level = value["risk_level"].as_str().unwrap_or("medium").to_string();
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
        });
    }
    let overview = ReportOverview {
        health_score: 50,
        risk_level: "medium".to_string(),
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
            risk_level: "medium".to_string(),
            top_risks: vec![],
            recommendation: String::new(),
        },
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
        }];
        let scores = vec![ExpertScore {
            expert_name: "security".to_string(),
            weight: 15,
            score: 60,
            summary: "Some issues".to_string(),
            details,
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
            },
            ExpertScore {
                expert_name: "code_quality".to_string(),
                weight: 10,
                score: 70,
                summary: "Quality report".to_string(),
                details: vec![],
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
                risk_level: "low".to_string(),
                finding_count: 1,
                findings: vec![],
            },
            RiskCategory {
                area: "b".to_string(),
                score: 60,
                risk_level: "medium".to_string(),
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
                risk_level: "low".to_string(),
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
                risk_level: "healthy".to_string(),
                finding_count: 0,
                findings: vec![],
            },
            RiskCategory {
                area: "b".to_string(),
                score: 40,
                risk_level: "critical".to_string(),
                finding_count: 3,
                findings: vec![],
            },
            RiskCategory {
                area: "c".to_string(),
                score: 70,
                risk_level: "medium".to_string(),
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
        }];
        let scores = vec![ExpertScore {
            expert_name: "test".to_string(),
            weight: 10,
            score: 70,
            summary: "".to_string(),
            details,
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
        assert_eq!(output.overview.risk_level, "low");
        assert_eq!(output.action_items.len(), 1);
        assert_eq!(output.action_items[0].message, "Add more tests");
    }
}
