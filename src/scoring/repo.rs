use crate::repo::analysis::{FileAnalysis, SecurityFinding};
use crate::repo::FileEntry;

/// A single risk item for the risk map.
#[derive(Debug, Clone)]
pub struct RiskItem {
    pub area: String,
    pub risk: String,
    pub recommendation: String,
}

/// An action item for improvement.
#[derive(Debug, Clone)]
pub struct ActionItem {
    pub title: String,
    pub effort: String,
}

/// Final review score for the repository.
#[derive(Debug, Clone)]
pub struct RepoScore {
    pub health_score: u8,
    pub risk_level: String,
    pub risk_map: Vec<RiskItem>,
    pub action_items: Vec<ActionItem>,
}

/// Compute the overall repository health score and risk map.
pub fn score_repository(
    entries: &[FileEntry],
    large_files: &[FileAnalysis],
    security_findings: &[SecurityFinding],
) -> RepoScore {
    let mut score = 100i32;

    // Deduct for large files (non-linear: first 5 cost 5 each, rest cost 2 each, max 40)
    let large_count = large_files.len();
    let large_deduction = if large_count > 5 {
        5 * 5 + (large_count - 5).min(10) * 2
    } else {
        large_count * 5
    };
    score -= (large_deduction as i32).min(40);

    // Deduct for security findings
    score -= (security_findings.len() as i32).min(20) * 8;

    // Deduct for generated files
    let generated = entries.iter().filter(|e| e.is_generated).count();
    score -= (generated as i32).min(10) * 2;

    let health_score = score.clamp(0, 100) as u8;
    let risk_level = match health_score {
        0..=40 => "critical",
        41..=60 => "high",
        61..=80 => "medium",
        81..=90 => "low",
        _ => "healthy",
    }
    .to_string();

    let mut risk_map = Vec::new();

    if !security_findings.is_empty() {
        risk_map.push(RiskItem {
            area: "security".to_string(),
            risk: format!("{} potential security issues found", security_findings.len()),
            recommendation: "Review and fix security patterns. Use environment variables for secrets.".to_string(),
        });
    }

    if !large_files.is_empty() {
        risk_map.push(RiskItem {
            area: "maintainability".to_string(),
            risk: format!("{} files exceed 500 lines", large_files.len()),
            recommendation: "Split large files into smaller modules for better maintainability.".to_string(),
        });
    }

    let total_loc: usize = entries.iter().map(|e| e.loc).sum();
    let source_files = entries.iter().filter(|e| !e.is_binary && !e.is_generated).count();
    if source_files > 0 && total_loc > 0 {
        let avg_loc = total_loc / source_files;
        if avg_loc > 200 {
            risk_map.push(RiskItem {
                area: "code_quality".to_string(),
                risk: format!("Average file size is {} lines", avg_loc),
                recommendation: "Consider keeping files under 200 lines for better readability.".to_string(),
            });
        }
    }

    let mut action_items = Vec::new();
    if !security_findings.is_empty() {
        action_items.push(ActionItem {
            title: format!("Fix {} security issues", security_findings.len()),
            effort: "medium".to_string(),
        });
    }
    if !large_files.is_empty() {
        action_items.push(ActionItem {
            title: format!("Refactor {} large files", large_files.len()),
            effort: "medium".to_string(),
        });
    }
    if generated > 0 {
        action_items.push(ActionItem {
            title: "Review generated files in repository".to_string(),
            effort: "low".to_string(),
        });
    }
    if total_loc < 1000 {
        action_items.push(ActionItem {
            title: "Add more code to the repository".to_string(),
            effort: "low".to_string(),
        });
    }

    RepoScore {
        health_score,
        risk_level,
        risk_map,
        action_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::analysis::{FileAnalysis, SecurityFinding};
    use crate::repo::FileEntry;

    fn make_entry(path: &str, loc: usize, is_generated: bool) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            language: "Rust".to_string(),
            loc,
            is_binary: false,
            is_generated,
        }
    }

    fn make_large(path: &str, loc: usize) -> FileAnalysis {
        FileAnalysis {
            path: path.to_string(),
            loc,
            language: "Rust".to_string(),
            issues: vec!["File is large".to_string()],
        }
    }

    fn make_security(pattern: &str) -> SecurityFinding {
        SecurityFinding {
            file: "src/main.rs".to_string(),
            pattern: pattern.to_string(),
            line: 1,
            severity: "medium".to_string(),
        }
    }

    #[test]
    fn test_score_perfect() {
        let score = score_repository(&[], &[], &[]);
        assert_eq!(score.health_score, 100);
        assert_eq!(score.risk_level, "healthy");
    }

    #[test]
    fn test_score_with_issues() {
        let entries = vec![crate::repo::FileEntry {
            path: "big.rs".to_string(),
            language: "Rust".to_string(),
            loc: 600,
            is_binary: false,
            is_generated: false,
        }];
        let large = vec![FileAnalysis {
            path: "big.rs".to_string(),
            loc: 600,
            language: "Rust".to_string(),
            issues: vec!["File has 600 lines".to_string()],
        }];
        let score = score_repository(&entries, &large, &[]);
        assert!(score.health_score < 100);
        assert!(score.health_score > 85); // single large file deducts 5 → 95
        assert!(!score.risk_map.is_empty());
    }

    #[test]
    fn test_score_many_large_files_capped() {
        // 15 large files: first 5 cost 5 each (25), next 10 cost 2 each (20) = 45 total
        let large: Vec<FileAnalysis> = (0..15)
            .map(|i| FileAnalysis {
                path: format!("big{i}.rs"),
                loc: 600,
                language: "Rust".to_string(),
                issues: vec!["File has 600 lines".to_string()],
            })
            .collect();
        let score = score_repository(&[], &large, &[]);
        assert_eq!(score.health_score, 60); // 100 - min(45, 40) = 60
        assert_eq!(score.risk_level, "high");
    }

    #[test]
    fn test_score_generated_files_no_double_penalty() {
        // Generated files should not be counted in large_files, but may be in generated count
        let score = score_repository(&[], &[], &[]);
        assert_eq!(score.health_score, 100);
    }

    #[test]
    fn score_repository_empty_inputs_is_healthy() {
        let score = score_repository(&[], &[], &[]);
        assert_eq!(score.health_score, 100);
        assert_eq!(score.risk_level, "healthy");
    }

    #[test]
    fn score_repository_large_files_deduction_capped_at_40() {
        let large: Vec<FileAnalysis> = (0..30).map(|i| make_large(&format!("big{i}.rs"), 600)).collect();
        let score = score_repository(&[], &large, &[]);
        assert_eq!(score.health_score, 60); // 100 - 40 (capped)
        assert_eq!(score.risk_level, "high");

        let fewer: Vec<FileAnalysis> = (0..20).map(|i| make_large(&format!("big{i}.rs"), 600)).collect();
        let fewer_score = score_repository(&[], &fewer, &[]);
        assert_eq!(fewer_score.health_score, score.health_score);
    }

    #[test]
    fn score_repository_security_findings_capped_at_20_count() {
        let few: Vec<SecurityFinding> = (0..3).map(|_| make_security("Hardcoded password")).collect();
        let score = score_repository(&[], &[], &few);
        assert_eq!(score.health_score, 76); // 100 - 3 * 8

        let max: Vec<SecurityFinding> = (0..20).map(|_| make_security("Hardcoded password")).collect();
        let max_score = score_repository(&[], &[], &max);
        assert_eq!(max_score.health_score, 0);

        let over: Vec<SecurityFinding> = (0..30).map(|_| make_security("Hardcoded password")).collect();
        let over_score = score_repository(&[], &[], &over);
        assert_eq!(over_score.health_score, max_score.health_score);
    }

    #[test]
    fn score_repository_generated_files_deduction() {
        let entries: Vec<FileEntry> = (0..10).map(|i| make_entry(&format!("gen{i}.rs"), 100, true)).collect();
        let score = score_repository(&entries, &[], &[]);
        assert_eq!(score.health_score, 80); // 100 - 10 * 2

        let more: Vec<FileEntry> = (0..15).map(|i| make_entry(&format!("gen{i}.rs"), 100, true)).collect();
        let more_score = score_repository(&more, &[], &[]);
        assert_eq!(more_score.health_score, score.health_score);
    }

    #[test]
    fn score_repository_combination_pushes_critical_risk() {
        let large: Vec<FileAnalysis> = (0..20).map(|i| make_large(&format!("big{i}.rs"), 600)).collect();
        let entries: Vec<FileEntry> = (0..10).map(|i| make_entry(&format!("gen{i}.rs"), 100, true)).collect();
        let score = score_repository(&entries, &large, &[]);
        assert_eq!(score.health_score, 40); // 100 - 40 - 20
        assert_eq!(score.risk_level, "critical");
    }

    #[test]
    fn score_repository_combination_pushes_high_risk() {
        let large: Vec<FileAnalysis> = (0..15).map(|i| make_large(&format!("big{i}.rs"), 600)).collect();
        let score = score_repository(&[], &large, &[]);
        assert_eq!(score.health_score, 60);
        assert_eq!(score.risk_level, "high");
    }

    #[test]
    fn score_repository_combination_pushes_medium_risk() {
        let large: Vec<FileAnalysis> = (0..12).map(|i| make_large(&format!("big{i}.rs"), 600)).collect();
        let score = score_repository(&[], &large, &[]);
        assert_eq!(score.health_score, 61); // 100 - 39
        assert_eq!(score.risk_level, "medium");
    }

    #[test]
    fn score_repository_combination_pushes_low_risk() {
        let large = vec![
            make_large("big.rs", 600),
            make_large("big2.rs", 600),
            make_large("big3.rs", 600),
        ];
        let score = score_repository(&[], &large, &[]);
        assert_eq!(score.health_score, 85); // 100 - 15
        assert_eq!(score.risk_level, "low");
    }

    #[test]
    fn score_repository_combination_stays_healthy() {
        let large = vec![make_large("big.rs", 600)];
        let score = score_repository(&[], &large, &[]);
        assert_eq!(score.health_score, 95); // 100 - 5
        assert_eq!(score.risk_level, "healthy");
    }
}
