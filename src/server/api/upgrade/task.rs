use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::server::state::UpgradeJobState;
use crate::server::AppState;
use crate::upgrade::{download, find_checksum_asset, platform, verify, UpdateCheck, UpgradeError};

use super::start::set_job;

const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);
const FRONTEND_DIST_ASSET: &str = "frontend-dist.tar.gz";
const DONE_DWELL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpgradeMode {
    BinaryOnly,
    ContainerWithFrontend,
}

impl UpgradeMode {
    fn done_message(self) -> &'static str {
        match self {
            Self::BinaryOnly => "升级完成，服务需重启后生效",
            Self::ContainerWithFrontend => "升级完成，容器即将自动重启",
        }
    }
}

pub(crate) fn check_response(state: &AppState, check: &UpdateCheck) -> serde_json::Value {
    serde_json::json!({
        "currentVersion": check.current_version.to_string(),
        "latestVersion": check.latest_version.to_string(),
        "updateAvailable": check.has_update,
        "installMethod": install_method_str(state.upgrade.install_method),
        "platformAssetAvailable": check.platform.is_some() && check.asset.is_some(),
        "releaseUrl": check.latest_release.html_url,
        "upgradeHint": state.upgrade.install_method.upgrade_command(),
        "cachedAt": super::check::cached_at_str(state),
    })
}

pub(crate) fn install_method_str(method: crate::upgrade::InstallMethod) -> &'static str {
    match method {
        crate::upgrade::InstallMethod::Plain => "binary",
        crate::upgrade::InstallMethod::Brew => "brew",
        crate::upgrade::InstallMethod::Docker => "docker",
        crate::upgrade::InstallMethod::Cargo => "cargo",
        crate::upgrade::InstallMethod::Unknown => "unknown",
    }
}

