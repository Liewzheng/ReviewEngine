use super::*;

#[test]
fn version() {
    let output = run(&["--version"], None);
    assert!(output.status.success(), "--version failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Review Engine v"),
        "expected version string, got: {}",
        stdout
    );
}

#[test]
fn help() {
    let output = run(&["--help"], None);
    assert!(output.status.success(), "--help failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("repo-review"), "missing repo-review subcommand");
    assert!(stdout.contains("review"), "missing review subcommand");
    assert!(stdout.contains("serve"), "missing serve subcommand");
    assert!(stdout.contains("validate"), "missing validate subcommand");
    assert!(stdout.contains("upgrade"), "missing upgrade subcommand");
}

#[test]
fn help_lists_audit_and_repo_review() {
    let output = run(&["--help"], None);
    assert!(output.status.success(), "--help failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("audit"),
        "root --help missing visible alias 'audit': {}",
        stdout
    );
    assert!(
        stdout.contains("repo-review"),
        "root --help missing 'repo-review': {}",
        stdout
    );
}

#[test]
fn audit_is_visible_alias_of_repo_review() {
    let audit = run(&["audit", "--help"], None);
    assert!(audit.status.success(), "audit --help failed: {:?}", audit);
    let audit_stdout = String::from_utf8_lossy(&audit.stdout);

    let repo = run(&["repo-review", "--help"], None);
    assert!(repo.status.success(), "repo-review --help failed: {:?}", repo);
    let repo_stdout = String::from_utf8_lossy(&repo.stdout);

    // Key content of `audit --help` must match `repo-review --help`: the
    // same about text and the same options.
    for key in [
        "Run a full repository health review",
        "--local-path",
        "--config",
        "--llm-config",
        "--format",
        "--output",
    ] {
        assert!(
            audit_stdout.contains(key),
            "audit --help missing {:?}: {}",
            key,
            audit_stdout
        );
        assert!(
            repo_stdout.contains(key),
            "repo-review --help missing {:?}: {}",
            key,
            repo_stdout
        );
    }
}

/// The displayed program name follows argv[0]'s basename, so a `reng`
/// symlink shows `reng` in help/usage instead of `review-engine`.
#[cfg(unix)]
#[test]
fn bin_name_follows_argv0_via_symlink() {
    use std::os::unix::fs::symlink;

    let bin = bin_path();
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("reng");
    symlink(&bin, &link).expect("failed to create reng symlink");

    let help = Command::new(&link)
        .arg("--help")
        .output()
        .expect("failed to run reng --help");
    assert!(help.status.success(), "reng --help failed: {:?}", help);
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    assert!(
        help_stdout.contains("Usage: reng"),
        "expected dynamic bin name 'reng' in help, got: {}",
        help_stdout
    );
    assert!(
        help_stdout.contains("audit"),
        "reng --help missing audit alias: {}",
        help_stdout
    );
    assert!(
        help_stdout.contains("repo-review"),
        "reng --help missing repo-review: {}",
        help_stdout
    );

    let version = Command::new(&link)
        .arg("--version")
        .output()
        .expect("failed to run reng --version");
    assert!(version.status.success(), "reng --version failed: {:?}", version);
    let version_stdout = String::from_utf8_lossy(&version.stdout);
    assert!(
        version_stdout.contains("Review Engine v"),
        "version output format changed under symlink: {}",
        version_stdout
    );
}

#[test]
fn init_default() {
    let dir = TempDir::new().unwrap();
    let output = run(&["init", "--default"], Some(dir.path()));
    assert!(output.status.success(), "init --default failed: {:?}", output);
    let config_path = dir.path().join(".code-audit-config.toml");
    assert!(config_path.exists(), ".code-audit-config.toml was not created");
}

#[test]
fn validate_valid_config() {
    let dir = TempDir::new().unwrap();
    let config = r#"
[commands]
repo_review = true
"#;
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, config).unwrap();

    let output = run(
        &["validate", "--config", config_path.to_str().unwrap()],
        Some(dir.path()),
    );
    assert!(
        output.status.success(),
        "validate failed for valid config: {:?}",
        output
    );
}

#[test]
fn validate_invalid_config() {
    let dir = TempDir::new().unwrap();
    // Overriding lead weight without disabling other default experts makes the
    // enabled experts' weights sum to more than 100.
    let config = r#"
[review_experts.lead]
weight = 99
"#;
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, config).unwrap();

    let output = run(
        &["validate", "--config", config_path.to_str().unwrap()],
        Some(dir.path()),
    );
    assert!(
        !output.status.success(),
        "validate should have failed for invalid config"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("error"),
        "stderr did not contain expected error: {}",
        stderr
    );
}

#[test]
fn repo_review_local_only() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path();

    git_init(repo_path);
    git_config_user(repo_path);

    let src_dir = repo_path.join("src");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("main.rs"),
        "fn main() {\n    println!(\"Hello, world!\");\n}\n",
    )
    .unwrap();

    let config_path = repo_path.join(".code-audit-config.toml");
    std::fs::write(
        &config_path,
        r#"
[commands]
repo_review = true
"#,
    )
    .unwrap();

    git_add_and_commit(repo_path, "initial commit");

    let report_path = repo_path.join("report.json");
    let output = run(
        &[
            "repo-review",
            "--local-path",
            ".",
            "--format",
            "json",
            "--output",
            report_path.to_str().unwrap(),
        ],
        Some(repo_path),
    );
    assert!(output.status.success(), "repo-review failed: {:?}", output);

    assert!(report_path.exists(), "report.json was not created");

    let content = std::fs::read_to_string(&report_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).expect("report.json is not valid JSON");

    // The report should contain scoring, summary, and expert information.
    assert!(value.get("overview").is_some(), "missing overview");
    assert!(value["overview"].get("health_score").is_some(), "missing health_score");
    assert!(value.get("expert_scores").is_some(), "missing expert_scores");
    assert!(value.get("conclusion").is_some(), "missing conclusion");
}
