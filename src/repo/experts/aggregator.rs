use std::collections::HashMap;

use super::{ExpertScore, ScoreItem};
use crate::models::{AppConfig, ExpertReport, Finding, RiskLevel};
use crate::team::lead_consolidator::ConsolidatorConfig;

// ─── AggregatedResult ────────────────────────

/// Cleaned, deduplicated output from the aggregator.
pub struct AggregatedResult {
    pub scores: Vec<ExpertScore>,
    pub all_findings: Vec<ScoreItem>,
    pub conclusion: ReportConclusion,
}

/// Conclusion summary produced by the aggregator.
#[derive(Debug, Clone)]
pub struct ReportConclusion {
    pub aggregated_score: u8,
    pub risk_level: RiskLevel,
    pub top_risks: Vec<(String, u8)>,
    pub recommendation: String,
}

// ─── Config ──────────────────────────────────

const MAX_FINDINGS: usize = 20;
const NOISE_PATTERNS: &[&str] = &[
    "No code snippet",
    "No code provided",
    "No code sample",
    "Unable to assess",
    "Unable to evaluate",
    "Unable to determine",
    "no code was provided",
    "no code snippets",
    "cannot assess",
];

// ─── Aggregator ──────────────────────────────

/// Deduplicate, filter noise, and merge chunk-level expert scores.
///
/// Multi-chunk (code_quality) findings are mapped to standard
/// [`Finding`]s and consolidated through the shared lead consolidator
/// (confidence downgrade + dedup + conflict detection), replacing the old
/// `merge_deduplicate` pass for LLM findings. `app_config` supplies the
/// consolidator's confidence thresholds; `None` uses its defaults.
pub fn aggregate(scores: Vec<ExpertScore>, app_config: Option<&AppConfig>) -> AggregatedResult {
    // Group by expert name
    let mut by_expert: HashMap<String, Vec<ExpertScore>> = HashMap::new();
    for s in scores {
        by_expert.entry(s.expert_name.clone()).or_default().push(s);
    }

    let mut merged_scores = Vec::new();
    let mut all_findings = Vec::new();

    for (name, mut group) in by_expert {
        if group.len() == 1 {
            // Single-call expert (static, architecture lead) — use as-is
            let mut s = group.swap_remove(0);
            s.details = filter_noise(s.details);
            all_findings.extend(s.details.iter().cloned());
            merged_scores.push(s);
        } else {
            // Multi-chunk expert (code_quality) — merge by LOC-weighted average
            let weight = group[0].weight;
            let mut total_weighted = 0u64;
            let mut total_loc = 0u64;
            let mut merged_details = Vec::new();

            for s in &group {
                // Prefer the chunk's REAL LOC (reported by the evaluating
                // expert). The findings-count heuristic below is only a
                // last resort for scores with no LOC attached — on its own
                // it inverts the weighting: more findings ⇒ bigger weight,
                // so the noisiest chunk would dominate the average.
                let chunk_loc = s.evaluated_loc.unwrap_or_else(|| estimate_loc(&s.details));
                total_weighted += (s.score as u64) * chunk_loc;
                total_loc += chunk_loc;
                merged_details.extend(filter_noise(s.details.clone()));
            }

            let avg_score = total_weighted
                .checked_div(total_loc)
                .map(|score| score as u8)
                .unwrap_or_else(|| {
                    let sum: u32 = group.iter().map(|s| s.score as u32).sum();
                    (sum / group.len() as u32) as u8
                });

            let mut merged_details = consolidate_chunk_findings(&name, merged_details, app_config);
            merged_details.truncate(MAX_FINDINGS);

            // Pick the best summary from the group
            let best_summary = group
                .iter()
                .filter(|s| !is_noise_summary(&s.summary))
                .max_by_key(|s| s.score)
                .map(|s| s.summary.clone())
                .unwrap_or_else(|| format!("{} chunks evaluated, avg score {}", group.len(), avg_score));

            all_findings.extend(merged_details.iter().cloned());
            // The merged score is an explicit fallback only when EVERY chunk
            // fell back — one genuine chunk assessment makes the aggregate a
            // genuine (if degraded) assessment. Partial fallbacks stay visible
            // through the per-chunk WARN lines emitted by the caller.
            let merged_fallback = group.iter().all(|s| s.fallback);
            // Sum the real chunk LOCs when known; `None` only if no chunk
            // reported one (then the per-chunk merge above used the
            // findings-count heuristic throughout).
            let merged_loc: Option<u64> = {
                let locs: Vec<u64> = group.iter().filter_map(|s| s.evaluated_loc).collect();
                if locs.is_empty() {
                    None
                } else {
                    Some(locs.iter().sum())
                }
            };
            // Concatenate raw sample scores across chunks when sampling was
            // active — they are the full evidence base of the merged score.
            let merged_samples: Option<Vec<u8>> = if group.iter().any(|s| s.samples.is_some()) {
                Some(
                    group
                        .iter()
                        .flat_map(|s| s.samples.clone().unwrap_or_default())
                        .collect(),
                )
            } else {
                None
            };
            merged_scores.push(ExpertScore {
                expert_name: name,
                weight,
                score: avg_score,
                summary: best_summary,
                details: merged_details,
                fallback: merged_fallback,
                evaluated_loc: merged_loc,
                samples: merged_samples,
            });
        }
    }

    // Global dedup across all findings
    all_findings = merge_deduplicate(all_findings);
    all_findings.truncate(MAX_FINDINGS);

    // Build conclusion
    let (aggregated_score, risk_level) = crate::repo::experts::weighted_total(&merged_scores);
    let mut top_risks: Vec<(String, u8)> = merged_scores.iter().map(|s| (s.expert_name.clone(), s.score)).collect();
    top_risks.sort_by_key(|(_, score)| *score);
    top_risks.truncate(5);

    let recommendation = if merged_scores.is_empty() {
        "Analysis incomplete. No expert data to evaluate.".to_string()
    } else if top_risks.is_empty() {
        "No significant issues found.".to_string()
    } else {
        let areas: Vec<&str> = top_risks.iter().map(|(n, _)| n.as_str()).collect();
        format!("Prioritize: {}.", areas.join(", "))
    };

    AggregatedResult {
        scores: merged_scores,
        all_findings,
        conclusion: ReportConclusion {
            aggregated_score,
            risk_level,
            top_risks,
            recommendation,
        },
    }
}

