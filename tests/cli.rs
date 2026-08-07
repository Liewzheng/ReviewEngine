#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn bin_path() -> String {
    std::env::var("CARGO_BIN_EXE_review-engine").unwrap_or_else(|_| "target/debug/review-engine".to_string())
}

fn run(args: &[&str], current_dir: Option<&Path>) -> std::process::Output {
    let mut cmd = Command::new(bin_path());
    cmd.args(args);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    cmd.output().expect("failed to execute review-engine")
}

fn git_init(path: &Path) {
    run_git(path, &["init"])
}

fn git_config_user(path: &Path) {
    run_git(path, &["config", "user.email", "test@example.com"]);
    run_git(path, &["config", "user.name", "Test User"]);
}

fn git_add_and_commit(path: &Path, message: &str) {
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", message]);
}

fn run_git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(path)
        .status()
        .expect("git is not available");
    assert!(status.success(), "git command failed: git {:?}", args);
}

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

// ─────────────────────────────────────────────────────────────────────
// `upgrade` subcommand — self-update.
//
// The upgrade library (`review_engine::upgrade`) hardcodes the GitHub API
// base URL and only exposes its test seam to its own unit tests, so these
// CLI tests drive the binary through the documented env overrides in
// `src/cli/handlers.rs`:
//   REVIEW_UPGRADE_TEST_RELEASE     fake release metadata (bypasses GitHub API)
//   REVIEW_UPGRADE_CURRENT_VERSION  fake current version
//   REVIEW_UPGRADE_INSTALL_METHOD   force the install method
//   REVIEW_UPGRADE_EXE              target exe for self-replace / rollback
// Asset and checksum downloads are served by a local wiremock server.
// ─────────────────────────────────────────────────────────────────────

fn shasum(data: &[u8]) -> String {
    review_engine::upgrade::verify::data_sha256_hex(data)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Build a single-entry `.tar.gz` (stored deflate block) containing `content`
/// as `name` with the given octal mode — valid enough for the `tar` crate.
fn single_file_tar_gz(name: &str, content: &[u8], mode: u32) -> Vec<u8> {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    assert!(nb.len() <= 100, "tar name too long: {name}");
    h[..nb.len()].copy_from_slice(nb);
    h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", content.len() as u64).as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = b'0'; // regular file
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h[..148].iter().map(|b| *b as u32).sum::<u32>()
        + 8 * (b' ' as u32)
        + h[156..].iter().map(|b| *b as u32).sum::<u32>();
    let chksum = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(chksum.as_bytes());

    let mut tar = Vec::new();
    tar.extend_from_slice(&h);
    tar.extend_from_slice(content);
    let pad = (512 - content.len() % 512) % 512;
    tar.extend(std::iter::repeat(0u8).take(pad));
    tar.extend_from_slice(&[0u8; 1024]);

    let mut out = Vec::new();
    out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff]);
    out.push(0x01); // final, stored deflate block
    let len = u16::try_from(tar.len()).expect("archive too large for one stored block");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(&tar);
    out.extend_from_slice(&crc32(&tar).to_le_bytes());
    out.extend_from_slice(&(tar.len() as u32).to_le_bytes());
    out
}

fn fake_binary(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"Review Engine v{version}\"\n").into_bytes()
}

fn test_release_json(tag: &str, asset_url: &str, asset_size: u64, checksum_url: &str, checksum_size: u64) -> String {
    format!(
        r#"{{"tag":"{tag}","asset_name":"review-engine-test.tar.gz","asset_url":"{asset_url}","asset_size":{asset_size},"checksum_url":"{checksum_url}","checksum_size":{checksum_size}}}"#
    )
}

fn run_with_env(args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(bin_path());
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to execute review-engine")
}

/// Mount a wiremock release: the asset archive plus a two-line `.sha256`
/// sidecar (line 1 = archive hash for `download_verified_asset`, line 2 =
/// binary hash for the post-extract double-check). Returns (asset_url,
/// checksum_url, asset_size, checksum_size).
async fn mount_release(server: &wiremock::MockServer, archive: &[u8], binary_hex: &str) -> (String, String, u64, u64) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let sidecar = format!(
        "{}  review-engine-test.tar.gz\n{}  review-engine\n",
        shasum(archive),
        binary_hex
    );
    Mock::given(method("GET"))
        .and(path("/asset.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive.to_vec()))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/asset.tar.gz.sha256"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sidecar.clone()))
        .mount(server)
        .await;
    (
        format!("{}/asset.tar.gz", server.uri()),
        format!("{}/asset.tar.gz.sha256", server.uri()),
        archive.len() as u64,
        sidecar.len() as u64,
    )
}

