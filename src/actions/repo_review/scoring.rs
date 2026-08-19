use super::types::*;
use crate::models::*;
use crate::repo::experts::llm_experts;
use crate::repo::experts::{self, ExpertScore, RepoExpert};

/// Result of converting `ExpertScore` slices into their output representations.
pub(crate) struct ConvertedScores {
    pub(crate) expert_scores: Vec<ExpertScoreOutput>,
    pub(crate) lead_summary: Option<String>,
}

/// Shared: convert `ExpertScore` → `ExpertScoreOutput` and extract lead summary.
pub(crate) fn convert_scores(scores: &[ExpertScore]) -> ConvertedScores {
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
pub(crate) fn total_weight_f(expert_scores: &[ExpertScoreOutput]) -> f64 {
    expert_scores.iter().map(|s| s.weight as u32).sum::<u32>().max(1) as f64
}

/// Map a 0–100 score to the unified [`RiskLevel`] using the default
/// thresholds — the same bands the retired repo-local mapping used
/// (≤40 Critical, 41–60 High, 61–80 Medium, 81–90 LowMedium, 91+ Healthy).
pub(crate) fn repo_risk_level(score: u8) -> RiskLevel {
    crate::scoring::review::score_to_risk_level_with_config(score, &RiskThresholdConfig::default())
}

/// Build the per-expert score breakdown table rows.
pub(crate) fn build_score_breakdown(expert_scores: &[ExpertScoreOutput], divisor: f64) -> Vec<ScoreRow> {
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
pub(crate) fn build_risk_categories(expert_scores: &[ExpertScoreOutput]) -> Vec<RiskCategory> {
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
pub(crate) fn build_action_items(expert_scores: &[ExpertScoreOutput]) -> Vec<ActionItem> {
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
pub(crate) fn build_languages(stats: &crate::repo::RepoStats) -> Vec<String> {
    let mut lang_list: Vec<(&str, usize)> = stats.languages.iter().map(|(k, v)| (k.as_str(), v.files)).collect();
    lang_list.sort_by_key(|b| std::cmp::Reverse(b.1));
    lang_list
        .into_iter()
        .take(3)
        .map(|(name, _)| name.to_string())
        .collect()
}

/// Return the 5 risk areas with the lowest (worst) scores, sorted ascending.
pub(crate) fn pick_top_risks(risk_categories: &[RiskCategory]) -> Vec<(String, u8)> {
    let mut top: Vec<(String, u8)> = risk_categories.iter().map(|rc| (rc.area.clone(), rc.score)).collect();
    if top.len() > 5 {
        top.select_nth_unstable_by_key(4, |x| x.1);
        top.truncate(5);
    }
    top.sort_by_key(|(_, s)| *s);
    top
}

/// Build the explicit fallback score for a failed CodeQuality chunk.
///
/// The score lands in the report (flagged `fallback`) instead of vanishing,
/// so the aggregate keeps the code_quality weight and consumers can see
/// that this chunk was not genuinely assessed. `evaluated_loc` uses the
/// chunk's true LOC so the LOC-weighted merge still weights it correctly.
pub(crate) fn chunk_fallback_score(chunk: &crate::repo::experts::chunk::CodeChunk, reason: String) -> ExpertScore {
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
pub(crate) fn strip_dropped_from_scores(scores: &mut [ExpertScore], kept: &[Finding]) {
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