// ─── Helpers ─────────────────────────────────

fn severity_rank(s: &str) -> u8 {
    match s {
        "critical" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        "note" | "info" => 1,
        _ => 0,
    }
}

fn is_noise(text: &str) -> bool {
    NOISE_PATTERNS.iter().any(|p| text.contains(p))
}

fn is_noise_summary(text: &str) -> bool {
    text.contains("No code") || text.contains("no code")
}

/// Filter out empty and noise findings. Shared with the repo-review
/// verification path, which pre-filters before building standard findings.
pub(crate) fn filter_noise(details: Vec<ScoreItem>) -> Vec<ScoreItem> {
    let original_len = details.len();
    let result: Vec<ScoreItem> = details
        .into_iter()
        .filter(|d| {
            if d.message.trim().is_empty() {
                return false;
            }
            if is_noise(&d.message) {
                return false;
            }
            if let Some(ref evidence) = d.evidence {
                if is_noise(evidence) {
                    return false;
                }
            }
            true
        })
        .collect();
    let filtered = original_len - result.len();
    if filtered > 0 {
        tracing::debug!("filter_noise: removed {} of {} findings", filtered, original_len);
    }
    result
}

fn effort_rank(effort: &str) -> u8 {
    match effort {
        "large" => 4,
        "medium" => 3,
        "small" => 2,
        "trivial" => 1,
        _ => 0,
    }
}

