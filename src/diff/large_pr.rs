use crate::diff::chunker::DiffChunk;
use crate::diff::constants::DEFAULT_TOKEN_MODEL;
use crate::diff::processor;
use crate::diff::render::render_file_diff;
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

/// Apply compression according to the configured `[diff] compression_level`,
/// falling back to `assessed` (from [`assess_large_pr`]) when the configured
/// value is `"auto"`, empty, or unrecognised.
///
/// Configured semantics:
/// - `"none"`: skip compression entirely.
/// - `"light"`: compress deletion-only files only.
/// - `"medium"` / `"aggressive"`: the corresponding [`apply_compression`] behaviour.
///
/// Returns the effective level and the actions taken.
pub fn apply_configured_compression(
    files: &mut Vec<DiffFile>,
    configured: &str,
    assessed: &CompressionLevel,
) -> (CompressionLevel, Vec<String>) {
    match configured.trim().to_lowercase().as_str() {
        "none" => (CompressionLevel::None, Vec::new()),
        "light" => (CompressionLevel::Light, apply_deletion_only_compression(files)),
        "medium" => (
            CompressionLevel::Medium,
            apply_compression(files, &CompressionLevel::Medium),
        ),
        "aggressive" => (
            CompressionLevel::Aggressive,
            apply_compression(files, &CompressionLevel::Aggressive),
        ),
        _ => (assessed.clone(), apply_compression(files, assessed)),
    }
}

/// Compress only deletion-only files (files whose hunks contain nothing but
/// deletions). Backs the explicitly configured `compression_level = "light"`;
/// the automatic `Light` level in [`apply_compression`] instead filters
/// generated/vendor files, and is left unchanged.
pub fn apply_deletion_only_compression(files: &mut Vec<DiffFile>) -> Vec<String> {
    let (kept, deleted) = processor::compress_deletions(std::mem::take(files));
    *files = kept;
    if deleted.is_empty() {
        Vec::new()
    } else {
        vec![format!("Compressed {} deletion-only files", deleted.len())]
    }
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

/// Route files to appropriate experts.
///
/// Routing is per file, in two tiers:
/// 1. **Content-pattern routing (priority).** If the file's rendered diff text
///    contains any of an expert's `content_patterns` substrings, the file is
///    routed to that expert. When at least one expert matches on content, the
///    file goes only to the content-matching experts.
/// 2. **Fallback routing.** Files that match no expert's `content_patterns`
///    follow the previous rules: experts with an [`ExpertTrigger::FilePatterns`]
///    trigger only receive files matching their glob patterns; every other
///    review expert receives all files.
///
/// Only experts whose `commands` include `review` participate.
pub fn route_chunks<'a>(chunks: &[DiffChunk], experts: &'a [ExpertDef]) -> Vec<(&'a ExpertDef, Vec<DiffFile>)> {
    let review_experts: Vec<&ExpertDef> = experts
        .iter()
        .filter(|e| e.config.commands.iter().any(|c| c == "review"))
        .collect();
    let mut buckets: Vec<Vec<DiffFile>> = review_experts.iter().map(|_| Vec::new()).collect();
    let any_content_patterns = review_experts.iter().any(|e| !e.config.content_patterns.is_empty());

    for chunk in chunks {
        for file in &chunk.files {
            // Tier 1: content-pattern routing takes priority when it matches.
            let mut content_matched = false;
            if any_content_patterns {
                let text = render_file_diff(file);
                for (i, expert) in review_experts.iter().enumerate() {
                    if expert.config.content_patterns.iter().any(|p| text.contains(p.as_str())) {
                        buckets[i].push(file.clone());
                        content_matched = true;
                    }
                }
            }
            if content_matched {
                continue;
            }

            // Tier 2: existing file-pattern / route-to-all fallback.
            for (i, expert) in review_experts.iter().enumerate() {
                if let ExpertTrigger::FilePatterns { ref patterns } = expert.trigger {
                    if !matches_file_patterns(patterns, &file.new_path) {
                        continue;
                    }
                }
                buckets[i].push(file.clone());
            }
        }
    }

    review_experts
        .into_iter()
        .zip(buckets)
        .filter(|(_, files)| !files.is_empty())
        .collect()
}