#[test]
fn upgrade_check_reports_update_by_install_method() {
    let cases: [(&str, &str, &str); 5] = [
        ("brew", "Homebrew", "brew upgrade review-engine"),
        (
            "cargo",
            "Cargo (~/.cargo/bin)",
            "cargo install review-engine --locked --features cli",
        ),
        ("docker", "Docker 容器", "git pull && docker compose up -d --build"),
        ("plain", "直接部署的二进制", "reng upgrade"),
        ("unknown", "未知（手动安装）", "使用官方 install.sh 手动升级"),
    ];
    for (method, label, cmd) in cases {
        let release = test_release_json(
            "v9.9.9",
            "http://127.0.0.1:1/asset",
            1,
            "http://127.0.0.1:1/asset.sha256",
            1,
        );
        let output = run_with_env(
            &["upgrade", "--check"],
            &[
                ("REVIEW_UPGRADE_INSTALL_METHOD", method),
                ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
                ("REVIEW_UPGRADE_TEST_RELEASE", &release),
            ],
        );
        assert!(
            output.status.success(),
            "{method}: upgrade --check failed: {:?}",
            output
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("A newer version of review-engine is available (0.8.2 -> 9.9.9)."),
            "{method}: {stdout}"
        );
        assert!(
            stdout.contains(&format!("Detected install source: {label}.")),
            "{method}: {stdout}"
        );
        assert!(stdout.contains(&format!("To update, run: {cmd}")), "{method}: {stdout}");
    }
}

