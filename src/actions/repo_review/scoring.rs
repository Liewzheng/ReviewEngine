use super::types::*;
use crate::models::*;
use crate::repo::experts::llm_experts;
use crate::repo::experts::{self, ExpertScore, RepoExpert};

/// Tag emitted in a `ScoreItem.message` to mark the item as a
/// **hygiene** reminder (e.g. a missing style config) rather than a
/// code-quality defect. Producers in `src/repo/experts/static_experts/`
/// prepend it; the report-layer filters below rely on it to keep
/// hygiene-only experts out of the Top-Risks list.
const HYGIENE_TAG: &str = "[hygiene]";

/// `true` when the given finding is a hygiene reminder rather than a
/// genuine code-quality or risk observation.
///
/// Only the explicit `[hygiene]` tag qualifies — producers (currently
/// `code_style` for missing-config reminders) prepend the tag when the
/// detail is a hygiene-style reminder that should not climb the risk
/// ranking. A bare `note` severity is **not** sufficient any more:
///
/// - The RENG-115 round-3 logic also accepted `severity == "note"` as
///   a hygiene signal, but the broadened predicate swallowed
///   `test_coverage` on sirena — its only emitted detail was the
///   `severity: note` "No CI test step detected" line, and the 70/100
///   expert vanished from Top Risks and Risk Map while staying in the
///   breakdown table (a 14.0 contribution with no visible rank).
/// - Hygiene filter now matches the producer's actual contract:
///   only `code_style` (and any future expert that opts in by
///   tagging its reminders `[hygiene]`) gets filtered. Everything
///   else surfaces normally; the report's Top Risks and Risk Map
///   agree on the same set of areas.
pub(crate) fn is_hygiene_finding(detail: &ScoreItemDetail) -> bool {
    detail.message.starts_with(HYGIENE_TAG)
}

/// `true` when **every** detail on the expert is a hygiene reminder
/// (or the expert has no details). Used to gate `risk_categories` /
/// `top_risks` against ranking hygiene-only experts as risks.
pub(crate) fn expert_is_hygiene_only(details: &[ScoreItemDetail]) -> bool {
    !details.is_empty() && details.iter().all(is_hygiene_finding)
}

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
            // M18-W2: surface the by-expert sample-fallback rate on the
            // serialized report so dashboards can aggregate over time.
            // Both fields are `None` on the single-shot path
            // (`score_samples = 1`), and the renderer skips the row when
            // `samples` is absent — so absent == no sampling == no rate.
            samples_attempted: s.samples_attempted,
            samples_failed: s.samples_failed,
        });
    }
    ConvertedScores {
        expert_scores,
        lead_summary,
    }
}

/// Compute the normalised total weight used for score-breakdown contributions.
///
/// Fallback experts (`s.fallback == true`) hold a placeholder score
/// (the `LLM_FALLBACK_SCORE` constant, typically 70) that
/// [`crate::repo::experts::weighted_total`] already filters out of
/// the headline aggregate. The breakdown table must use the same
/// denominator, otherwise the per-expert `weighted_contrib` rows sum
/// to a number that differs from `health_score` whenever a fallback
/// expert is present — a visible inconsistency in the report. Sum
/// only non-fallback weights here so `build_score_breakdown`'s rows
/// tally to the headline.
pub(crate) fn total_weight_f(expert_scores: &[ExpertScoreOutput]) -> f64 {
    expert_scores
        .iter()
        .filter(|s| !s.fallback)
        .map(|s| s.weight as u32)
        .sum::<u32>()
        .max(1) as f64
}

/// Map a 0–100 score to the unified [`RiskLevel`] using the default
/// thresholds — the same bands the retired repo-local mapping used
/// (≤40 Critical, 41–60 High, 61–80 Medium, 81–90 LowMedium, 91+ Healthy).
pub(crate) fn repo_risk_level(score: u8) -> RiskLevel {
    crate::scoring::review::score_to_risk_level_with_config(score, &RiskThresholdConfig::default())
}

/// Build the per-expert score breakdown table rows.
///
/// Fallback experts (placeholder scores, `fallback = true`) are
/// excluded — the per-row `weighted_contrib` would otherwise inflate
/// the breakdown sum above the headline `health_score`, and the row
/// itself would be misleading (a fake 70/100 placeholder would show
/// up as a real contribution). The headline's matching denominator is
/// [`total_weight_f`], which already sums only non-fallback weights.
pub(crate) fn build_score_breakdown(expert_scores: &[ExpertScoreOutput], divisor: f64) -> Vec<ScoreRow> {
    expert_scores
        .iter()
        .filter(|s| !s.fallback)
        .map(|s| ScoreRow {
            area: s.name.clone(),
            score: s.score,
            weight: s.weight,
            weighted_contrib: s.score as f64 * s.weight as f64 / divisor,
            risk_label: repo_risk_level(s.score),
        })
        .collect()
}

