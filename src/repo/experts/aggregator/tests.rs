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
        fallback: false,
        evaluated_loc: None,
        samples: None,
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

// ─── real-LOC weighting (evaluated_loc) ─────

#[test]
fn test_aggregate_multi_chunk_prefers_real_loc_over_heuristic() {
    // Same finding count (1 each), very different real sizes: the big
    // chunk must dominate. Heuristic would weight them equally (70).
    let mut big = make_score("code_quality", 90, 10, "big", vec![make_item("low", "Nit")]);
    big.evaluated_loc = Some(1000);
    let mut small = make_score("code_quality", 50, 10, "small", vec![make_item("high", "Bug")]);
    small.evaluated_loc = Some(100);
    let result = aggregate(vec![big, small], None);
    // (90*1000 + 50*100) / 1100 = 86.36 → 86
    assert_eq!(result.scores[0].score, 86);
}

#[test]
fn test_aggregate_multi_chunk_mixed_loc_falls_back_to_heuristic_per_chunk() {
    // Chunk A reports real LOC, chunk B does not: B alone uses the
    // findings-count heuristic (2 findings → 400), A keeps its real 300.
    let mut a = make_score("code_quality", 90, 10, "a", vec![]);
    a.evaluated_loc = Some(300);
    let b = make_score(
        "code_quality",
        50,
        10,
        "b",
        vec![make_item("medium", "X"), make_item("low", "Y")],
    );
    let result = aggregate(vec![a, b], None);
    // (90*300 + 50*400) / 700 = 67.14 → 67
    assert_eq!(result.scores[0].score, 67);
}

#[test]
fn test_aggregate_multi_chunk_merges_loc_and_samples() {
    let mut a = make_score("code_quality", 80, 10, "a", vec![]);
    a.evaluated_loc = Some(1000);
    a.samples = Some(vec![75, 85]);
    let mut b = make_score("code_quality", 60, 10, "b", vec![]);
    b.evaluated_loc = Some(100);
    b.samples = Some(vec![60]);
    let result = aggregate(vec![a, b], None);
    let cq = &result.scores[0];
    assert_eq!(cq.evaluated_loc, Some(1100));
    assert_eq!(cq.samples, Some(vec![75, 85, 60]));
    // (80*1000 + 60*100) / 1100 = 78.18 → 78
    assert_eq!(cq.score, 78);
}

#[test]
fn test_aggregate_multi_chunk_fallback_only_when_every_chunk_fell_back() {
    let mut a = make_score("code_quality", 70, 10, "a", vec![]);
    a.fallback = true;
    let mut b = make_score("code_quality", 70, 10, "b", vec![]);
    b.fallback = true;
    let result = aggregate(vec![a, b], None);
    assert!(result.scores[0].fallback, "all-fallback group must stay flagged");

    let genuine = make_score("code_quality", 80, 10, "a", vec![]);
    let mut fell_back = make_score("code_quality", 70, 10, "b", vec![]);
    fell_back.fallback = true;
    let result = aggregate(vec![genuine, fell_back], None);
    assert!(
        !result.scores[0].fallback,
        "one genuine chunk makes the aggregate a genuine (degraded) assessment"
    );
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
