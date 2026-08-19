use super::metadata::*;
use super::render::*;
use super::scoring::*;
use super::types::*;
use super::{run_local_repo_review, run_repo_review};
use crate::models::*;
use crate::repo::experts::{self, ExpertScore, ScoreItem};
use crate::repo::FileEntry;
use anyhow::Result;

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