pub(crate) async fn run_upgrade_task(
    state: Arc<AppState>,
    check: UpdateCheck,
    install_dir: PathBuf,
    mode: UpgradeMode,
) {
    let format = check.platform.map(|p| p.format).unwrap_or(platform::AssetFormat::TarGz);
    let staging = std::env::temp_dir().join(format!(
        "reng-upgrade-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));

    let result = async {
        let asset = check
            .asset
            .as_ref()
            .ok_or_else(|| UpgradeError::not_found("release asset missing"))?;
        let checksum = check
            .checksum_asset
            .as_ref()
            .ok_or_else(|| UpgradeError::not_found("checksum asset missing"))?;

        tokio::fs::create_dir_all(&staging).await?;

        set_job(&state, UpgradeJobState::Downloading, "正在下载 release 资产");
        let (asset_temp, _) =
            download::download_asset(&asset.download_url, &staging, &asset.name, Some(asset.size)).await?;
        let (checksum_temp, _) =
            download::download_asset(&checksum.download_url, &staging, &checksum.name, Some(checksum.size)).await?;

        set_job(&state, UpgradeJobState::Verifying, "正在校验 sha256");
        let checksum_text = tokio::fs::read_to_string(&checksum_temp).await?;
        verify::verify_file_with_checksum_text(&asset_temp, &checksum_text)?;

        set_job(&state, UpgradeJobState::Installing, "正在解压并替换二进制");
        let extracted = staging.join("extracted");
        verify::extract_asset(&asset_temp, format, &extracted)?;
        let new_binary = find_extracted_binary(&extracted)
            .ok_or_else(|| UpgradeError::invalid_data("archive contains no review-engine binary"))?;
        make_executable(&new_binary)?;

        if !smoke_test(&new_binary).await? {
            return Err(UpgradeError::invalid_data("新二进制冒烟测试失败，未替换"));
        }

        let staged_dist = if mode == UpgradeMode::ContainerWithFrontend {
            stage_frontend_dist(&state, &check.latest_release, &staging).await?
        } else {
            None
        };

        let exe_name = current_exe_name();
        replace_binary(&new_binary, &install_dir, &exe_name).await?;

        if let Some(dist_root) = staged_dist {
            replace_frontend_dist(&dist_root, &resolve_frontend_dir())?;
        }

        Ok::<(), UpgradeError>(())
    }
    .await;

    let succeeded = result.is_ok();
    match result {
        Ok(()) => {
            set_job(&state, UpgradeJobState::Done, mode.done_message());
        }
        Err(e) => {
            tracing::warn!(error = %e, "upgrade failed");
            set_job(&state, UpgradeJobState::Failed, format!("升级失败：{e}"));
        }
    }

    let _ = std::fs::remove_dir_all(&staging);

    if mode == UpgradeMode::ContainerWithFrontend && succeeded && exit_after_upgrade_enabled() {
        tokio::time::sleep(DONE_DWELL).await;
        std::process::exit(0);
    }
}

pub(crate) async fn stage_frontend_dist(
    state: &AppState,
    release: &crate::upgrade::Release,
    staging: &Path,
) -> crate::upgrade::Result<Option<PathBuf>> {
    let Some(asset) = release.assets.iter().find(|a| a.name == FRONTEND_DIST_ASSET) else {
        tracing::warn!("release has no {FRONTEND_DIST_ASSET}; skipping frontend dist upgrade (binary-only)");
        return Ok(None);
    };

    set_job(state, UpgradeJobState::Downloading, "正在下载 frontend dist");
    let (dist_temp, _) = download::download_asset(&asset.download_url, staging, &asset.name, Some(asset.size)).await?;

    if let Some(checksum) = find_checksum_asset(release, &asset.name) {
        set_job(state, UpgradeJobState::Verifying, "正在校验 frontend dist sha256");
        let (checksum_temp, _) =
            download::download_asset(&checksum.download_url, staging, &checksum.name, Some(checksum.size)).await?;
        let checksum_text = tokio::fs::read_to_string(&checksum_temp).await?;
        verify::verify_file_with_checksum_text(&dist_temp, &checksum_text)?;
    } else {
        tracing::warn!("{FRONTEND_DIST_ASSET} has no .sha256 sidecar; skipping checksum verification");
    }

    set_job(state, UpgradeJobState::Installing, "正在解压 frontend dist");
    let extracted = staging.join("frontend-dist-extracted");
    verify::extract_asset(&dist_temp, platform::AssetFormat::TarGz, &extracted)?;
    let dist_root = find_dist_root(&extracted)
        .ok_or_else(|| UpgradeError::invalid_data("frontend dist archive contains no index.html"))?;
    Ok(Some(dist_root))
}

pub(crate) fn find_dist_root(root: &Path) -> Option<PathBuf> {
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        if dir.join("index.html").is_file() {
            return Some(dir);
        }
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                queue.push(path);
            }
        }
    }
    None
}

pub(crate) fn replace_frontend_dist(staged: &Path, frontend_dir: &Path) -> crate::upgrade::Result<()> {
    std::fs::create_dir_all(frontend_dir)?;

    let nonce: u64 = rand::random();
    let new_dir = frontend_dir.join(format!(".new-{nonce:x}"));
    let backup_dir = frontend_dir.join(format!(".old-{nonce:x}"));

    if let Err(e) = copy_dir_contents(staged, &new_dir) {
        let _ = std::fs::remove_dir_all(&new_dir);
        return Err(e);
    }

    if let Err(e) = park_old_contents(frontend_dir, &backup_dir, &new_dir) {
        let _ = std::fs::remove_dir_all(&new_dir);
        restore_old_contents(frontend_dir, &backup_dir);
        return Err(e);
    }

    if let Err(e) = bring_new_contents_in(frontend_dir, &new_dir) {
        clear_live_contents(frontend_dir, &new_dir, &backup_dir);
        let _ = std::fs::remove_dir_all(&new_dir);
        restore_old_contents(frontend_dir, &backup_dir);
        return Err(e);
    }

    let _ = std::fs::remove_dir_all(&backup_dir);
    let _ = std::fs::remove_dir_all(&new_dir);
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path) -> crate::upgrade::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_contents(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn park_old_contents(live: &Path, backup_dir: &Path, new_dir: &Path) -> crate::upgrade::Result<()> {
    let new_name = new_dir.file_name().unwrap_or_default();
    let backup_name = backup_dir.file_name().unwrap_or_default();
    std::fs::create_dir_all(backup_dir)?;
    for entry in std::fs::read_dir(live)? {
        let entry = entry?;
        if entry.file_name() == new_name || entry.file_name() == backup_name {
            continue;
        }
        std::fs::rename(entry.path(), backup_dir.join(entry.file_name()))?;
    }
    Ok(())
}

fn bring_new_contents_in(live: &Path, new_dir: &Path) -> crate::upgrade::Result<()> {
    for entry in std::fs::read_dir(new_dir)? {
        let entry = entry?;
        std::fs::rename(entry.path(), live.join(entry.file_name()))?;
    }
    Ok(())
}

fn restore_old_contents(live: &Path, backup_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(backup_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::rename(entry.path(), live.join(entry.file_name()));
        }
    }
    let _ = std::fs::remove_dir_all(backup_dir);
}

