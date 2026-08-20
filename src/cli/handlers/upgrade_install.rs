use anyhow::Result;
use review_engine::upgrade::download::{download_asset, download_verified_asset};
use review_engine::upgrade::verify::{extract_asset, parse_sha256_line, verify_file_sha256};
use review_engine::upgrade::{ReleaseAsset, UpdateCheck};
use std::path::{Path, PathBuf};

use super::upgrade::{current_exe_path, LOCK_FILE_NAME, LOCK_STALE_SECS};

/// Execute `brew upgrade review-engine`, passing brew's output straight
/// through to the terminal. Fails with brew's exit status on error.
pub(super) fn run_brew_upgrade() -> Result<()> {
    println!("Running: brew upgrade review-engine");
    let status = std::process::Command::new("brew")
        .args(["upgrade", "review-engine"])
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "`brew upgrade review-engine` failed (exit status {:?}); the output above is from brew",
            status.code()
        );
    }
    Ok(())
}

/// Restore the previous binary from `review-engine.bak`.
pub(super) fn run_rollback() -> Result<()> {
    let exe = current_exe_path();
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the directory of {}", exe.display()))?;
    let bak = dir.join("review-engine.bak");
    if !bak.exists() {
        anyhow::bail!("no backup found at {}; nothing to roll back", bak.display());
    }
    if exe.exists() {
        std::fs::remove_file(&exe)?;
    }
    std::fs::rename(&bak, &exe)?;
    set_executable(&exe)?;
    println!("rolled back to the previous binary at {}", exe.display());
    Ok(())
}

/// In-place self-replace: download → extract → backup → install → verify →
/// smoke → keep `.bak`. Every failure after the backup restores the previous
/// binary and preserves the `.bak` for a later `--rollback`.
pub(super) async fn run_plain_upgrade(check: &UpdateCheck) -> Result<()> {
    let exe = current_exe_path();
    let exe_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine the directory of {}", exe.display()))?
        .to_path_buf();
    let asset = check
        .asset
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no release asset for this platform; cannot auto-upgrade"))?;
    let checksum = check.checksum_asset.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "release has no checksum sidecar for {}; cannot auto-upgrade",
            asset.name
        )
    })?;
    let platform = check
        .platform
        .ok_or_else(|| anyhow::anyhow!("unsupported platform; cannot auto-upgrade"))?;
    if !exe.is_file() {
        anyhow::bail!("current executable not found at {}; cannot upgrade", exe.display());
    }

    // Serialize concurrent upgrades; the lock file is removed on drop.
    let _lock = UpgradeLock::acquire(&exe_dir)?;

    println!("downloading {}", asset.name);
    let archive = download_verified_asset(asset, checksum, &exe_dir, None).await?;
    println!("verifying checksum of {}", asset.name);

    // Extract into a temp dir next to the exe (same filesystem → atomic rename).
    let extract_dir = unique_temp_dir(&exe_dir, "extract")?;
    let _cleanup = CleanupPaths(vec![archive.clone(), extract_dir.clone()]);
    extract_asset(&archive, platform.format, &extract_dir)
        .map_err(|e| anyhow::anyhow!("failed to extract release archive: {e}"))?;

    let exe_name = if platform.is_windows() {
        "review-engine.exe"
    } else {
        "review-engine"
    };
    let extracted = find_binary_in(&extract_dir, exe_name)
        .ok_or_else(|| anyhow::anyhow!("no {exe_name} found inside the release archive"))?;

    // Back up the current binary before touching it.
    println!("installing");
    let bak = exe_dir.join("review-engine.bak");
    if bak.exists() {
        let _ = std::fs::remove_file(&bak);
    }
    std::fs::rename(&exe, &bak)?;

    if let Err(e) = install_binary(&extracted, &exe) {
        rollback_restore(&exe, &bak);
        return Err(anyhow::anyhow!(
            "failed to install the new binary: {e}; previous version restored (backup kept at {})",
            bak.display()
        ));
    }

    // 双保险: re-verify the installed binary against a downloaded checksum
    // (a `<hex>  <binary-name>` line inside the `.sha256` sidecar, when
    // published). Falls back to archive-checksum + smoke test otherwise.
    match expected_binary_sha(checksum, exe_name).await {
        Ok(Some(hex)) => {
            if let Err(e) = verify_file_sha256(&exe, &hex) {
                rollback_restore(&exe, &bak);
                return Err(anyhow::anyhow!(
                    "installed binary failed sha256 verification: {e}; previous version restored (backup kept at {})",
                    bak.display()
                ));
            }
        }
        Ok(None) => {
            eprintln!(
                "warning: release does not publish a binary-level sha256; relying on archive checksum + smoke test"
            );
        }
        Err(e) => {
            eprintln!(
                "warning: could not fetch the binary-level checksum ({e}); relying on archive checksum + smoke test"
            );
        }
    }

    // Smoke test: the new binary must report the target version.
    if !smoke_test_version(&exe, &check.latest_version.to_string()) {
        rollback_restore(&exe, &bak);
        return Err(anyhow::anyhow!(
            "new binary failed the smoke test (--version did not report v{}); previous version restored (backup kept at {})",
            check.latest_version,
            bak.display()
        ));
    }

    println!("done. Upgraded review-engine to v{}.", check.latest_version);
    println!(
        "Previous binary kept at {}; roll back with `reng upgrade --rollback`.",
        bak.display()
    );
    Ok(())
}

