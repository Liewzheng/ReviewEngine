use anyhow::Result;
use review_engine::upgrade::check_for_updates_with_version;
use review_engine::upgrade::{current_asset_spec, InstallMethod, Release, ReleaseAsset, UpdateCheck, Version};
use std::path::{Path, PathBuf};

use super::upgrade_install::{run_brew_upgrade, run_plain_upgrade, run_rollback};

// ───────────────────────────────────────────────────────────────────────
// `reng upgrade` — self-update (check / install-method hint / self-replace).
//
// The upgrade library (`review_engine::upgrade`) owns release lookup,
// download verification and extraction. This layer owns the CLI UX, the
// install-method dispatch, the concurrent-upgrade lock, and the atomic
// self-replace + rollback of the running binary.
//
// Test seams (documented env overrides, inert in normal use):
//   REVIEW_UPGRADE_TEST_RELEASE     inject release metadata as JSON instead
//                                   of querying the GitHub API
//   REVIEW_UPGRADE_CURRENT_VERSION  fake the "current" version (default: pkg)
//   REVIEW_UPGRADE_INSTALL_METHOD   force brew/cargo/docker/plain/unknown
//   REVIEW_UPGRADE_EXE              override the target exe path (self-replace
//                                   against a temp fixture instead of $0)
// ───────────────────────────────────────────────────────────────────────

const ENV_TEST_RELEASE: &str = "REVIEW_UPGRADE_TEST_RELEASE";
const ENV_CURRENT_VERSION: &str = "REVIEW_UPGRADE_CURRENT_VERSION";
const ENV_INSTALL_METHOD: &str = "REVIEW_UPGRADE_INSTALL_METHOD";
const ENV_EXE_OVERRIDE: &str = "REVIEW_UPGRADE_EXE";

pub(super) const LOCK_FILE_NAME: &str = ".review-engine.upgrade.lock";
/// A lock older than this (seconds) is considered stale and can be reclaimed.
pub(super) const LOCK_STALE_SECS: u64 = 600;

#[derive(serde::Deserialize)]
struct TestReleaseOverride {
    tag: String,
    asset_name: String,
    asset_url: String,
    asset_size: u64,
    checksum_url: String,
    checksum_size: u64,
}

/// `reng upgrade` entry point.
///
/// * `--check` / default first screen: report what's available.
/// * Plain installs perform an in-place self-replace (confirmed unless `--yes`).
/// * Brew: hint only, or execute `brew upgrade` when `--yes`.
/// * Cargo / Docker / Unknown: hint only, never auto-execute.
/// * `--version <tag>`: target a specific release; only the latest release is
///   auto-installable by the built-in updater.
/// * `--rollback`: restore `review-engine.bak` over the current binary.
pub async fn run_upgrade(check_only: bool, yes: bool, target_version: Option<&str>, rollback: bool) -> Result<()> {
    if rollback {
        return run_rollback();
    }

    let check = resolve_update_check().await?;

    // Explicit target version (--version <tag>).
    if let Some(tag) = target_version {
        let target = Version::parse_release_tag(tag).ok_or_else(|| {
            anyhow::anyhow!("invalid target version {tag:?}: expected a stable vMAJOR.MINOR.PATCH tag")
        })?;
        if target <= check.current_version {
            println!("review-engine is up to date (v{target})");
            return Ok(());
        }
        if target != check.latest_version {
            anyhow::bail!(
                "cannot auto-upgrade to v{target}: only the latest release v{} is supported by the built-in updater; run without --version",
                check.latest_version
            );
        }
    }

    // First screen (check mode or default): always report what's available.
    if check.has_update {
        println!(
            "A newer version of review-engine is available ({} -> {}).",
            check.current_version, check.latest_version
        );
        println!(
            "Detected install source: {}.",
            install_source_label(check.install_method)
        );
        println!("To update, run: {}", check.upgrade_command());
    } else {
        println!("review-engine is up to date (v{})", check.current_version);
        return Ok(());
    }

    if check_only {
        return Ok(());
    }

    // Dispatch by install method.
    match check.install_method {
        InstallMethod::Plain => {
            if !yes && !confirm_upgrade(&check.latest_version.to_string())? {
                println!("upgrade aborted.");
                return Ok(());
            }
            run_plain_upgrade(&check).await?;
        }
        InstallMethod::Brew if yes => run_brew_upgrade()?,
        InstallMethod::Brew => {
            println!("Run again with --yes to execute `brew upgrade review-engine`.");
        }
        InstallMethod::Cargo | InstallMethod::Docker | InstallMethod::Unknown => {}
    }
    Ok(())
}