fn clear_live_contents(live: &Path, new_dir: &Path, backup_dir: &Path) {
    let new_name = new_dir.file_name().unwrap_or_default();
    let backup_name = backup_dir.file_name().unwrap_or_default();
    if let Ok(entries) = std::fs::read_dir(live) {
        for entry in entries.flatten() {
            if entry.file_name() == new_name || entry.file_name() == backup_name {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

async fn replace_binary(new_binary: &Path, install_dir: &Path, exe_name: &str) -> crate::upgrade::Result<()> {
    let exe_path = install_dir.join(exe_name);
    let nonce: u64 = rand::random();
    let backup = install_dir.join(format!(".{exe_name}.bak-{nonce:x}"));
    let staged = install_dir.join(format!(".{exe_name}.new-{nonce:x}"));

    let had_original = exe_path.exists();
    if had_original {
        std::fs::copy(&exe_path, &backup)?;
    }

    std::fs::copy(new_binary, &staged)?;
    make_executable(&staged)?;

    if let Err(e) = std::fs::rename(&staged, &exe_path) {
        let _ = std::fs::remove_file(&staged);
        if had_original {
            let _ = std::fs::rename(&backup, &exe_path);
        }
        return Err(UpgradeError::Io(e));
    }

    if !smoke_test(&exe_path).await? {
        if had_original {
            let _ = std::fs::rename(&backup, &exe_path);
        }
        return Err(UpgradeError::invalid_data("替换后冒烟测试失败，已回滚"));
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

async fn smoke_test(binary: &Path) -> crate::upgrade::Result<bool> {
    let mut child = tokio::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(UpgradeError::from)?;
    match tokio::time::timeout(SMOKE_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Ok(status.success()),
        Ok(Err(e)) => Err(UpgradeError::from(e)),
        Err(_) => {
            let _ = child.kill().await;
            Ok(false)
        }
    }
}

fn find_extracted_binary(root: &Path) -> Option<PathBuf> {
    let mut queue = vec![root.to_path_buf()];
    while let Some(dir) = queue.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                queue.push(path);
            } else {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name == "review-engine" || name == "review-engine.exe" {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn make_executable(path: &Path) -> crate::upgrade::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn current_exe_canonical() -> Option<PathBuf> {
    let raw = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&raw).unwrap_or(raw))
}

pub(crate) fn resolve_install_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("REVIEW_UPGRADE_INSTALL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    current_exe_canonical()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn resolve_frontend_dir() -> PathBuf {
    std::env::var("REVIEW_UPGRADE_FRONTEND_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/app/frontend/dist"))
}

pub(crate) fn exit_after_upgrade_enabled() -> bool {
    std::env::var("REVIEW_UPGRADE_EXIT_AFTER")
        .map(|v| v != "0")
        .unwrap_or(true)
}

pub(crate) fn current_exe_name() -> String {
    current_exe_canonical()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "review-engine".to_string())
}