/// Last-resort LOC estimate for a chunk score that carries no
/// `evaluated_loc` (legacy callers and any link in the chain that cannot
/// obtain the real LOC). Deliberately kept for those cases only: the
/// findings count is NOT a chunk-size proxy — a noisy chunk produces more
/// findings per LOC, so weighting by it over-weights exactly the worst
/// chunks. Real LOC, when available, always wins (see the merge loop).
fn estimate_loc(details: &[ScoreItem]) -> u64 {
    (details.len() * 200).max(100) as u64
}

/// Consolidate one multi-chunk expert's findings through the shared lead
/// consolidator: map to standard [`Finding`]s, wrap them in a single
/// [`ExpertReport`], and run [`ConsolidatorConfig::consolidate`] for
/// confidence downgrade, deduplication, and conflict detection. The result
/// is converted back to [`ScoreItem`]s and sorted by severity (desc), so
/// downstream rendering keeps the old ordering.
fn consolidate_chunk_findings(
    expert_name: &str,
    details: Vec<ScoreItem>,
    app_config: Option<&AppConfig>,
) -> Vec<ScoreItem> {
    let findings: Vec<Finding> = details.iter().map(super::score_item_to_finding).collect();
    let report = ExpertReport {
        expert_name: expert_name.to_string(),
        findings,
        markdown: String::new(),
        raw_llm_response: String::new(),
        parse_error: None,
        raw_dump_path: None,
    };
    let consolidator = match app_config {
        Some(c) => ConsolidatorConfig {
            min_confidence: c.report.min_confidence,
            drop_low_confidence: c.report.drop_low_confidence,
            ..Default::default()
        },
        None => ConsolidatorConfig::default(),
    };
    let result = consolidator.consolidate(&[report], None);
    if !result.conflicts.is_empty() {
        tracing::debug!(
            "{}: {} conflicts detected during consolidation",
            expert_name,
            result.conflicts.len()
        );
    }
    let mut items: Vec<ScoreItem> = result.findings.iter().map(super::finding_to_score_item).collect();
    items.sort_by_key(|d| std::cmp::Reverse(severity_rank(&d.severity)));
    items
}

/// Cross-expert dedup for the final `all_findings` list (static experts'
/// findings never go through the lead consolidator). Multi-chunk LLM
/// findings are consolidated earlier by [`consolidate_chunk_findings`].
fn merge_deduplicate(items: Vec<ScoreItem>) -> Vec<ScoreItem> {
    let mut merged: HashMap<(String, Option<String>), ScoreItem> = HashMap::new();
    for item in items {
        let key = (normalize(&item.message), item.file.clone());
        match merged.get_mut(&key) {
            Some(existing) => {
                // Merge: take higher severity, longer/richer text fields
                if severity_rank(&item.severity) > severity_rank(&existing.severity) {
                    existing.severity = item.severity;
                }
                if let Some(ref ev) = item.evidence {
                    if ev.len() > existing.evidence.as_ref().map_or(0, |e| e.len()) {
                        existing.evidence = Some(ev.clone());
                    }
                }
                if let Some(ref imp) = item.impact {
                    if imp.len() > existing.impact.as_ref().map_or(0, |i| i.len()) {
                        existing.impact = Some(imp.clone());
                    }
                }
                if let Some(ref rec) = item.recommendation {
                    if rec.len() > existing.recommendation.as_ref().map_or(0, |r| r.len()) {
                        existing.recommendation = Some(rec.clone());
                    }
                }
                if let Some(ref eff) = item.effort {
                    if effort_rank(eff) > effort_rank(existing.effort.as_deref().unwrap_or("")) {
                        existing.effort = Some(eff.clone());
                    }
                }
            }
            None => {
                merged.insert(key, item);
            }
        }
    }
    let mut result: Vec<ScoreItem> = merged.into_values().collect();
    result.sort_by_key(|d| std::cmp::Reverse(severity_rank(&d.severity)));
    result
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests;
