use super::*;
use crate::repo::experts::{ExpertScore, RepoContext, RepoExpert};
use crate::repo::{FileEntry, RepoStats};

fn entry(path: &str, language: &str, loc: usize) -> FileEntry {
    FileEntry {
        path: path.to_string(),
        language: language.to_string(),
        loc,
        is_binary: false,
        is_generated: false,
    }
}

fn ctx(entries: Vec<FileEntry>) -> RepoContext {
    let stats = RepoStats {
        total_files: entries.len(),
        total_loc: entries.iter().map(|e| e.loc).sum(),
        ..Default::default()
    };
    RepoContext {
        entries,
        stats,
        llm_configs: Vec::new(),
        config: None,
        facts_block: None,
    }
}

async fn evaluate<E: RepoExpert + ?Sized>(expert: &E, context: &RepoContext) -> ExpertScore {
    expert
        .evaluate(context, None)
        .await
        .expect("static expert should not fail")
}

/// Build a temp fixture repo that triggers every static finding: a
/// credential leak (security), a 600-line file (code_organization),
/// a Cargo.lock with 201 packages (dependency), and nothing else so the
/// documentation/code_style "missing file" findings fire. Returns the
/// context plus the TempDir keep-alive handle.
fn fixture_context() -> (RepoContext, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let big_path = dir.path().join("src").join("big.rs");
    std::fs::create_dir_all(big_path.parent().unwrap()).unwrap();
    let big_body = (0..600)
        .map(|i| format!("fn f{i}() {{}}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&big_path, big_body).unwrap();

    let secret_path = dir.path().join("config").join("secret.env");
    std::fs::create_dir_all(secret_path.parent().unwrap()).unwrap();
    std::fs::write(&secret_path, "api_key = \"aaaaaaaaaaaaaaaa\"\n").unwrap();

    let lock_path = dir.path().join("Cargo.lock");
    let mut lock = String::from("version = 3\n\n");
    for i in 0..201 {
        lock.push_str(&format!("[[package]]\nname = \"pkg{i}\"\nversion = \"0.1.0\"\n\n"));
    }
    std::fs::write(&lock_path, lock).unwrap();

    let entries = vec![
        entry(big_path.to_str().unwrap(), "Rust", 600),
        entry(secret_path.to_str().unwrap(), "Config", 1),
        entry(lock_path.to_str().unwrap(), "Config", 604),
    ];
    (ctx(entries), dir)
}

#[tokio::test]
async fn security_details_len_matches_summary_and_has_no_banner() {
    let (context, _dir) = fixture_context();
    let score = evaluate(&Security, &context).await;
    assert!(!score.details.is_empty(), "fixture must contain a credential hit");

    // Summary count must equal the rendered details count (the old
    // synthetic banner inflated details by one and made them diverge).
    let summary_count: usize = score
        .summary
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("summary should start with a count");
    assert_eq!(summary_count, score.details.len());

    // No synthetic banner: every detail is a real hit with a file path,
    // and none claims a bare "N security patterns detected" count.
    for d in &score.details {
        assert!(d.file.is_some(), "banner pseudo-finding must be gone: {}", d.message);
        assert!(
            !d.message.ends_with(" security patterns detected"),
            "banner message should not exist: {}",
            d.message
        );
    }
}

#[tokio::test]
async fn static_findings_have_recommendation_and_effort() {
    let (context, _dir) = fixture_context();
    let experts: Vec<Box<dyn RepoExpert>> = vec![
        Box::new(CodeOrganization),
        Box::new(Security),
        Box::new(Documentation),
        Box::new(Dependency),
        Box::new(CodeStyle),
    ];
    for expert in &experts {
        let score = evaluate(expert.as_ref(), &context).await;
        for d in &score.details {
            let rec = d
                .recommendation
                .as_deref()
                .unwrap_or_else(|| panic!("{} detail missing recommendation: {}", score.expert_name, d.message));
            assert!(
                !rec.trim().is_empty(),
                "{} detail has empty recommendation: {}",
                score.expert_name,
                d.message
            );
            let effort = d
                .effort
                .as_deref()
                .unwrap_or_else(|| panic!("{} detail missing effort: {}", score.expert_name, d.message));
            assert!(
                ["trivial", "small", "medium", "large"].contains(&effort),
                "{} detail has unexpected effort {effort:?}: {}",
                score.expert_name,
                d.message
            );
        }
    }
}

#[tokio::test]
async fn security_clean_repo_has_no_findings() {
    // A context with no credentials must score 100 and emit no details —
    // including no synthetic "0 security patterns detected" banner.
    let context = ctx(vec![entry("src/main.rs", "Rust", 10)]);
    let score = evaluate(&Security, &context).await;
    assert_eq!(score.score, 100);
    assert!(score.details.is_empty());
    assert_eq!(score.summary, "0 security findings");
}

// ─── CodeStyle normalisation ───────────────────────────────

#[tokio::test]
async fn code_style_full_score_when_all_applicable_configs_present() {
    // Single-language Rust repo with every applicable config: the
    // always-applicable .editorconfig plus the rustfmt and clippy tool
    // groups (`.rustfmt.toml` satisfies the rustfmt group — aliases
    // count once, not twice).
    let context = ctx(vec![
        entry("src/main.rs", "Rust", 10),
        entry(".editorconfig", "Other", 5),
        entry(".rustfmt.toml", "Config", 3),
        entry("clippy.toml", "Config", 3),
    ]);
    let score = evaluate(&CodeStyle, &context).await;
    assert_eq!(score.score, 100, "summary: {}", score.summary);
    assert!(
        score.details.is_empty(),
        "no findings expected, got: {:?}",
        score.details.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert!(score.summary.contains("3/3"), "summary: {}", score.summary);
}

#[tokio::test]
async fn code_style_missing_configs_emit_note_findings() {
    // Rust repo with only rustfmt configured: 1 of 3 applicable items.
    let context = ctx(vec![
        entry("src/main.rs", "Rust", 10),
        entry("rustfmt.toml", "Config", 3),
    ]);
    let score = evaluate(&CodeStyle, &context).await;
    assert_eq!(score.score, 33, "summary: {}", score.summary);
    assert_eq!(score.details.len(), 2);
    let messages: Vec<&str> = score.details.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains(".editorconfig")),
        "expected a missing-.editorconfig finding: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("clippy.toml")),
        "expected a missing-clippy finding: {messages:?}"
    );
    for d in &score.details {
        assert_eq!(d.severity, "note");
        assert!(
            d.recommendation.is_some(),
            "finding must say how to fix it: {}",
            d.message
        );
    }
}

#[tokio::test]
async fn code_style_normalises_across_languages_and_groups_aliases() {
    // Rust + Python; ruff configured via the `.ruff.toml` alias, mypy
    // missing. Applicable: editorconfig + rustfmt + clippy + ruff +
    // mypy = 5; satisfied = 4.
    let context = ctx(vec![
        entry("src/main.rs", "Rust", 10),
        entry("app.py", "Python", 20),
        entry(".editorconfig", "Other", 5),
        entry("rustfmt.toml", "Config", 3),
        entry("clippy.toml", "Config", 3),
        entry(".ruff.toml", "Config", 3),
    ]);
    let score = evaluate(&CodeStyle, &context).await;
    assert_eq!(score.score, 80, "summary: {}", score.summary);
    assert_eq!(score.details.len(), 1);
    assert!(
        score.details[0].message.contains("mypy.ini"),
        "the only finding should be the missing mypy config: {}",
        score.details[0].message
    );
}

#[test]
fn style_tool_key_folds_modern_flat_config_aliases() {
    // Modern eslint flat configs (`eslint.config.*`) and the legacy
    // `.eslintrc*` family configure the same tool and must collapse to
    // one group key; ditto `prettier.config.*` / `.prettierrc*`.
    for modern in [
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.cjs",
        "eslint.config.ts",
    ] {
        assert_eq!(style_tool_key(modern), "eslint", "{modern}");
    }
    for legacy in [".eslintrc", ".eslintrc.json", ".eslintrc.cjs", ".eslintrc.yaml"] {
        assert_eq!(style_tool_key(legacy), "eslint", "{legacy}");
    }
    for modern in [
        "prettier.config.js",
        "prettier.config.mjs",
        "prettier.config.cjs",
        "prettier.config.ts",
    ] {
        assert_eq!(style_tool_key(modern), "prettier", "{modern}");
    }
    for legacy in [".prettierrc", ".prettierrc.json", ".prettierrc.yaml", ".prettierrc.js"] {
        assert_eq!(style_tool_key(legacy), "prettier", "{legacy}");
    }
    // Unrelated tools keep their own keys; .editorconfig stays distinct.
    assert_eq!(style_tool_key(".editorconfig"), "editorconfig");
    assert_eq!(style_tool_key("rustfmt.toml"), "rustfmt");
    assert_eq!(style_tool_key("clippy.toml"), "clippy");
}

#[tokio::test]
async fn code_style_recognises_modern_eslint_prettier_configs() {
    // A JS/TS repo configured only with the modern flat config names
    // (exactly what the frontend now ships) must score 100: the
    // eslint group is satisfied by `eslint.config.js` and the prettier
    // group by `prettier.config.js` — no legacy `.eslintrc` needed.
    let context = ctx(vec![
        entry("src/main.ts", "TypeScript", 10),
        entry("src/App.vue", "Vue", 20),
        entry(".editorconfig", "Other", 5),
        entry("eslint.config.js", "Config", 3),
        entry("prettier.config.js", "Config", 3),
    ]);
    let score = evaluate(&CodeStyle, &context).await;
    assert_eq!(score.score, 100, "summary: {}", score.summary);
    assert!(
        score.details.is_empty(),
        "no findings expected, got: {:?}",
        score.details.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    assert!(score.summary.contains("3/3"), "summary: {}", score.summary);
}
