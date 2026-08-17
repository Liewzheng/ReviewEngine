use super::*;

#[test]
fn validate_findings_keeps_file_without_line() {
    let findings = vec![Finding {
        file: "src/main.rs".to_string(),
        line: None,
        line_end: None,
        severity: Severity::High,
        confidence: 8,
        category: "test".to_string(),
        title: "No line".to_string(),
        summary: "summary".to_string(),
        evidence: "evidence".to_string(),
        impact: "impact".to_string(),
        recommendation: "rec".to_string(),
        effort: Effort::Small,
        expert_name: "expert".to_string(),
        expert_role: "role".to_string(),
        agrees_with: vec![],
        references: vec![],
    }];
    let diff_files = vec![("src/main.rs".to_string(), vec![])];
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1);
}

#[test]
fn validate_findings_drops_file_not_in_diff() {
    let findings = vec![Finding {
        file: "src/other.rs".to_string(),
        line: Some(10),
        line_end: None,
        severity: Severity::High,
        confidence: 8,
        category: "test".to_string(),
        title: "Other file".to_string(),
        summary: "summary".to_string(),
        evidence: "evidence".to_string(),
        impact: "impact".to_string(),
        recommendation: "rec".to_string(),
        effort: Effort::Small,
        expert_name: "expert".to_string(),
        expert_role: "role".to_string(),
        agrees_with: vec![],
        references: vec![],
    }];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -1,5 +1,5 @@".to_string(),
            old_start: 1,
            old_lines: 5,
            new_start: 1,
            new_lines: 5,
            lines: vec![],
        }],
    )];
    let validated = validate_findings(&findings, &diff_files);
    assert!(validated.is_empty());
}

#[test]
fn validate_findings_keeps_line_inside_hunk_range() {
    let findings = vec![Finding {
        file: "src/main.rs".to_string(),
        line: Some(12),
        line_end: None,
        severity: Severity::High,
        confidence: 8,
        category: "test".to_string(),
        title: "In range".to_string(),
        summary: "summary".to_string(),
        evidence: "evidence".to_string(),
        impact: "impact".to_string(),
        recommendation: "rec".to_string(),
        effort: Effort::Small,
        expert_name: "expert".to_string(),
        expert_role: "role".to_string(),
        agrees_with: vec![],
        references: vec![],
    }];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -10,5 +10,8 @@".to_string(),
            old_start: 10,
            old_lines: 5,
            new_start: 10,
            new_lines: 8,
            lines: vec![],
        }],
    )];
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1);
}

#[test]
fn validate_findings_drops_line_outside_hunk_range() {
    let findings = vec![Finding {
        file: "src/main.rs".to_string(),
        line: Some(25),
        line_end: None,
        severity: Severity::High,
        confidence: 8,
        category: "test".to_string(),
        title: "Out of range".to_string(),
        summary: "summary".to_string(),
        evidence: "evidence".to_string(),
        impact: "impact".to_string(),
        recommendation: "rec".to_string(),
        effort: Effort::Small,
        expert_name: "expert".to_string(),
        expert_role: "role".to_string(),
        agrees_with: vec![],
        references: vec![],
    }];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -10,5 +10,8 @@".to_string(),
            old_start: 10,
            old_lines: 5,
            new_start: 10,
            new_lines: 8,
            lines: vec![],
        }],
    )];
    // File IS in the diff but the reported line is outside the hunk: the
    // finding must be downgraded to keep-with-note, never dropped.
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1, "file in diff + line outside hunk must be kept");
    assert!(
        validated[0].summary.contains("line outside diff hunk"),
        "kept finding must carry the outside-hunk note, got: {}",
        validated[0].summary
    );
}

