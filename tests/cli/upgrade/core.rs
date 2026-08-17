use super::*;

#[test]
fn upgrade_check_reports_update_by_install_method() {
    let cases: [(&str, &str, &str); 5] = [
        ("brew", "Homebrew", "brew upgrade review-engine"),
        (
            "cargo",
            "Cargo (~/.cargo/bin)",
            "cargo install review-engine --locked --features cli",
        ),
        (
            "docker",
            "Docker 容器",
            "Web UI 或 reng upgrade 自动升级（容器将自动重启）",
        ),
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