/// Expected sha256 of the extracted binary: re-fetch the release's `.sha256`
/// sidecar and look for a `<hex>  <binary-name>` line. `None` means the
/// release does not publish a binary-level checksum.
async fn expected_binary_sha(checksum: &ReleaseAsset, binary_name: &str) -> Result<Option<String>> {
    let tmp = unique_temp_dir(&std::env::temp_dir(), "checksum")?;
    let _cleanup = CleanupPaths(vec![tmp.clone()]);
    let (sidecar_path, _) =
        download_asset(&checksum.download_url, &tmp, &checksum.name, Some(checksum.size), None).await?;
    let text = std::fs::read_to_string(&sidecar_path)?;
    Ok(parse_sidecar_binary_hex(&text, binary_name))
}

pub(super) fn parse_sidecar_binary_hex(text: &str, binary_name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok((hex, name)) = parse_sha256_line(line) {
            if name == binary_name {
                return Some(hex);
            }
        }
    }
    None
}

/// Locate `binary_name` in the extracted tree, preferring the shallowest
/// match (e.g. `bin/review-engine` over a nested copy).
pub(super) fn find_binary_in(root: &Path, binary_name: &str) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(binary_name) {
                let depth = path.components().count();
                if best.as_ref().map(|(d, _)| depth < *d).unwrap_or(true) {
                    best = Some((depth, path));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Move the extracted binary into place, falling back to copy+remove for a
/// cross-device rename, then mark it executable.
fn install_binary(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Err(rename_err) = std::fs::rename(from, to) {
        std::fs::copy(from, to).map_err(|copy_err| {
            std::io::Error::new(
                rename_err.kind(),
                format!("rename failed ({rename_err}); copy fallback failed ({copy_err})"),
            )
        })?;
        let _ = std::fs::remove_file(from);
    }
    set_executable(to)
}

fn set_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(path, perms)
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Restore the previous binary after a failed install. Copies (rather than
/// renames) so the `.bak` survives for a later explicit `--rollback`.
fn rollback_restore(exe: &Path, bak: &Path) {
    let _ = std::fs::remove_file(exe);
    if let Err(e) = std::fs::copy(bak, exe) {
        eprintln!("warning: failed to restore the previous binary: {e}");
    }
    let _ = set_executable(exe);
}

/// Run `<exe> --version`; it must succeed and print the target version.
fn smoke_test_version(exe: &Path, target: &str) -> bool {
    match std::process::Command::new(exe).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            out.status.success() && (stdout.contains(target) || stderr.contains(target))
        }
        Err(_) => false,
    }
}

fn unique_temp_dir(base: &Path, tag: &str) -> Result<PathBuf> {
    let nonce: u64 = rand::random();
    let dir = base.join(format!(".review-engine-{tag}-{}-{:x}", std::process::id(), nonce));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Best-effort removal of temp files/dirs on drop (success or error).
struct CleanupPaths(Vec<PathBuf>);

impl Drop for CleanupPaths {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_dir_all(p);
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Exclusive lock preventing two upgrades of the same directory at once.
/// `create_new`, contains `pid=<pid> ts=<unix-seconds>`, removed on drop.
/// A lock older than `LOCK_STALE_SECS` is treated as stale and reclaimed.
#[derive(Debug)]
pub(super) struct UpgradeLock {
    path: PathBuf,
}

impl UpgradeLock {
    pub(super) fn acquire(dir: &Path) -> Result<UpgradeLock> {
        use std::io::Write;
        let path = dir.join(LOCK_FILE_NAME);
        let now = unix_now_secs();
        for attempt in 0..2 {
            let content = format!("pid={} ts={}\n", std::process::id(), now);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(content.as_bytes())?;
                    return Ok(UpgradeLock { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = read_lock_ts(&path)
                        .map(|ts| now.saturating_sub(ts) > LOCK_STALE_SECS)
                        .unwrap_or(false);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        if attempt == 0 {
                            continue;
                        }
                    }
                    let pid = read_lock_pid(&path)
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    anyhow::bail!(
                        "another upgrade appears to be in progress (lock: {}, pid={}); remove the lock file if it is stale",
                        path.display(),
                        pid
                    );
                }
                Err(e) => return Err(e.into()),
            }
        }
        anyhow::bail!("could not acquire the upgrade lock at {}", path.display())
    }
}

impl Drop for UpgradeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_lock_pid(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.split_whitespace()
        .find_map(|tok| tok.strip_prefix("pid="))
        .and_then(|v| v.parse().ok())
}

fn read_lock_ts(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.split_whitespace()
        .find_map(|tok| tok.strip_prefix("ts="))
        .and_then(|v| v.parse().ok())
}