#[test]
fn validate_findings_checks_any_hunk_for_file() {
    let findings = vec![Finding {
        file: "src/main.rs".to_string(),
        line: Some(35),
        line_end: None,
        severity: Severity::High,
        confidence: 8,
        category: "test".to_string(),
        title: "Second hunk".to_string(),
        summary: "summary".to_string(),
        evidence: "evidence".to_string(),
        impact: "impact".to_string(),
        recommendation: "rec".to_string(),
        effort: Effort::Small,
        expert_name: "expert".to_string(),
        expert_role: "role".to_string(),
        agrees_with: vec![],
        references: vec![],
    }];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![
            DiffHunk {
                header: "@@ -10,5 +10,5 @@".to_string(),
                old_start: 10,
                old_lines: 5,
                new_start: 10,
                new_lines: 5,
                lines: vec![],
            },
            DiffHunk {
                header: "@@ -30,5 +30,10 @@".to_string(),
                old_start: 30,
                old_lines: 5,
                new_start: 30,
                new_lines: 10,
                lines: vec![],
            },
        ],
    )];
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1);
}

fn test_finding(file: &str, line: Option<u32>, line_end: Option<u32>, title: &str) -> Finding {
    Finding {
        file: file.to_string(),
        line,
        line_end,
        severity: Severity::High,
        confidence: 8,
        category: "test".to_string(),
        title: title.to_string(),
        summary: "summary".to_string(),
        evidence: "evidence".to_string(),
        impact: "impact".to_string(),
        recommendation: "rec".to_string(),
        effort: Effort::Small,
        expert_name: "expert".to_string(),
        expert_role: "role".to_string(),
        agrees_with: vec![],
        references: vec![],
    }
}

#[test]
fn validate_findings_rejects_pure_deletion_hunk() {
    let findings = vec![test_finding("src/main.rs", Some(10), None, "Pure deletion")];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -10,5 +9,0 @@".to_string(),
            old_start: 10,
            old_lines: 5,
            new_start: 9,
            new_lines: 0,
            lines: vec![],
        }],
    )];
    // File is in the diff (it was modified by a deletion): keep with the
    // outside-hunk note instead of dropping.
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(
        validated.len(),
        1,
        "file in diff must be kept even for a deletion-only hunk"
    );
    assert!(validated[0].summary.contains("line outside diff hunk"));
}

#[test]
fn validate_findings_accepts_valid_new_lines() {
    let findings = vec![test_finding("src/main.rs", Some(12), None, "Valid new line")];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -10,5 +10,8 @@".to_string(),
            old_start: 10,
            old_lines: 5,
            new_start: 10,
            new_lines: 8,
            lines: vec![],
        }],
    )];
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1);
}

#[test]
fn validate_findings_rejects_line_end_outside_hunk() {
    let findings = vec![test_finding("src/main.rs", Some(12), Some(25), "line_end outside")];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -10,5 +10,8 @@".to_string(),
            old_start: 10,
            old_lines: 5,
            new_start: 10,
            new_lines: 8,
            lines: vec![],
        }],
    )];
    // The starting line is inside the hunk; line_end may span beyond it.
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1, "line in hunk with spanning line_end must be kept");
    assert!(
        !validated[0].summary.contains("line outside diff hunk"),
        "in-hunk finding must not carry the outside-hunk note"
    );
}

#[test]
fn validate_findings_accepts_line_end_inside_hunk() {
    let findings = vec![test_finding("src/main.rs", Some(12), Some(15), "line_end inside")];
    let diff_files = vec![(
        "src/main.rs".to_string(),
        vec![DiffHunk {
            header: "@@ -10,5 +10,8 @@".to_string(),
            old_start: 10,
            old_lines: 5,
            new_start: 10,
            new_lines: 8,
            lines: vec![],
        }],
    )];
    let validated = validate_findings(&findings, &diff_files);
    assert_eq!(validated.len(), 1);
}

#[test]
fn validate_findings_drops_all_when_diff_files_empty() {
    let findings = vec![
        test_finding("src/main.rs", None, None, "No line"),
        test_finding("src/main.rs", Some(10), None, "With line"),
    ];
    let diff_files: Vec<(String, Vec<DiffHunk>)> = vec![];
    let validated = validate_findings(&findings, &diff_files);
    assert!(validated.is_empty());
}