#[test]
fn upgrade_check_up_to_date() {
    let release = test_release_json("v9.9.9", "http://127.0.0.1:1/a", 1, "http://127.0.0.1:1/s", 1);
    let output = run_with_env(
        &["upgrade", "--check"],
        &[
            ("REVIEW_UPGRADE_CURRENT_VERSION", "9.9.9"),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(output.status.success(), "upgrade --check failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("review-engine is up to date (v9.9.9)"), "{stdout}");
}

#[test]
fn upgrade_target_version_must_be_latest() {
    let release = test_release_json("v9.9.9", "http://127.0.0.1:1/a", 1, "http://127.0.0.1:1/s", 1);

    // --version equal to the latest → normal update info.
    let ok = run_with_env(
        &["upgrade", "--check", "--version", "9.9.9"],
        &[
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(ok.status.success(), "upgrade --check --version 9.9.9 failed: {:?}", ok);
    assert!(String::from_utf8_lossy(&ok.stdout).contains("0.8.2 -> 9.9.9"));

    // --version equal to the current version → up to date.
    let current = run_with_env(
        &["upgrade", "--check", "--version", "0.8.2"],
        &[
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(
        current.status.success(),
        "upgrade --check --version 0.8.2 failed: {:?}",
        current
    );
    assert!(String::from_utf8_lossy(&current.stdout).contains("review-engine is up to date (v0.8.2)"));

    // --version different from the latest → clear error, no auto-install.
    let bad = run_with_env(
        &["upgrade", "--check", "--version", "9.9.8"],
        &[
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(!bad.status.success(), "--version 9.9.8 should fail: {:?}", bad);
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("cannot auto-upgrade to v9.9.8"), "{stderr}");

    // --version with a non-stable tag → clear error.
    let bad_tag = run_with_env(
        &["upgrade", "--check", "--version", "abc"],
        &[
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(!bad_tag.status.success(), "--version abc should fail: {:?}", bad_tag);
    let stderr = String::from_utf8_lossy(&bad_tag.stderr);
    assert!(stderr.contains("invalid target version"), "{stderr}");
}

#[test]
fn brew_without_yes_prints_hint_only() {
    let release = test_release_json("v9.9.9", "http://127.0.0.1:1/a", 1, "http://127.0.0.1:1/s", 1);
    let output = run_with_env(
        &["upgrade"],
        &[
            ("REVIEW_UPGRADE_INSTALL_METHOD", "brew"),
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(
        output.status.success(),
        "brew default must not execute anything: {:?}",
        output
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Run again with --yes"), "{stdout}");
}

#[tokio::test]
async fn plain_upgrade_replaces_binary_and_keeps_bak() {
    let server = wiremock::MockServer::start().await;
    let script = fake_binary("9.9.9");
    let archive = single_file_tar_gz("review-engine", &script, 0o755);
    let (asset_url, checksum_url, asset_size, checksum_size) = mount_release(&server, &archive, &shasum(&script)).await;

    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("review-engine");
    std::fs::write(&exe, fake_binary("0.8.2")).unwrap();

    let release = test_release_json("v9.9.9", &asset_url, asset_size, &checksum_url, checksum_size);
    let output = run_with_env(
        &["upgrade", "--yes"],
        &[
            ("REVIEW_UPGRADE_INSTALL_METHOD", "plain"),
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_EXE", exe.to_str().unwrap()),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(output.status.success(), "plain upgrade failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("downloading"), "{stdout}");
    assert!(stdout.contains("installing"), "{stdout}");
    assert!(stdout.contains("done. Upgraded review-engine to v9.9.9."), "{stdout}");

    // New binary in place, previous binary preserved as .bak.
    assert!(dir.path().join("review-engine.bak").exists(), "backup missing");
    let new_out = Command::new(&exe).arg("--version").output().unwrap();
    assert!(new_out.status.success(), "new binary failed: {:?}", new_out);
    assert!(String::from_utf8_lossy(&new_out.stdout).contains("v9.9.9"));
    // The downloaded archive and extraction temp dir are cleaned up.
    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "review-engine" && n != "review-engine.bak")
        .collect();
    assert!(leftovers.is_empty(), "unexpected leftover files: {leftovers:?}");
}

#[tokio::test]
async fn plain_upgrade_rolls_back_on_sha_mismatch_and_keeps_bak() {
    let server = wiremock::MockServer::start().await;
    let script = fake_binary("9.9.9");
    let archive = single_file_tar_gz("review-engine", &script, 0o755);
    // Sidecar line 2 (binary hash) is deliberately wrong.
    let good = shasum(&script);
    let wrong = format!("0{}", &good[1..]);
    let (asset_url, checksum_url, asset_size, checksum_size) = mount_release(&server, &archive, &wrong).await;

    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("review-engine");
    std::fs::write(&exe, fake_binary("0.8.2")).unwrap();

    let release = test_release_json("v9.9.9", &asset_url, asset_size, &checksum_url, checksum_size);
    let output = run_with_env(
        &["upgrade", "--yes"],
        &[
            ("REVIEW_UPGRADE_INSTALL_METHOD", "plain"),
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_EXE", exe.to_str().unwrap()),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(!output.status.success(), "sha mismatch must fail: {:?}", output);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("sha256 verification"), "{combined}");

    // Rolled back: the exe is the previous binary again, and the .bak survives.
    let old_out = Command::new(&exe).arg("--version").output().unwrap();
    assert!(old_out.status.success(), "restored binary failed: {:?}", old_out);
    assert!(String::from_utf8_lossy(&old_out.stdout).contains("v0.8.2"));
    assert!(
        dir.path().join("review-engine.bak").exists(),
        "backup must be kept after rollback"
    );
}

#[tokio::test]
async fn plain_upgrade_fails_when_lock_held() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let server = wiremock::MockServer::start().await;
    let script = fake_binary("9.9.9");
    let archive = single_file_tar_gz("review-engine", &script, 0o755);
    let (asset_url, checksum_url, asset_size, checksum_size) = mount_release(&server, &archive, &shasum(&script)).await;

    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("review-engine");
    std::fs::write(&exe, fake_binary("0.8.2")).unwrap();
    // A concurrent upgrade holds a fresh lock.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    std::fs::write(
        dir.path().join(".review-engine.upgrade.lock"),
        format!("pid=424242 ts={now}\n"),
    )
    .unwrap();

    let release = test_release_json("v9.9.9", &asset_url, asset_size, &checksum_url, checksum_size);
    let output = run_with_env(
        &["upgrade", "--yes"],
        &[
            ("REVIEW_UPGRADE_INSTALL_METHOD", "plain"),
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_EXE", exe.to_str().unwrap()),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(!output.status.success(), "lock conflict must fail: {:?}", output);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("another upgrade appears to be in progress"),
        "{combined}"
    );
    // Nothing was touched: exe unchanged, no backup created.
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        fake_binary("0.8.2"),
        "exe must be untouched on lock conflict"
    );
    assert!(
        !dir.path().join("review-engine.bak").exists(),
        "no backup should be created on lock conflict"
    );
}

#[tokio::test]
async fn stale_lock_is_reclaimed_and_upgrade_succeeds() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let server = wiremock::MockServer::start().await;
    let script = fake_binary("9.9.9");
    let archive = single_file_tar_gz("review-engine", &script, 0o755);
    let (asset_url, checksum_url, asset_size, checksum_size) = mount_release(&server, &archive, &shasum(&script)).await;

    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("review-engine");
    std::fs::write(&exe, fake_binary("0.8.2")).unwrap();
    // Lock older than the 10-minute staleness window.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    std::fs::write(
        dir.path().join(".review-engine.upgrade.lock"),
        format!("pid=1 ts={}\n", now - 700),
    )
    .unwrap();

    let release = test_release_json("v9.9.9", &asset_url, asset_size, &checksum_url, checksum_size);
    let output = run_with_env(
        &["upgrade", "--yes"],
        &[
            ("REVIEW_UPGRADE_INSTALL_METHOD", "plain"),
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            ("REVIEW_UPGRADE_EXE", exe.to_str().unwrap()),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(output.status.success(), "stale lock should be reclaimed: {:?}", output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("done. Upgraded review-engine to v9.9.9."));
    assert!(
        !dir.path().join(".review-engine.upgrade.lock").exists(),
        "lock must be removed on completion"
    );
}

#[test]
fn rollback_restores_backup() {
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("review-engine");
    std::fs::write(&exe, fake_binary("9.9.9")).unwrap();
    let bak = dir.path().join("review-engine.bak");
    std::fs::write(&bak, fake_binary("0.8.2")).unwrap();

    let output = run_with_env(
        &["upgrade", "--rollback"],
        &[("REVIEW_UPGRADE_EXE", exe.to_str().unwrap())],
    );
    assert!(output.status.success(), "rollback failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("rolled back"), "{stdout}");

    let old_out = Command::new(&exe).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&old_out.stdout).contains("v0.8.2"));
    assert!(!bak.exists(), "backup consumed by rollback");
}

#[test]
fn rollback_without_backup_errors() {
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("review-engine");
    std::fs::write(&exe, fake_binary("9.9.9")).unwrap();

    let output = run_with_env(
        &["upgrade", "--rollback"],
        &[("REVIEW_UPGRADE_EXE", exe.to_str().unwrap())],
    );
    assert!(!output.status.success(), "rollback without backup must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no backup found"), "{stderr}");
}

/// Regression (HIGH): on macOS `std::env::current_exe()` returns the *symlink
/// invocation path* (e.g. `.../bin/reng`) rather than the resolved binary, so
/// an upgrade must canonicalize the exe path before replacing it. This test
/// drives the plain self-replace through a `reng -> review-engine` symlink
/// (the REVIEW_UPGRADE_EXE seam simulates what macOS current_exe() returns)
/// and asserts the REAL binary is upgraded, the symlink survives, and the
/// backup is the real old binary — not the link.
#[cfg(unix)]
#[tokio::test]
async fn plain_upgrade_through_symlink_upgrades_real_binary() {
    use std::os::unix::fs::symlink;

    let server = wiremock::MockServer::start().await;
    let script = fake_binary("9.9.9");
    let archive = single_file_tar_gz("review-engine", &script, 0o755);
    let (asset_url, checksum_url, asset_size, checksum_size) = mount_release(&server, &archive, &shasum(&script)).await;

    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let real = dir.path().join("review-engine");
    std::fs::write(&real, fake_binary("0.8.2")).unwrap();
    let link = bin_dir.join("reng");
    symlink(&real, &link).unwrap();

    let release = test_release_json("v9.9.9", &asset_url, asset_size, &checksum_url, checksum_size);
    let output = run_with_env(
        &["upgrade", "--yes"],
        &[
            ("REVIEW_UPGRADE_INSTALL_METHOD", "plain"),
            ("REVIEW_UPGRADE_CURRENT_VERSION", "0.8.2"),
            // Simulate macOS current_exe() returning the symlink invocation path.
            ("REVIEW_UPGRADE_EXE", link.to_str().unwrap()),
            ("REVIEW_UPGRADE_TEST_RELEASE", &release),
        ],
    );
    assert!(output.status.success(), "upgrade through symlink failed: {:?}", output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("done. Upgraded review-engine to v9.9.9."), "{stdout}");

    // The REAL binary was upgraded...
    let real_out = Command::new(&real).arg("--version").output().unwrap();
    assert!(
        real_out.status.success(),
        "real binary failed after upgrade: {:?}",
        real_out
    );
    assert!(
        String::from_utf8_lossy(&real_out.stdout).contains("v9.9.9"),
        "real binary must be upgraded, got: {}",
        String::from_utf8_lossy(&real_out.stdout)
    );

    // ...the symlink is still a symlink pointing at it...
    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink(), "reng must remain a symlink");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        real,
        "reng must still point at the real binary"
    );

    // ...and running through the symlink reports the new version.
    let via_link = Command::new(&link).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&via_link.stdout).contains("v9.9.9"));

    // The backup is the real old binary, next to the real binary (not the
    // symlink's directory), and is a real file rather than the link.
    let bak = dir.path().join("review-engine.bak");
    assert!(bak.exists(), "backup must sit next to the real binary");
    assert!(
        !std::fs::symlink_metadata(&bak).unwrap().file_type().is_symlink(),
        "backup must be a real file, not a symlink"
    );
    assert_eq!(std::fs::read(&bak).unwrap(), fake_binary("0.8.2"));
    assert!(
        !bin_dir.join("reng.bak").exists(),
        "no backup may appear in the symlink dir"
    );
}

// ─────────────────────────────────────────────────────────────────────
// `serve` subcommand — startup contract (v0.9.2 silent-hang defect fix):
//   * occupied port → fail FAST: non-zero exit + stderr naming the
//     conflict ("Address already in use (port N)") — never a silent hang.
//     Regression root cause: the config watcher's `spawn_blocking` task
//     parks on a never-ready `mpsc::recv`, which blocks tokio runtime
//     teardown forever, so a bind error never reached the terminal.
//   * free port → one-line stdout startup banner (listen address + health
//     URL + log file path) and `/health` answers 200.
// Both tests run with HOME pointed at a TempDir so user-level config and
// logs.ndjson never leak into the assertion surface.
// ─────────────────────────────────────────────────────────────────────

/// Spawn the binary and wait for it to exit, killing it after `timeout`.
/// Panics (after killing the child) when the process hangs — a regression
/// must fail the test fast instead of blocking the whole suite forever.
fn run_with_timeout(args: &[&str], home: &Path, timeout: std::time::Duration) -> std::process::Output {
    let mut child = Command::new(bin_path())
        .args(args)
        .env("HOME", home)
        .current_dir(home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn review-engine");
    let start = std::time::Instant::now();
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return child.wait_with_output().expect("failed to collect output");
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait_with_output();
            panic!("review-engine {args:?} did not exit within {timeout:?} — silent-hang regression");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Find an ephemeral free port by binding and immediately releasing :0.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    listener.local_addr().expect("no local addr").port()
}

#[test]
fn serve_fails_fast_when_port_in_use() {
    let home = TempDir::new().unwrap();
    // Occupy the target port for the whole lifetime of the child run.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind blocker");
    let port = blocker.local_addr().unwrap().port();

    let output = run_with_timeout(
        &["serve", "--port", &port.to_string()],
        home.path(),
        std::time::Duration::from_secs(30),
    );

    assert!(
        !output.status.success(),
        "serve on an occupied port must exit non-zero: {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Address already in use"),
        "stderr must name the conflict, got: {stderr}"
    );
    assert!(
        stderr.contains(&port.to_string()),
        "stderr must name the occupied port {port}, got: {stderr}"
    );
    drop(blocker);
}

#[test]
fn serve_prints_startup_banner_and_serves_health() {
    let home = TempDir::new().unwrap();
    let port = free_port();

    let mut child = Command::new(bin_path())
        .args(["serve", "--port", &port.to_string()])
        .env("HOME", home.path())
        .current_dir(home.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn review-engine serve");

    // Poll /health until the server answers (or fail within 30s).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let health_response = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                use std::io::{Read, Write};
                stream
                    .write_all(b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                    .expect("failed to write health request");
                let mut buf = String::new();
                stream.read_to_string(&mut buf).expect("failed to read health response");
                break buf;
            }
            Err(e) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = e;
            }
            Err(e) => {
                let _ = child.kill();
                panic!("serve did not open port {port} within 30s: {e}");
            }
        }
    };
    assert!(
        health_response.contains("200") && health_response.contains("\"status\":\"ok\""),
        "unexpected /health response: {health_response}"
    );

    // Give the line-buffered banner a moment to flush, then stop the server.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = child.kill();
    let output = child.wait_with_output().expect("failed to collect output");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("listening on"),
        "stdout must carry the startup banner, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("127.0.0.1:{port}")),
        "banner must name the listen address, got: {stdout}"
    );
    assert!(
        stdout.contains("/health"),
        "banner must name the health-check URL, got: {stdout}"
    );
    assert!(
        stdout.contains("logs.ndjson"),
        "banner must name the log file path, got: {stdout}"
    );
}
