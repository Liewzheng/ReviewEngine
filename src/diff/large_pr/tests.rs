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