/// Build risk categories from expert scores, skipping experts with no findings
/// **and** experts whose findings are all hygiene reminders.
///
/// A hygiene-only expert (e.g. `code_style` on an unconfigured repo:
/// 100/100 with `note`-severity `[hygiene]` "missing config" findings)
/// is not a real risk — pushing it into `risk_categories` would let
/// `pick_top_risks` rank it on the report's Top Risks list and pull
/// the reader's attention away from actual issues. Skip the whole
/// expert at this layer; the hygiene findings stay on the per-expert
/// detail list and remain visible in the report's hygiene section.
pub(crate) fn build_risk_categories(expert_scores: &[ExpertScoreOutput]) -> Vec<RiskCategory> {
    expert_scores
        .iter()
        .filter(|s| !s.details.is_empty())
        .filter(|s| !expert_is_hygiene_only(&s.details))
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

/// Build the "Prioritize: …" recommendation line that pairs with
/// `pick_top_risks`'s output. Mirrors the wording the aggregator
/// uses (`aggregator::aggregate`) so the two paths sound identical
/// even though the underlying list comes from different sources — the
/// report assembler's conclusion list (`build_output_from_aggregated`)
/// now agrees with the local path, and this helper makes sure the
/// text agrees too.
///
/// Empty input → "No significant issues found." so the report reads
/// cleanly when the report has nothing to flag.
pub(crate) fn build_conclusion_recommendation(top_risks: &[(String, u8)]) -> String {
    if top_risks.is_empty() {
        "No significant issues found.".to_string()
    } else {
        let areas: Vec<&str> = top_risks.iter().map(|(n, _)| n.as_str()).collect();
        format!("Prioritize: {}.", areas.join(", "))
    }
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
///
/// `risk_categories` is the output of [`build_risk_categories`]; this
/// function trusts that input has already filtered hygiene-only
/// experts out (see [`build_risk_categories`]'s doc comment).
pub(crate) fn pick_top_risks(risk_categories: &[RiskCategory]) -> Vec<(String, u8)> {
    let mut top: Vec<(String, u8)> = risk_categories
        .iter()
        // Defence-in-depth: a future call site that hands in raw
        // categories without going through build_risk_categories must
        // still skip hygiene-only entries. The criterion mirrors
        // `build_risk_categories`: every detail is a hygiene reminder.
        .filter(|rc| !expert_is_hygiene_only(&rc.findings))
        .map(|rc| (rc.area.clone(), rc.score))
        .collect();
    if top.len() > 5 {
        top.select_nth_unstable_by_key(4, |x| x.1);
        top.truncate(5);
    }
    top.sort_by_key(|(_, s)| *s);
    top
}

/// Marker prefix in a fallback `ExpertScore::summary` so the renderer and
/// any consumer reading the JSON contract can tell at a glance that the
/// chunk was not reviewed by the model — a stronger signal than the
/// `fallback: true` flag (which the LLM_FALLBACK_SCORE placeholder also
/// carries for unrelated expert failures). The chunk-failed surface MUST
/// stay readable: M26 added the `disable_thinking` retry for the empty-
/// completion path, but a chunk whose LLM call hits a credential / 4xx /
/// non-empty error still lands here, and the reader needs the word
/// "not reviewed" in the prose to know that the chunk's real code was
/// never assessed by the model.
pub(crate) const CHUNK_NOT_REVIEWED_PREFIX: &str = "chunk not reviewed";

/// Build the explicit fallback score for a failed CodeQuality chunk.
///
/// The score lands in the report (flagged `fallback`) instead of vanishing,
/// so the aggregate keeps the code_quality weight and consumers can see
/// that this chunk was not genuinely assessed. `evaluated_loc` uses the
/// chunk's true LOC so the LOC-weighted merge still weights it correctly.
///
/// The summary carries the `chunk not reviewed` prefix
/// ([`CHUNK_NOT_REVIEWED_PREFIX`]) so the renderer's "Detailed Findings"
/// surface can detect this specific failure class — the report
/// assembler's `Chunks Reviewed` appendix (M26) keys off it to enumerate
/// the chunks that never reached a real model assessment.
pub(crate) fn chunk_fallback_score(chunk: &crate::repo::experts::chunk::CodeChunk, reason: String) -> ExpertScore {
    ExpertScore {
        expert_name: "code_quality".to_string(),
        weight: llm_experts::CodeQuality.weight(),
        score: experts::LLM_FALLBACK_SCORE,
        summary: format!("{CHUNK_NOT_REVIEWED_PREFIX} — module {}: {reason}", chunk.module),
        details: Vec::new(),
        fallback: true,
        evaluated_loc: Some(chunk.total_loc as u64),
        samples: None,
        // M26 / M18-W2: chunk-fallback scores carry no per-sample-rate signal
        // — the chunk task does one expert call, not N — so leave the
        // sampling counters at their `Default::default()` (None).
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::experts::chunk::CodeChunk;

    fn fixture_chunk(module: &str, loc: usize) -> CodeChunk {
        CodeChunk {
            module: module.to_string(),
            files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            total_loc: loc,
            code: String::new(),
        }
    }

    /// M26 visibility: every chunk-level fallback score carries the
    /// `chunk not reviewed` prefix AND a module-specific summary line so
    /// a reader can grep the report (or the JSON) for any chunk the model
    /// didn't assess. The prefix is the contract the renderer keys off to
    /// build the `Chunks Reviewed` appendix.
    #[test]
    fn chunk_fallback_score_carries_not_reviewed_prefix_and_module() {
        let score = chunk_fallback_score(&fixture_chunk("src/llm", 123), "boom".to_string());
        assert!(score.fallback, "fallback flag must be set");
        assert_eq!(score.expert_name, "code_quality");
        assert_eq!(score.evaluated_loc, Some(123), "real LOC survives");
        assert!(
            score.summary.starts_with(CHUNK_NOT_REVIEWED_PREFIX),
            "summary must lead with the chunk-not-reviewed prefix: {}",
            score.summary
        );
        assert!(
            score.summary.contains("src/llm"),
            "the summary must name the module so the report's reader can grep for it: {}",
            score.summary
        );
        assert!(
            score.summary.contains("boom"),
            "the reason must reach the rendered summary line: {}",
            score.summary
        );
    }
}
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