/// Match a file path against the simplified glob patterns used by
/// [`ExpertTrigger::FilePatterns`].
///
/// Supported forms: `*.ext`, `**/*.ext`, `**/dir/**`, `prefix/**`, `prefix/`,
/// and bare substrings.
fn matches_file_patterns(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| {
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
                && (path.len() == prefix.len() || path.as_bytes().get(prefix.len()) == Some(&b'/'));
        }
        // Default: contains match
        path.contains(p.trim_matches('*'))
    })
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

    // ─── helpers for routing / compression tests ───

    fn make_chunk(files: Vec<DiffFile>) -> DiffChunk {
        DiffChunk {
            files,
            chunk_index: 0,
            total_chunks: 1,
        }
    }

    fn make_file_with_lines(path: &str, lines: Vec<&str>) -> DiffFile {
        DiffFile {
            path: path.to_string(),
            old_path: path.to_string(),
            new_path: path.to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 0,
            hunks: vec![DiffHunk {
                header: "@@ -1 +1 @@".to_string(),
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                lines: lines
                    .into_iter()
                    .map(|c| DiffLine {
                        kind: if c.starts_with('-') {
                            DiffLineKind::Delete
                        } else {
                            DiffLineKind::Add
                        },
                        content: c.to_string(),
                        old_line_no: Some(1),
                        new_line_no: Some(1),
                    })
                    .collect(),
            }],
        }
    }

    fn make_expert(name: &str, trigger: ExpertTrigger, content_patterns: Vec<&str>) -> ExpertDef {
        ExpertDef {
            name: name.to_string(),
            trigger,
            prompt: String::new(),
            config: ExpertTomlDef {
                commands: vec!["review".to_string()],
                content_patterns: content_patterns.into_iter().map(String::from).collect(),
                ..Default::default()
            },
        }
    }

    fn assigned_paths(assignments: &[(&ExpertDef, Vec<DiffFile>)], name: &str) -> Vec<String> {
        assignments
            .iter()
            .find(|(e, _)| e.name == name)
            .map(|(_, files)| files.iter().map(|f| f.new_path.clone()).collect())
            .unwrap_or_default()
    }

    // ─── content-pattern routing ───

    #[test]
    fn test_route_chunks_content_patterns_priority() {
        let security = make_expert("security", ExpertTrigger::Always, vec!["token"]);
        let quality = make_expert("quality", ExpertTrigger::Always, vec![]);
        let experts = vec![security, quality];

        let auth = make_file_with_lines("src/auth.rs", vec!["+let token = fetch();"]);
        let plain = make_file_with_lines("src/plain.rs", vec!["+hello"]);
        let chunks = vec![make_chunk(vec![auth, plain])];

        let assignments = route_chunks(&chunks, &experts);

        // Content-matched file goes only to the content-matching expert.
        assert_eq!(
            assigned_paths(&assignments, "security"),
            vec!["src/auth.rs", "src/plain.rs"]
        );
        // Unmatched file still falls back to route-to-all.
        assert_eq!(assigned_paths(&assignments, "quality"), vec!["src/plain.rs"]);
    }

    #[test]
    fn test_route_chunks_content_patterns_override_file_patterns() {
        let security = make_expert(
            "security",
            ExpertTrigger::FilePatterns {
                patterns: vec!["*.rs".to_string()],
            },
            vec!["secret"],
        );
        let frontend = make_expert(
            "frontend",
            ExpertTrigger::FilePatterns {
                patterns: vec!["*.ts".to_string()],
            },
            vec![],
        );
        let experts = vec![security, frontend];

        let rust = make_file_with_lines("src/a.rs", vec!["+fn a() {}"]);
        let ts = make_file_with_lines("web/b.ts", vec!["+const b = 1;"]);
        let py = make_file_with_lines("app/c.py", vec!["+secret = 'x'"]);
        let chunks = vec![make_chunk(vec![rust, ts, py])];

        let assignments = route_chunks(&chunks, &experts);

        // c.py matches security's content_patterns, so content routing wins
        // even though "*.rs" does not match it.
        assert_eq!(assigned_paths(&assignments, "security"), vec!["src/a.rs", "app/c.py"]);
        assert_eq!(assigned_paths(&assignments, "frontend"), vec!["web/b.ts"]);
    }

    #[test]
    fn test_route_chunks_without_content_patterns_unchanged() {
        let rust_only = make_expert(
            "rust",
            ExpertTrigger::FilePatterns {
                patterns: vec!["*.rs".to_string()],
            },
            vec![],
        );
        let all = make_expert("all", ExpertTrigger::Always, vec![]);
        let experts = vec![rust_only, all];

        let chunks = vec![make_chunk(vec![
            make_file("src/a.rs", 1, 0),
            make_file("web/b.ts", 1, 0),
        ])];
        let assignments = route_chunks(&chunks, &experts);

        assert_eq!(assigned_paths(&assignments, "rust"), vec!["src/a.rs"]);
        assert_eq!(assigned_paths(&assignments, "all"), vec!["src/a.rs", "web/b.ts"]);
    }

    // ─── configured compression levels ───

    #[test]
    fn test_apply_configured_compression_none_skips() {
        let mut files = vec![
            make_file("src/main.rs", 10, 5),
            make_file("Cargo.lock", 10, 5),
            make_file_with_lines("src/deleted.rs", vec!["-gone"]),
        ];
        let (level, actions) = apply_configured_compression(&mut files, "none", &CompressionLevel::Aggressive);
        assert_eq!(level, CompressionLevel::None);
        assert!(actions.is_empty());
        assert_eq!(files.len(), 3); // nothing touched
    }

    #[test]
    fn test_apply_configured_compression_light_deletion_only() {
        let mut files = vec![
            make_file("src/main.rs", 10, 5),
            make_file("Cargo.lock", 10, 5),
            make_file_with_lines("src/deleted.rs", vec!["-gone"]),
        ];
        let (level, actions) = apply_configured_compression(&mut files, "light", &CompressionLevel::None);
        assert_eq!(level, CompressionLevel::Light);
        assert_eq!(actions.len(), 1);
        // Deletion-only file compressed; generated/vendor files kept at light.
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.new_path == "Cargo.lock"));
        assert!(!files.iter().any(|f| f.new_path == "src/deleted.rs"));
    }

    #[test]
    fn test_apply_configured_compression_medium() {
        let mut files = vec![
            make_file("src/main.rs", 10, 5),
            make_file("Cargo.lock", 10, 5),
            make_file_with_lines("src/deleted.rs", vec!["-gone"]),
        ];
        let (level, actions) = apply_configured_compression(&mut files, "medium", &CompressionLevel::None);
        assert_eq!(level, CompressionLevel::Medium);
        assert!(!actions.is_empty());
        // Medium = ignore-filter + deletion compression: both removed.
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].new_path, "src/main.rs");
    }

    #[test]
    fn test_apply_configured_compression_aggressive() {
        let mut files = vec![
            make_file("src/main.rs", 10, 5),
            make_file("Cargo.lock", 10, 5),
            make_file_with_lines("src/deleted.rs", vec!["-gone"]),
        ];
        let (level, actions) = apply_configured_compression(&mut files, "aggressive", &CompressionLevel::None);
        assert_eq!(level, CompressionLevel::Aggressive);
        assert!(!actions.is_empty());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].new_path, "src/main.rs");
    }

    #[test]
    fn test_apply_configured_compression_auto_and_unknown_fall_back() {
        // "auto" defers to the assessed level.
        let mut files = vec![make_file("Cargo.lock", 10, 5)];
        let (level, _) = apply_configured_compression(&mut files, "auto", &CompressionLevel::Medium);
        assert_eq!(level, CompressionLevel::Medium);
        assert!(files.is_empty()); // Cargo.lock filtered by medium

        // Unrecognised values also defer to the assessed level.
        let mut files = vec![make_file("src/main.rs", 10, 5)];
        let (level, actions) = apply_configured_compression(&mut files, "banana", &CompressionLevel::None);
        assert_eq!(level, CompressionLevel::None);
        assert!(actions.is_empty());
        assert_eq!(files.len(), 1);
    }
}
