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
                let chunk_loc = estimate_loc(&s.details);
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
            merged_scores.push(ExpertScore {
                expert_name: name,
                weight,
                score: avg_score,
                summary: best_summary,
                details: merged_details,
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

fn estimate_loc(details: &[ScoreItem]) -> u64 {
    // Rough LOC estimate: each chunk typically has 200-2000 LOC
    // We use the number of findings as a proxy for chunk size
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
mod tests {
    use super::*;

    fn make_item(severity: &str, message: &str) -> ScoreItem {
        ScoreItem {
            severity: severity.to_string(),
            message: message.to_string(),
            file: None,
            ..Default::default()
        }
    }

    fn make_score(name: &str, score: u8, weight: u8, summary: &str, details: Vec<ScoreItem>) -> ExpertScore {
        ExpertScore {
            expert_name: name.to_string(),
            weight,
            score,
            summary: summary.to_string(),
            details,
        }
    }

    // ─── aggregate with single-expert groups ─────

    #[test]
    fn test_aggregate_empty_input() {
        let result = aggregate(vec![], None);
        assert!(result.scores.is_empty());
        assert!(result.all_findings.is_empty());
    }

    #[test]
    fn test_aggregate_single_expert() {
        let details = vec![make_item("high", "Issue 1")];
        let scores = vec![make_score("lead", 85, 20, "Good", details.clone())];
        let result = aggregate(scores, None);
        assert_eq!(result.scores.len(), 1);
        assert_eq!(result.scores[0].expert_name, "lead");
        assert_eq!(result.scores[0].score, 85);
        assert_eq!(result.all_findings.len(), 1);
    }

    #[test]
    fn test_aggregate_single_expert_with_noise_filtered() {
        let details = vec![
            make_item("high", "Real issue"),
            make_item("info", "No code snippet provided"), // should be filtered
        ];
        let scores = vec![make_score("lead", 70, 20, "Assessment", details)];
        let result = aggregate(scores, None);
        // noise should be filtered
        assert_eq!(result.scores[0].details.len(), 1);
        assert_eq!(result.scores[0].details[0].message, "Real issue");
    }

    // ─── aggregate with multi-expert groups ──────

    #[test]
    fn test_aggregate_multi_chunk_loc_weighted_average() {
        let scores = vec![
            make_score(
                "code_quality",
                80,
                10,
                "chunk1 review",
                vec![make_item("medium", "Issue A")],
            ),
            make_score(
                "code_quality",
                60,
                10,
                "chunk2 review",
                vec![make_item("high", "Issue B")],
            ),
        ];
        let result = aggregate(scores, None);
        assert_eq!(result.scores.len(), 1);
        assert_eq!(result.scores[0].expert_name, "code_quality");
        // LOC-weighted: each chunk has (1 finding * 200) = 200 LOC estimate
        // total_weighted = 80*200 + 60*200 = 28000, total_loc = 400, avg = 70
        assert_eq!(result.scores[0].score, 70);
    }

    #[test]
    fn test_aggregate_multi_chunk_loc_weighted_summary() {
        let scores = vec![
            make_score("code_quality", 90, 10, "Great module", vec![make_item("note", "Fine")]),
            make_score("code_quality", 50, 10, "Needs work", vec![make_item("critical", "Bug")]),
        ];
        let result = aggregate(scores, None);
        // Should pick the best (non-noise) summary: "Great module" (score 90)
        assert_eq!(result.scores[0].summary, "Great module");
    }

    #[test]
    fn test_aggregate_multi_chunk_only_noise_summaries() {
        let scores = vec![
            make_score(
                "code_quality",
                70,
                10,
                "No code provided",
                vec![make_item("medium", "Issue")],
            ),
            make_score("code_quality", 80, 10, "No code sample", vec![make_item("low", "Nit")]),
        ];
        let result = aggregate(scores, None);
        // Both summaries are noise, should fall back to "N chunks evaluated, avg score M"
        assert!(result.scores[0].summary.contains("chunks evaluated"));
    }

    #[test]
    fn test_aggregate_multi_chunk_deduplicated() {
        let details = vec![
            make_item("high", "Duplicate issue"),
            make_item("high", "Duplicate issue"), // same after normalization
        ];
        let scores = vec![
            make_score("code_quality", 70, 10, "OK", details),
            make_score("code_quality", 70, 10, "OK", vec![make_item("low", "Unique issue")]),
        ];
        let result = aggregate(scores, None);
        // Dedup should leave only 2 unique findings (duplicate issue + unique)
        // But both chunks have separate details; dedup happens after merging
        assert_eq!(result.all_findings.len(), 2);
    }

    // ─── lead-consolidator integration ──────────

    fn make_item_full(severity: &str, message: &str, file: &str, confidence: Option<u8>) -> ScoreItem {
        ScoreItem {
            severity: severity.to_string(),
            message: message.to_string(),
            file: Some(file.to_string()),
            confidence,
            ..Default::default()
        }
    }

    #[test]
    fn test_consolidator_dedupes_identical_chunk_findings() {
        // Two chunks report the identical issue in the same file: the lead
        // consolidator must merge them into one finding.
        let scores = vec![
            make_score(
                "code_quality",
                80,
                10,
                "chunk1",
                vec![make_item_full("high", "Duplicate issue", "src/a.rs", Some(9))],
            ),
            make_score(
                "code_quality",
                60,
                10,
                "chunk2",
                vec![
                    make_item_full("high", "Duplicate issue", "src/a.rs", Some(9)),
                    make_item_full("medium", "Unique issue", "src/b.rs", Some(9)),
                ],
            ),
        ];
        let result = aggregate(scores, None);
        let cq = result.scores.iter().find(|s| s.expert_name == "code_quality").unwrap();
        assert_eq!(cq.details.len(), 2);
        assert_eq!(cq.details.iter().filter(|d| d.message == "Duplicate issue").count(), 1);
        // confidence 9 >= min_confidence (6): severities kept
        assert_eq!(cq.details[0].severity, "high");
        assert_eq!(cq.details[1].severity, "medium");
    }

    #[test]
    fn test_consolidator_downgrades_low_confidence_findings() {
        // Default min_confidence is 6 and drop_low_confidence is false, so a
        // low-confidence critical finding is downgraded one severity level.
        let scores = vec![
            make_score(
                "code_quality",
                70,
                10,
                "chunk1",
                vec![make_item_full("medium", "Solid issue", "src/a.rs", Some(8))],
            ),
            make_score(
                "code_quality",
                70,
                10,
                "chunk2",
                vec![make_item_full("critical", "Shaky claim", "src/b.rs", Some(3))],
            ),
        ];
        let result = aggregate(scores, None);
        let cq = result.scores.iter().find(|s| s.expert_name == "code_quality").unwrap();
        assert_eq!(cq.details.len(), 2);
        let shaky = cq.details.iter().find(|d| d.message == "Shaky claim").unwrap();
        assert_eq!(shaky.severity, "high"); // critical downgraded once
        assert_eq!(shaky.confidence, Some(3));
        let solid = cq.details.iter().find(|d| d.message == "Solid issue").unwrap();
        assert_eq!(solid.severity, "medium");
        assert_eq!(solid.confidence, Some(8));
    }

    #[test]
    fn test_consolidator_drops_low_confidence_when_configured() {
        let config: AppConfig = toml::from_str("[report]\ndrop_low_confidence = true\n").unwrap();
        let scores = vec![
            make_score(
                "code_quality",
                70,
                10,
                "chunk1",
                vec![make_item_full("high", "Kept issue", "src/a.rs", Some(8))],
            ),
            make_score(
                "code_quality",
                70,
                10,
                "chunk2",
                vec![make_item_full("high", "Dropped issue", "src/b.rs", Some(2))],
            ),
        ];
        let result = aggregate(scores, Some(&config));
        let cq = result.scores.iter().find(|s| s.expert_name == "code_quality").unwrap();
        assert_eq!(cq.details.len(), 1);
        assert_eq!(cq.details[0].message, "Kept issue");
    }

    // ─── filter_noise / dedup ────────────────────

    #[test]
    fn test_filter_noise_removes_noise_patterns() {
        let items = vec![
            make_item("high", "Real vulnerability"),
            make_item("info", "No code snippet in response"),
            make_item("low", "Unable to evaluate the code"),
            make_item("medium", "cannot assess this section"),
        ];
        let filtered = filter_noise(items);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].message, "Real vulnerability");
    }

    #[test]
    fn test_filter_noise_all_noise() {
        let items = vec![
            make_item("info", "No code sample available"),
            make_item("note", "Unable to determine"),
        ];
        let filtered = filter_noise(items);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_noise_empty() {
        let filtered: Vec<ScoreItem> = filter_noise(vec![]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_merge_deduplicate_removes_duplicates() {
        let items = vec![
            make_item("high", "Same issue!"),
            make_item("high", "Same issue!"),
            make_item("low", "Same issue!"), // same message, different severity
        ];
        let deduped = merge_deduplicate(items);
        assert_eq!(deduped.len(), 1);
        // higher severity should win
        assert_eq!(deduped[0].severity, "high");
    }

    #[test]
    fn test_merge_deduplicate_case_insensitive() {
        let items = vec![
            make_item("high", "Issue in File"),
            make_item("medium", "issue in file"), // same normalized
        ];
        let deduped = merge_deduplicate(items);
        assert_eq!(deduped.len(), 1);
        // higher severity should win
        assert_eq!(deduped[0].severity, "high");
    }

    #[test]
    fn test_merge_deduplicate_merges_fields() {
        let items = vec![
            ScoreItem {
                severity: "low".to_string(),
                message: "first version".to_string(),
                file: None,
                evidence: None,
                impact: Some("Small impact".to_string()),
                recommendation: Some("Fix it".to_string()),
                effort: Some("small".to_string()),
                confidence: None,
            },
            ScoreItem {
                severity: "high".to_string(),
                message: "first version".to_string(),
                file: None,
                evidence: Some("Longer evidence here".to_string()),
                impact: Some("Larger impact".to_string()),
                recommendation: Some("Better fix".to_string()),
                effort: Some("large".to_string()),
                confidence: None,
            },
        ];
        let deduped = merge_deduplicate(items);
        assert_eq!(deduped.len(), 1);
        // higher severity, longer evidence/impact/recommendation, higher effort
        assert_eq!(deduped[0].severity, "high");
        assert_eq!(deduped[0].evidence.as_deref(), Some("Longer evidence here"));
        assert_eq!(deduped[0].impact.as_deref(), Some("Larger impact"));
        assert_eq!(deduped[0].recommendation.as_deref(), Some("Better fix"));
        assert_eq!(deduped[0].effort.as_deref(), Some("large"));
    }

    // ─── severity_rank / sorting ─────────────────

    #[test]
    fn test_aggregate_findings_sorted_by_severity() {
        let scores = vec![make_score(
            "lead",
            80,
            20,
            "Summary",
            vec![
                make_item("low", "Minor issue"),
                make_item("critical", "Critical bug"),
                make_item("medium", "Medium concern"),
            ],
        )];
        let result = aggregate(scores, None);
        assert_eq!(result.all_findings.len(), 3);
        // Should be sorted by severity descending: critical, medium, low
        assert_eq!(result.all_findings[0].severity, "critical");
        assert_eq!(result.all_findings[1].severity, "medium");
        assert_eq!(result.all_findings[2].severity, "low");
    }

    #[test]
    fn test_aggregate_truncates_to_max_findings() {
        let many_items: Vec<ScoreItem> = (0..30).map(|i| make_item("low", &format!("Issue {}", i))).collect();
        let scores = vec![make_score("lead", 50, 20, "Summary", many_items)];
        let result = aggregate(scores, None);
        assert!(result.all_findings.len() <= 20);
    }

    // ─── edge cases ──────────────────────────────

    #[test]
    fn test_aggregate_zero_score_division() {
        // Simulate group with zero details (zero LOC)
        let scores = vec![
            make_score("code_quality", 80, 10, "First", vec![]),
            make_score("code_quality", 60, 10, "Second", vec![]),
        ];
        let result = aggregate(scores, None);
        // total_loc = max(0*200, 100) = 100 for each, so 200 total
        // Actually estimate_loc returns (0 * 200).max(100) = 100 for each
        // total_weighted = 80*100 + 60*100 = 14000, total_loc = 200, avg = 70
        assert_eq!(result.scores[0].score, 70);
    }

    #[test]
    fn test_aggregate_mixed_expert_groups() {
        let scores = vec![
            make_score(
                "architecture",
                90,
                15,
                "Good structure",
                vec![make_item("note", "Well organized")],
            ),
            make_score("code_quality", 70, 10, "Chunk1", vec![make_item("medium", "Issue X")]),
            make_score("code_quality", 50, 10, "Chunk2", vec![make_item("high", "Issue Y")]),
        ];
        let result = aggregate(scores, None);
        // 2 groups: architecture (single) and code_quality (multi)
        assert_eq!(result.scores.len(), 2);
        let arch = result.scores.iter().find(|s| s.expert_name == "architecture").unwrap();
        assert_eq!(arch.score, 90);
        let cq = result.scores.iter().find(|s| s.expert_name == "code_quality").unwrap();
        assert_eq!(cq.score, 60); // (70*200 + 50*200) / 400 = 60
    }

    #[test]
    fn test_is_noise_summary() {
        assert!(is_noise_summary("No code provided"));
        assert!(is_noise_summary("no code sample"));
        assert!(!is_noise_summary("Valid summary"));
    }
}
