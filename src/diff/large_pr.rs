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

/// Route chunks to experts.
///
/// The chunk is the atomic unit of routing: a chunk is never split across
/// experts, so `max_chunks_per_expert` is a *chunk* budget (root cause A —
/// the old code truncated per expert by **file** count, silently dropping
/// whole chunks and with them entire files).
///
/// Allocation strategy (designed for **coverage, not duplication**):
///
/// 1. **Mandatory coverage pass (round-robin).** Every chunk gets exactly one
///    primary owner: chunk `i` → expert `i % N` (round-robin over the experts
///    whose trigger accepts the chunk). This is what guarantees the union of
///    all experts' assignments covers every file in the diff. Experts with an
///    [`ExpertTrigger::FilePatterns`] trigger only accept chunks containing at
///    least one matching file; a chunk no expert accepts stays uncovered and
///    is reported by the coverage accounting downstream.
/// 2. **Content-pattern additive pass.** A chunk whose files contain any of an
///    expert's `content_patterns` is *additionally* routed to that expert
///    (bounded by `max_chunks_per_expert`, deduplicated). This is **additive,
///    not exclusive** (root cause B): a content-matched file is never removed
///    from the global coverage pool — it still has its round-robin owner and
///    is additionally reviewed by the specialized expert.
/// 3. **Balance pass (activate the team within quota).** The fair share is
///    `ceil(C / N)` chunks per expert, capped by `max_chunks_per_expert`. Idle
///    experts are given chunks they do not already hold (round-robin over
///    chunk index), so every expert has files to review while **no expert
///    receives more than its fair share** — the diff is spread across the
///    team instead of being route-to-all'd (which would blow the token
///    budget on large PRs).
/// 4. **Quota truncation (by chunk count).** Each expert keeps at most
///    `max_chunks_per_expert` chunks, preserving chunk boundaries. When the
///    quota is smaller than the per-expert coverage share
///    (`C > N × max_chunks_per_expert`) some chunks lose their only reviewer;
///    the coverage accounting downstream reports them and caps the score, so
///    under-coverage can never inflate the result.
///
/// Returns per-expert chunk-grouped file lists (one inner `Vec` per chunk).
/// Only experts whose `commands` include `review` participate.
pub fn route_chunks<'a>(
    chunks: &[DiffChunk],
    experts: &'a [ExpertDef],
    max_chunks_per_expert: usize,
) -> Vec<(&'a ExpertDef, Vec<Vec<DiffFile>>)> {
    let review_experts: Vec<&ExpertDef> = experts
        .iter()
        .filter(|e| e.config.commands.iter().any(|c| c == "review"))
        .collect();
    if review_experts.is_empty() {
        return Vec::new();
    }

    let n = review_experts.len();
    // chunk indices assigned to each expert
    let mut assigned: Vec<Vec<usize>> = vec![Vec::new(); n];

    // Does the expert accept this chunk given its trigger?
    let accepts = |expert: &ExpertDef, chunk: &DiffChunk| -> bool {
        match &expert.trigger {
            ExpertTrigger::FilePatterns { patterns } => {
                chunk.files.iter().any(|f| matches_file_patterns(patterns, &f.new_path))
            }
            _ => true,
        }
    };

    // Phase 1 — mandatory coverage: round-robin primary ownership. Every chunk
    // gets exactly one owner, so the union of all assignments covers every
    // file (unless no expert's trigger accepts the chunk at all).
    for (ci, chunk) in chunks.iter().enumerate() {
        let candidates: Vec<usize> = (0..n).filter(|&e| accepts(review_experts[e], chunk)).collect();
        if candidates.is_empty() {
            tracing::warn!(
                "route_chunks: chunk {} matches no expert trigger; it cannot be covered",
                ci
            );
            continue;
        }
        let owner = candidates[ci % candidates.len()];
        assigned[owner].push(ci);
    }

    // Phase 2 — content-pattern additive routing (bounded by quota,
    // deduplicated against the coverage pass). Never exclusive.
    let any_content_patterns = review_experts.iter().any(|e| !e.config.content_patterns.is_empty());
    if any_content_patterns {
        for (ci, chunk) in chunks.iter().enumerate() {
            for (e, expert) in review_experts.iter().enumerate() {
                if expert.config.content_patterns.is_empty() || assigned[e].contains(&ci) {
                    continue;
                }
                let matched = chunk.files.iter().any(|f| {
                    let text = render_file_diff(f);
                    expert.config.content_patterns.iter().any(|p| text.contains(p.as_str()))
                });
                if matched && (max_chunks_per_expert == 0 || assigned[e].len() < max_chunks_per_expert) {
                    assigned[e].push(ci);
                }
            }
        }
    }

    // Phase 3 — balance: activate idle experts within their fair share so the
    // whole team reviews, without route-to-all. Fair share = ceil(C / N)
    // chunks per expert, capped by the quota. Only adds chunks the expert does
    // not already hold and that its trigger accepts; coverage from Phase 1 is
    // never reduced.
    if max_chunks_per_expert > 0 {
        let fair_share = chunks.len().div_ceil(n).min(max_chunks_per_expert);
        for e in 0..n {
            while assigned[e].len() < fair_share {
                let next =
                    (0..chunks.len()).find(|&ci| !assigned[e].contains(&ci) && accepts(review_experts[e], &chunks[ci]));
                match next {
                    Some(ci) => assigned[e].push(ci),
                    None => break,
                }
            }
        }
    }

    // Phase 4 — quota truncation by chunk count. Coverage was established in
    // Phase 1; dropping an expert's tail chunks only loses coverage when the
    // quota is smaller than the coverage share — that shortfall is surfaced
    // honestly by the coverage accounting downstream (unreviewed files list +
    // score cap), never silently.
    let mut quota_shortfall = false;
    if max_chunks_per_expert > 0 {
        for list in &mut assigned {
            if list.len() > max_chunks_per_expert {
                quota_shortfall = true;
                list.truncate(max_chunks_per_expert);
            }
        }
    }
    if quota_shortfall {
        tracing::warn!(
            "route_chunks: max_chunks_per_expert={} below coverage share ({} chunks across {} experts); \
             some files may be unreviewed and will be reported by coverage accounting",
            max_chunks_per_expert,
            chunks.len(),
            n
        );
    }

    review_experts
        .into_iter()
        .zip(assigned)
        .filter(|(_, list)| !list.is_empty())
        .map(|(expert, list)| {
            let groups = list
                .iter()
                .map(|&ci| chunks[ci].files.clone())
                .collect::<Vec<Vec<DiffFile>>>();
            (expert, groups)
        })
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

    fn assigned_paths(assignments: &[(&ExpertDef, Vec<Vec<DiffFile>>)], name: &str) -> Vec<String> {
        assignments
            .iter()
            .find(|(e, _)| e.name == name)
            .map(|(_, groups)| groups.iter().flatten().map(|f| f.new_path.clone()).collect())
            .unwrap_or_default()
    }

    // ─── content-pattern routing (additive, not exclusive) ───

    #[test]
    fn test_route_chunks_content_patterns_additive_not_exclusive() {
        // Root cause B: a content-matched file must still be visible to the
        // rest of the team. With chunk 1 (`auth.rs` containing "token") owned
        // by `quality` via the coverage pass, `security` additionally receives
        // it through the content-pattern pass — and `quality` keeps it too.
        let security = make_expert("security", ExpertTrigger::Always, vec!["token"]);
        let quality = make_expert("quality", ExpertTrigger::Always, vec![]);
        let experts = vec![security, quality];

        let chunks = vec![
            make_chunk(vec![make_file_with_lines("src/plain.rs", vec!["+hello"])]),
            make_chunk(vec![make_file_with_lines("src/auth.rs", vec!["+let token = fetch();"])]),
        ];

        // quota 0 = unlimited: pure coverage + additive content routing.
        let assignments = route_chunks(&chunks, &experts, 0);

        // security sees the content-matched file (additive route) ...
        assert!(assigned_paths(&assignments, "security").contains(&"src/auth.rs".to_string()));
        // ... and quality (the round-robin owner) still sees it: not exclusive.
        assert!(assigned_paths(&assignments, "quality").contains(&"src/auth.rs".to_string()));
        // plain.rs is owned by security (chunk 0 → expert 0).
        assert!(assigned_paths(&assignments, "security").contains(&"src/plain.rs".to_string()));

        // Union of all experts' assignments covers every file.
        let union: std::collections::HashSet<&str> = assignments
            .iter()
            .flat_map(|(_, groups)| groups.iter().flatten())
            .map(|f| f.new_path.as_str())
            .collect();
        assert_eq!(union.len(), 2);
        assert!(union.contains("src/auth.rs") && union.contains("src/plain.rs"));
    }

    #[test]
    fn test_route_chunks_file_patterns_still_covered() {
        // Even with FilePatterns triggers, the union of all experts'
        // assignments still covers every file: chunk-atomic routing never
        // drops a file from the team's review pool.
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

        // One chunk holding a rust + ts file: owned by security (first
        // candidate); the ts file is still routed to frontend through the
        // coverage pass because frontend accepts chunks with a *.ts file.
        let mixed = make_chunk(vec![
            make_file_with_lines("src/a.rs", vec!["+fn a() {}"]),
            make_file_with_lines("web/b.ts", vec!["+const b = 1;"]),
        ]);
        let assignments = route_chunks(&[mixed], &experts, 0);

        let union: std::collections::HashSet<&str> = assignments
            .iter()
            .flat_map(|(_, groups)| groups.iter().flatten())
            .map(|f| f.new_path.as_str())
            .collect();
        assert_eq!(union.len(), 2, "every file keeps at least one reviewer");
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

        let chunks = vec![
            make_chunk(vec![make_file("src/a.rs", 1, 0)]),
            make_chunk(vec![make_file("web/b.ts", 1, 0)]),
        ];
        // quota 0 = unlimited: coverage round-robin assigns chunk 0 to rust
        // and chunk 1 to all; both accept both chunks here.
        let assignments = route_chunks(&chunks, &experts, 0);

        assert_eq!(assigned_paths(&assignments, "rust"), vec!["src/a.rs"]);
        assert_eq!(assigned_paths(&assignments, "all"), vec!["web/b.ts"]);
    }

    // ─── coverage guarantee + chunk-quota semantics (root cause A) ───

    #[test]
    fn test_route_chunks_covers_all_files() {
        // Core acceptance: for a >21-file diff the union of every expert's
        // assignment must cover every file.
        let experts = vec![
            make_expert("e1", ExpertTrigger::Always, vec![]),
            make_expert("e2", ExpertTrigger::Always, vec![]),
            make_expert("e3", ExpertTrigger::Always, vec![]),
        ];
        let files: Vec<DiffFile> = (0..24)
            .map(|i| make_file(&format!("src/file{:02}.rs", i), 5, 0))
            .collect();
        let chunks: Vec<DiffChunk> = files.chunks(4).map(|c| make_chunk(c.to_vec())).collect();
        assert_eq!(chunks.len(), 6);

        let assignments = route_chunks(&chunks, &experts, 3);
        assert!(!assignments.is_empty());

        let covered: std::collections::HashSet<&str> = assignments
            .iter()
            .flat_map(|(_, groups)| groups.iter().flatten())
            .map(|f| f.new_path.as_str())
            .collect();
        assert_eq!(covered.len(), 24);
        assert!(files.iter().all(|f| covered.contains(f.new_path.as_str())));
    }

    #[test]
    fn test_route_chunks_quota_counts_chunks_not_files() {
        // Root cause A: `max_chunks_per_expert = 2` must keep 2 CHUNKS
        // (4 files in 2 groups of 2), not 2 files.
        let experts = vec![make_expert("only", ExpertTrigger::Always, vec![])];
        let files: Vec<DiffFile> = (0..6).map(|i| make_file(&format!("f{}.rs", i), 1, 0)).collect();
        let chunks: Vec<DiffChunk> = files.chunks(2).map(|c| make_chunk(c.to_vec())).collect();

        let assignments = route_chunks(&chunks, &experts, 2);
        assert_eq!(assignments.len(), 1);
        let (_, groups) = &assignments[0];
        assert_eq!(groups.len(), 2, "quota bounds by chunk count, not file count");
        let total_files: usize = groups.iter().map(|g| g.len()).sum();
        assert_eq!(total_files, 4, "2 chunks × 2 files each");
    }

    #[test]
    fn test_route_chunks_balance_activates_all_experts_without_route_to_all() {
        // 3 chunks across 5 experts with quota 3: fair share = ceil(3/5) = 1
        // chunk per expert, so every expert gets work but no expert sees the
        // whole diff (no route-to-all), and the union still covers every file.
        let experts = vec![
            make_expert("e0", ExpertTrigger::Always, vec![]),
            make_expert("e1", ExpertTrigger::Always, vec![]),
            make_expert("e2", ExpertTrigger::Always, vec![]),
            make_expert("e3", ExpertTrigger::Always, vec![]),
            make_expert("e4", ExpertTrigger::Always, vec![]),
        ];
        let chunks: Vec<DiffChunk> = vec![
            make_chunk(vec![make_file("a.rs", 1, 0), make_file("b.rs", 1, 0)]),
            make_chunk(vec![make_file("c.rs", 1, 0), make_file("d.rs", 1, 0)]),
            make_chunk(vec![make_file("e.rs", 1, 0), make_file("f.rs", 1, 0)]),
        ];

        let assignments = route_chunks(&chunks, &experts, 3);
        assert_eq!(assignments.len(), 5, "every expert gets at least one chunk");
        for (_, groups) in &assignments {
            assert!(!groups.is_empty());
            assert!(groups.len() <= 3, "no expert exceeds its quota");
        }
        // No expert received the full diff (no route-to-all).
        assert!(assignments.iter().all(|(_, groups)| groups.len() < 3));
        // Union covers every file.
        let union: std::collections::HashSet<&str> = assignments
            .iter()
            .flat_map(|(_, groups)| groups.iter().flatten())
            .map(|f| f.new_path.as_str())
            .collect();
        assert_eq!(union.len(), 6);
    }

    #[test]
    fn test_route_chunks_respects_chunk_boundaries() {
        // Each expert's output is grouped by source chunk; a chunk is never
        // split across groups.
        let experts = vec![make_expert("e1", ExpertTrigger::Always, vec![])];
        let chunks: Vec<DiffChunk> = vec![
            make_chunk(vec![make_file("a.rs", 1, 0), make_file("b.rs", 1, 0)]),
            make_chunk(vec![make_file("c.rs", 1, 0), make_file("d.rs", 1, 0)]),
        ];
        let assignments = route_chunks(&chunks, &experts, 0);
        let (_, groups) = &assignments[0];
        assert_eq!(groups.len(), 2);
        assert!(groups[0].iter().all(|f| f.new_path == "a.rs" || f.new_path == "b.rs"));
        assert!(groups[1].iter().all(|f| f.new_path == "c.rs" || f.new_path == "d.rs"));
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
