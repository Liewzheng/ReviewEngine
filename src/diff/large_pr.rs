use crate::diff::chunker::DiffChunk;
use crate::diff::constants::DEFAULT_TOKEN_MODEL;
use crate::diff::processor;
use crate::models::*;
use crate::tokenizer::count_tokens;

/// Pre-assessment threshold: estimate large PR from diff byte size before parsing.
///
/// Derived from `large_pr_line_threshold × 50`, where 50 bytes/line is the
/// average size of a unified diff line (header + context + changed content).
/// Used by orchestrators to select progress stages before the diff is parsed.
///
/// For exact assessment (after parsing), see [`assess_large_pr`] + [`LargePrThresholds`].
pub fn pre_assess_bytes(config: &DiffConfig) -> usize {
    config.large_pr_line_threshold * 50
}

/// Thresholds for determining if a PR is large.
pub struct LargePrThresholds {
    pub max_files: usize,
    pub max_total_changes: u32,
    pub max_tokens: usize,
}

impl Default for LargePrThresholds {
    fn default() -> Self {
        Self {
            max_files: 20,
            max_total_changes: 1000,
            max_tokens: 80000,
        }
    }
}

/// Compression levels for large PRs.
#[derive(Debug, Clone, PartialEq)]
pub enum CompressionLevel {
    None,
    Light,
    Medium,
    Aggressive,
}

/// Result of a large PR assessment.
#[derive(Debug, Clone)]
pub struct LargePrAssessment {
    pub is_large: bool,
    pub compression_level: CompressionLevel,
    pub file_count: usize,
    pub total_changes: u32,
    pub estimated_tokens: usize,
    pub details: Vec<String>,
}

/// Assess whether a set of diff files constitutes a large PR.
pub fn assess_large_pr(files: &[DiffFile], thresholds: &LargePrThresholds) -> LargePrAssessment {
    let file_count = files.len();
    let total_changes: u32 = files.iter().map(|f| f.additions + f.deletions).sum();
    let diff_text = processor::render_diff_text(files);
    let estimated_tokens = match count_tokens(&diff_text, DEFAULT_TOKEN_MODEL) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count tokens for large PR estimate; assuming 0");
            0
        }
    };

    let mut details = Vec::new();
    let mut compression = CompressionLevel::None;

    if file_count > thresholds.max_files {
        details.push(format!("{} files (threshold: {})", file_count, thresholds.max_files));
    }
    if total_changes > thresholds.max_total_changes {
        details.push(format!(
            "{} total changes (threshold: {})",
            total_changes, thresholds.max_total_changes
        ));
    }
    if estimated_tokens > thresholds.max_tokens {
        details.push(format!(
            "{} estimated tokens (threshold: {})",
            estimated_tokens, thresholds.max_tokens
        ));
    }

    let is_large = !details.is_empty();

    if is_large {
        // Determine compression level
        let severity = details.len() as f64 + (file_count as f64 / thresholds.max_files as f64).max(1.0) * 0.5;

        compression = if severity > 4.0 {
            CompressionLevel::Aggressive
        } else if severity > 2.5 {
            CompressionLevel::Medium
        } else {
            CompressionLevel::Light
        };
    }

    LargePrAssessment {
        is_large,
        compression_level: compression,
        file_count,
        total_changes,
        estimated_tokens,
        details,
    }
}

/// Apply compression to files based on the compression level.
pub fn apply_compression(files: &mut Vec<DiffFile>, level: &CompressionLevel) -> Vec<String> {
    let mut actions = Vec::new();

    match level {
        CompressionLevel::None => {}
        CompressionLevel::Light => {
            // Light: filter generated/vendor files, apply token budget
            let before = files.len();
            files.retain(|f| !processor::should_ignore_file(f));
            let removed = before - files.len();
            if removed > 0 {
                actions.push(format!("Removed {} generated/vendor files", removed));
            }
        }
        CompressionLevel::Medium => {
            // Medium: Light + compress deletions + sort by priority + truncate long lines
            files.retain(|f| !processor::should_ignore_file(f));
            let (kept, deleted) = processor::compress_deletions(std::mem::take(files));
            *files = kept;
            if !deleted.is_empty() {
                actions.push(format!("Compressed {} deletion-only files", deleted.len()));
            }
            processor::sort_files_by_language_and_size(files);
            processor::truncate_long_lines(files, 200);
            actions.push("Sorted by language/size, truncated long lines".to_string());
        }
        CompressionLevel::Aggressive => {
            // Aggressive: Medium + aggressive token budget + priority-only files
            files.retain(|f| !processor::should_ignore_file(f));
            let (kept, deleted) = processor::compress_deletions(std::mem::take(files));
            *files = kept;
            if !deleted.is_empty() {
                actions.push(format!("Compressed {} deletion-only files", deleted.len()));
            }
            processor::sort_files_by_language_and_size(files);
            processor::truncate_long_lines(files, 120);
            processor::apply_token_budget(files, 40000);
            actions.push("Applied token budget of 40K".to_string());
        }
    }

    actions
}