fn install_source_label(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::Brew => "Homebrew",
        InstallMethod::Cargo => "Cargo (~/.cargo/bin)",
        InstallMethod::Docker => "Docker 容器",
        InstallMethod::Plain => "直接部署的二进制",
        InstallMethod::Unknown => "未知（手动安装）",
    }
}

fn current_version() -> String {
    std::env::var(ENV_CURRENT_VERSION).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn resolve_install_method() -> InstallMethod {
    let Ok(v) = std::env::var(ENV_INSTALL_METHOD) else {
        return InstallMethod::detect();
    };
    match v.to_ascii_lowercase().as_str() {
        "brew" => InstallMethod::Brew,
        "cargo" => InstallMethod::Cargo,
        "docker" => InstallMethod::Docker,
        "plain" => InstallMethod::Plain,
        _ => InstallMethod::Unknown,
    }
}

/// Resolve the executable to replace. Always canonicalized first: on macOS
/// `std::env::current_exe()` returns the *symlink invocation path* (e.g.
/// `.../bin/reng`), not the real binary — upgrading the link would replace the
/// symlink with a real file and leave the actual `review-engine` untouched.
/// `REVIEW_UPGRADE_EXE` is the test seam that also feeds this path, so it is
/// canonicalized the same way. Falls back to the raw path if it cannot be
/// resolved.
pub(super) fn current_exe_path() -> PathBuf {
    let raw = match std::env::var_os(ENV_EXE_OVERRIDE) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().unwrap_or_else(|_| PathBuf::from("review-engine")),
    };
    canonical_exe_path(&raw)
}

/// Canonicalize `raw` so a symlink invocation path resolves to the real
/// binary; falls back to the raw path when it cannot be resolved.
fn canonical_exe_path(raw: &Path) -> PathBuf {
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}

