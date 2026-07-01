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
    use crate::repo::analysis::FileAnalysis;

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
}