/// Priority scoring for files to determine review order.
pub fn file_priority(file: &DiffFile) -> u8 {
    let mut score: u8 = 50;

    // Source code changes are highest priority
    let path = &file.new_path;
    if path.ends_with(".rs") || path.ends_with(".py") || path.ends_with(".js") || path.ends_with(".ts") {
        score = score.saturating_add(30);
    }

    // Security-sensitive files
    if path.contains("auth") || path.contains("security") || path.contains("password") {
        score = score.saturating_add(25);
    }

    // Config files
    if path.ends_with(".toml") || path.ends_with(".yaml") || path.ends_with(".json") {
        score = score.saturating_sub(10);
    }

    // Documentation
    if path.ends_with(".md") || path.ends_with(".rst") || path.ends_with(".txt") {
        score = score.saturating_sub(20);
    }

    // Larger changes get higher priority (more impact)
    let change_size = file.additions + file.deletions;
    if change_size > 100 {
        score = score.saturating_add(15);
    } else if change_size > 50 {
        score = score.saturating_add(10);
    } else if change_size > 10 {
        score = score.saturating_add(5);
    }

    score
}

/// Sort files by priority (highest first).
pub fn sort_by_priority(files: &mut [DiffFile]) {
    files.sort_by_key(|f| std::cmp::Reverse(file_priority(f)));
}

/// Route files to appropriate experts based on file types.
pub fn route_chunks<'a>(chunks: &[DiffChunk], experts: &'a [ExpertDef]) -> Vec<(&'a ExpertDef, Vec<DiffFile>)> {
    let mut assignments: Vec<(&ExpertDef, Vec<DiffFile>)> = Vec::new();

    for expert in experts {
        if expert.config.commands.iter().any(|c| c == "review") {
            let mut expert_files = Vec::new();
            for chunk in chunks {
                for file in &chunk.files {
                    // Check if this expert's trigger patterns match the file
                    if let ExpertTrigger::FilePatterns { ref patterns } = expert.trigger {
                        let matched = patterns.iter().any(|p| {
                            let path = &file.new_path;
                            // *.rs → ends_with(".rs")
                            if p.starts_with("*.") {
                                return path.ends_with(&p[1..]);
                            }
                            // **/*.rs → ends_with(".rs")
                            if p.starts_with("**/*.") {
                                return path.ends_with(&p[4..]);
                            }
                            // **/api/** → contains("/api/")
                            if p.starts_with("**/") && p.ends_with("/**") {
                                let mid = &p[3..p.len() - 3];
                                return path.contains(&format!("/{}/", mid));
                            }
                            // src/** or src/ → starts_with
                            if p.ends_with("/**") || p.ends_with('/') {
                                let prefix = p.trim_end_matches("/**").trim_end_matches('/');
                                return path.starts_with(prefix)
                                    && (path.len() == prefix.len()
                                        || path.as_bytes().get(prefix.len()) == Some(&b'/'));
                            }
                            // Default: contains match
                            path.contains(p.trim_matches('*'))
                        });
                        if !matched {
                            continue;
                        }
                    }
                    expert_files.push(file.clone());
                }
            }
            if !expert_files.is_empty() {
                assignments.push((expert, expert_files));
            }
        }
    }

    assignments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(path: &str, additions: u32, deletions: u32) -> DiffFile {
        DiffFile {
            path: path.to_string(),
            old_path: path.to_string(),
            new_path: path.to_string(),
            status: "modified".to_string(),
            additions,
            deletions,
            hunks: vec![],
        }
    }

    #[test]
    fn test_assess_small_pr() {
        let files = vec![make_file("src/main.rs", 10, 5)];
        let assessment = assess_large_pr(&files, &LargePrThresholds::default());
        assert!(!assessment.is_large);
        assert_eq!(assessment.compression_level, CompressionLevel::None);
    }

    #[test]
    fn test_assess_large_pr() {
        let files: Vec<DiffFile> = (0..30)
            .map(|i| make_file(&format!("src/file{}.rs", i), 50, 20))
            .collect();
        let assessment = assess_large_pr(&files, &LargePrThresholds::default());
        assert!(assessment.is_large);
        assert_ne!(assessment.compression_level, CompressionLevel::None);
    }

    #[test]
    fn test_file_priority_source() {
        let file = make_file("src/auth.rs", 200, 0);
        let score = file_priority(&file);
        assert!(score > 50, "Security source file should have high priority");
    }

    #[test]
    fn test_file_priority_doc() {
        let file = make_file("README.md", 5, 0);
        let score = file_priority(&file);
        assert!(score < 50, "Doc file should have lower priority");
    }

    #[test]
    fn test_sort_by_priority() {
        let mut files = vec![make_file("README.md", 5, 0), make_file("src/auth.rs", 200, 0)];
        sort_by_priority(&mut files);
        assert_eq!(files[0].new_path, "src/auth.rs");
    }

    #[test]
    fn test_apply_light_compression() {
        let mut files = vec![make_file("src/main.rs", 10, 5), make_file("Cargo.lock", 10, 5)];
        let actions = apply_compression(&mut files, &CompressionLevel::Light);
        assert!(!actions.is_empty());
        assert_eq!(files.len(), 1); // Cargo.lock removed
    }
}