fn test_release_override() -> Option<TestReleaseOverride> {
    let raw = std::env::var(ENV_TEST_RELEASE).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Resolve the update check: from the test override when set, otherwise from
/// the real GitHub API. The detected install method is (re)applied so the
/// `REVIEW_UPGRADE_INSTALL_METHOD` override wins over `InstallMethod::detect()`.
async fn resolve_update_check() -> Result<UpdateCheck> {
    let current = current_version();
    if let Some(t) = test_release_override() {
        let current_version = Version::parse(&current)?;
        let latest_version = Version::parse_release_tag(&t.tag)
            .ok_or_else(|| anyhow::anyhow!("invalid test release tag {:?}", t.tag))?;
        let asset = ReleaseAsset {
            name: t.asset_name.clone(),
            download_url: t.asset_url,
            size: t.asset_size,
        };
        // Mirrors `find_checksum_asset`: the published sidecar is
        // `<base>.sha256` with no archive extension.
        let checksum_base = t
            .asset_name
            .strip_suffix(".tar.gz")
            .or_else(|| t.asset_name.strip_suffix(".zip"))
            .unwrap_or(&t.asset_name);
        let checksum = ReleaseAsset {
            name: format!("{checksum_base}.sha256"),
            download_url: t.checksum_url,
            size: t.checksum_size,
        };
        let release = Release {
            tag_name: t.tag.clone(),
            html_url: format!("https://github.com/Liewzheng/ReviewEngine/releases/tag/{}", t.tag),
            published_at: String::new(),
            assets: vec![asset.clone(), checksum.clone()],
        };
        return Ok(UpdateCheck {
            current_version,
            latest_version,
            has_update: latest_version > current_version,
            platform: current_asset_spec().ok(),
            asset: Some(asset),
            checksum_asset: Some(checksum),
            install_method: resolve_install_method(),
            latest_release: release,
        });
    }
    let mut check = check_for_updates_with_version(&current).await?;
    check.install_method = resolve_install_method();
    Ok(check)
}

fn confirm_upgrade(target: &str) -> Result<bool> {
    use std::io::Write;
    print!("Proceed with the upgrade to v{target}? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let answer = line.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

#[cfg(test)]
mod upgrade_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::upgrade_install::{find_binary_in, parse_sidecar_binary_hex, unix_now_secs, UpgradeLock};
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn install_source_labels_are_stable() {
        assert_eq!(install_source_label(InstallMethod::Brew), "Homebrew");
        assert_eq!(install_source_label(InstallMethod::Cargo), "Cargo (~/.cargo/bin)");
        assert_eq!(install_source_label(InstallMethod::Docker), "Docker 容器");
        assert_eq!(install_source_label(InstallMethod::Plain), "直接部署的二进制");
        assert_eq!(install_source_label(InstallMethod::Unknown), "未知（手动安装）");
    }

    #[test]
    fn parses_binary_hex_from_sidecar() {
        let text = format!(
            "{}  review-engine-aarch64-apple-darwin.tar.gz\n{}  review-engine\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(parse_sidecar_binary_hex(&text, "review-engine"), Some("b".repeat(64)));
        assert_eq!(parse_sidecar_binary_hex(&text, "review-engine.exe"), None);
        assert_eq!(parse_sidecar_binary_hex("# only comments\n", "review-engine"), None);
    }

    #[test]
    fn lock_conflicts_and_releases() {
        let dir = tempdir().unwrap();
        let lock = UpgradeLock::acquire(dir.path()).unwrap();
        let path = dir.path().join(LOCK_FILE_NAME);
        assert!(path.exists(), "lock file must exist while held");

        let err = UpgradeLock::acquire(dir.path()).unwrap_err();
        assert!(err.to_string().contains("in progress"), "got: {err}");

        drop(lock);
        assert!(!path.exists(), "lock file must be removed on drop");

        let lock2 = UpgradeLock::acquire(dir.path()).unwrap();
        drop(lock2);
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE_NAME);
        let old_ts = unix_now_secs().saturating_sub(LOCK_STALE_SECS + 60);
        std::fs::write(&path, format!("pid=1 ts={old_ts}\n")).unwrap();
        let lock = UpgradeLock::acquire(dir.path()).unwrap();
        drop(lock);
        assert!(!path.exists(), "stale lock must be reclaimed and removed");
    }

    #[test]
    fn finds_binary_in_extracted_tree() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("out");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("LICENSE"), "license").unwrap();
        let target = root.join("bin").join("review-engine");
        std::fs::write(&target, "#!/bin/sh").unwrap();
        std::fs::write(root.join("bin").join("other"), "x").unwrap();
        assert_eq!(find_binary_in(&root, "review-engine"), Some(target));
        assert_eq!(find_binary_in(&root, "review-engine.exe"), None);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_exe_path_resolves_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let real = dir.path().join("review-engine");
        std::fs::write(&real, "#!/bin/sh").unwrap();
        let link = dir.path().join("reng");
        symlink(&real, &link).unwrap();
        // macOS tempdirs live under /var/folders which is a symlink to
        // /private/var/folders, so compare against the canonicalized real path.
        let real_canonical = std::fs::canonicalize(&real).unwrap();

        // A symlink invocation path (what macOS current_exe() returns) must
        // resolve to the real binary, not stay as the link.
        assert_eq!(canonical_exe_path(&link), real_canonical);
        // A real path is returned as its canonical form.
        assert_eq!(canonical_exe_path(&real), real_canonical);
        // A missing path falls back to the raw value.
        let missing = dir.path().join("nope");
        assert_eq!(canonical_exe_path(&missing), missing);
    }
}
