//! Self-upgrade REST endpoints under `/api/v1/system/upgrade`.
//!
//! - `GET  /api/v1/system/upgrade/check`  — latest version + install hints (1h cache)
//! - `POST /api/v1/system/upgrade`        — start a binary upgrade (single-flight)
//! - `GET  /api/v1/system/upgrade/status` — job state machine
//!
//! Reuses the `crate::upgrade` core library (U2). For plain installs the
//! running process is **never restarted**: after a successful binary replace
//! the job reports `done` with the message "服务需重启后生效". For container
//! installs the same pipeline also swaps the frontend dist and then **exits
//! the process** so the compose `restart: unless-stopped` policy pulls the
//! container back up with the new files (the `done` state is kept readable for
//! a short dwell first so the frontend can poll it).
//!
//! All three endpoints sit behind the `/api/v1` auth middleware (mounted in
//! `api::routes`), so they inherit whatever auth policy the server was started
//! with.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::server::state::{UpgradeCache, UpgradeJobState};
use crate::server::AppState;
use crate::upgrade::{
    current_asset_spec, download, find_asset, find_checksum_asset, platform, verify, GitHubReleaseClient,
    InstallMethod, Release, UpdateCheck, UpgradeError, Version,
};

/// GitHub check results are cached server-side for 1h — the unauthenticated
/// GitHub API is rate limited to 60 requests/hour per IP.
const CHECK_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Timeout for running the new binary's `--version` smoke test.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Default GitHub API base; overridable via `REVIEW_UPGRADE_API_BASE`
/// (self-hosted mirror or wiremock in tests).
const GITHUB_API_BASE: &str = "https://api.github.com";
/// Frontend dist asset name in every release (produced by release.yml).
const FRONTEND_DIST_ASSET: &str = "frontend-dist.tar.gz";
/// How long the `done` state stays readable before the container-restart exit,
/// so the frontend's status poll can observe it.
const DONE_DWELL: Duration = Duration::from_millis(500);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/upgrade/check", get(check_upgrade))
        .route("/upgrade", post(start_upgrade))
        .route("/upgrade/status", get(upgrade_status))
}

// ─── GET /api/v1/system/upgrade/check ─────────────────────────────

async fn check_upgrade(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Serve a fresh cache hit without touching the network.
    {
        let cache = state.upgrade.cache.read().unwrap();
        if let Some(c) = cache.as_ref() {
            if c.cached_at + CHECK_CACHE_TTL > Utc::now() {
                return Json(check_response(&state, &c.check)).into_response();
            }
        }
    }

    // Stale/missing cache: run a fresh check. Surface "checking" to /status
    // only when no upgrade job is in flight (never stomp a running job).
    let was_idle = state.upgrade.job.read().unwrap().state == UpgradeJobState::Idle;
    if was_idle {
        set_job(&state, UpgradeJobState::Checking, "正在检查最新版本");
    }
    let result = refresh_check(&state).await;
    if was_idle {
        set_job(&state, UpgradeJobState::Idle, "idle");
    }

    match result {
        Ok(check) => Json(check_response(&state, &check)).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("check failed: {e}") })),
        )
            .into_response(),
    }
}

fn check_response(state: &AppState, check: &UpdateCheck) -> serde_json::Value {
    serde_json::json!({
        "currentVersion": check.current_version.to_string(),
        "latestVersion": check.latest_version.to_string(),
        "updateAvailable": check.has_update,
        "installMethod": install_method_str(state.upgrade.install_method),
        "platformAssetAvailable": check.platform.is_some() && check.asset.is_some(),
        "releaseUrl": check.latest_release.html_url,
        "upgradeHint": state.upgrade.install_method.upgrade_command(),
        "cachedAt": cached_at_str(state),
    })
}

/// Fetch the latest stable release from GitHub and store it in the cache.
async fn refresh_check(state: &AppState) -> crate::upgrade::Result<UpdateCheck> {
    let current = env!("CARGO_PKG_VERSION");
    let base = std::env::var("REVIEW_UPGRADE_API_BASE").unwrap_or_else(|_| GITHUB_API_BASE.to_string());
    let client = GitHubReleaseClient::with_base_url(current, &base)?;
    let latest = client
        .latest_stable_release()
        .await?
        .ok_or_else(|| UpgradeError::not_found("no stable vX.Y.Z release found on GitHub"))?;
    let latest_version = Version::parse_release_tag(&latest.tag_name)
        .ok_or_else(|| UpgradeError::invalid_data(format!("latest tag {:?} is not stable", latest.tag_name)))?;
    let current_version = Version::parse(current)?;

    let platform = current_asset_spec().ok();
    let asset = platform.as_ref().and_then(|spec| find_asset(&latest, spec));
    let checksum_asset = asset.and_then(|a| find_checksum_asset(&latest, &a.name));

    let check = UpdateCheck {
        current_version,
        latest_version,
        has_update: latest_version > current_version,
        platform,
        asset: asset.cloned(),
        checksum_asset: checksum_asset.cloned(),
        install_method: state.upgrade.install_method,
        latest_release: latest,
    };
    *state.upgrade.cache.write().unwrap() = Some(UpgradeCache {
        check: check.clone(),
        cached_at: Utc::now(),
    });
    Ok(check)
}

fn cached_at_str(state: &AppState) -> String {
    state
        .upgrade
        .cache
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.cached_at.to_rfc3339())
        .unwrap_or_default()
}

// ─── POST /api/v1/system/upgrade ─────────────────────────────────

async fn start_upgrade(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    // Cross-site defense (B2): a browser-triggered POST that is not same-site
    // (CSRF / DNS-rebinding) is rejected before any state is touched. Requests
    // without an `Origin` header (curl, scripts, in-process callers) already
    // hold loopback-equivalent authority and pass through.
    if let Err(resp) = validate_origin(&headers) {
        return *resp;
    }

    match state.upgrade.install_method {
        // Package-managed installs: we must not fight the package manager. Tell
        // the user the right command instead of mutating files behind its back.
        InstallMethod::Brew => reject_with_hint(
            "检测到 Homebrew 安装，请手动执行升级命令",
            InstallMethod::Brew.upgrade_command(),
        ),
        InstallMethod::Cargo => reject_with_hint(
            "检测到 cargo 安装，请手动执行升级命令",
            InstallMethod::Cargo.upgrade_command(),
        ),
        InstallMethod::Unknown => reject_with_hint(
            "无法识别安装方式，请使用官方 install.sh 手动升级",
            InstallMethod::Unknown.upgrade_command(),
        ),
        // Containers: the binary (`REVIEW_UPGRADE_INSTALL_DIR`) and the frontend
        // dist both live on writable volumes, so we can self-upgrade in place
        // and exit for the compose `restart: unless-stopped` policy to bring the
        // container back up with the new files.
        InstallMethod::Docker => start_upgrade_inner(state, UpgradeMode::ContainerWithFrontend).await,
        InstallMethod::Plain => start_upgrade_inner(state, UpgradeMode::BinaryOnly).await,
    }
}

fn reject_with_hint(message: &'static str, hint: &'static str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message, "upgradeHint": hint })),
    )
        .into_response()
}

/// Reject cross-site browser-triggered upgrades.
///
/// A request carrying an `Origin` header came from a browser (or a
/// browser-like client). Its authority must match the request `Host` header —
/// i.e. the request is same-site — otherwise it is a cross-site POST (CSRF,
/// DNS-rebinding) and is rejected with 403. Requests without `Origin` (curl,
/// scripts) are not browser-initiated and are allowed.
fn validate_origin(headers: &HeaderMap) -> Result<(), Box<axum::response::Response>> {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    // `Origin` is `scheme://authority`; only the authority is compared.
    let origin_authority = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(origin);
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if origin_authority == host {
        Ok(())
    } else {
        Err(Box::new(
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": "cross-origin upgrade rejected" })),
            )
                .into_response(),
        ))
    }
}

/// What an upgrade job must replace, and whether a restart follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeMode {
    /// Plain install: replace the binary only. The running process is never
    /// restarted; `done` means a restart is required to pick the change up.
    BinaryOnly,
    /// Container install: replace the binary and, when the release ships one,
    /// the frontend dist, then exit the process so the compose restart policy
    /// brings the container back up with the new files.
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

/// Claim the single-flight gate, refresh the GitHub check, and spawn the
/// upgrade pipeline for the given mode. Shared by the Plain (binary-only) and
/// Docker (binary + frontend dist) install paths.
async fn start_upgrade_inner(state: Arc<AppState>, mode: UpgradeMode) -> axum::response::Response {
    // Atomic single-flight claim under the job lock. From here on the job is
    // "running" (checking) and any concurrent POST is rejected with 409.
    {
        let mut job = state.upgrade.job.write().unwrap();
        if job.state.is_running() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "升级任务已在进行中，请稍后再试" })),
            )
                .into_response();
        }
        job.state = UpgradeJobState::Checking;
        job.message = "正在检查最新版本".to_string();
        job.started_at = Some(Utc::now());
    }

    let check = match refresh_check(&state).await {
        Ok(c) => c,
        Err(e) => {
            set_job(&state, UpgradeJobState::Failed, format!("升级检查失败：{e}"));
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": format!("check failed: {e}") })),
            )
                .into_response();
        }
    };

    if check.asset.is_none() || check.checksum_asset.is_none() {
        set_job(
            &state,
            UpgradeJobState::Failed,
            "当前平台没有可用的 release 资产，无法自动升级",
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no release asset for this platform" })),
        )
            .into_response();
    }

    let target = check.latest_version.to_string();
    {
        let mut job = state.upgrade.job.write().unwrap();
        job.state = UpgradeJobState::Downloading;
        job.message = "开始下载 release 资产".to_string();
        job.target_version = Some(target.clone());
    }

    let install_dir = resolve_install_dir();
    let task_state = state.clone();
    tokio::spawn(async move {
        run_upgrade_task(task_state, check, install_dir, mode).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "status": "started", "targetVersion": target })),
    )
        .into_response()
}

// ─── GET /api/v1/system/upgrade/status ───────────────────────────

async fn upgrade_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let job = state.upgrade.job.read().unwrap();
    Json(serde_json::json!({
        "state": job.state,
        "message": job.message,
        "currentVersion": job.current_version,
        "targetVersion": job.target_version,
    }))
}

// ─── binary upgrade executor (background task) ───────────────────

/// Runs the upgrade pipeline: download → verify → extract → smoke → replace,
/// plus (container mode) the frontend dist. On success in container mode the
/// process exits so the compose restart policy picks up the new files.
async fn run_upgrade_task(state: Arc<AppState>, check: UpdateCheck, install_dir: PathBuf, mode: UpgradeMode) {
    let format = check.platform.map(|p| p.format).unwrap_or(platform::AssetFormat::TarGz);
    // Unique staging dir under the system temp dir (never reuse a path).
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

        // Pre-replace smoke: never clobber a working binary with a broken one.
        if !smoke_test(&new_binary).await? {
            return Err(UpgradeError::invalid_data("新二进制冒烟测试失败，未替换"));
        }

        // Stage the frontend dist (container mode) — download/verify/extract
        // happen BEFORE the binary is touched, so a dist failure leaves the
        // binary untouched. The actual dist swap runs after the binary replace.
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

    // Best-effort staging cleanup so failed/interrupted upgrades do not leave
    // temp dirs accumulating on disk.
    let _ = std::fs::remove_dir_all(&staging);

    // Container restart trigger: keep the `done` state readable for a short
    // dwell so the frontend's status poll sees it, then exit so the compose
    // `restart: unless-stopped` policy brings the container back up. Skippable
    // via `REVIEW_UPGRADE_EXIT_AFTER=0` (the test seam).
    if mode == UpgradeMode::ContainerWithFrontend && succeeded && exit_after_upgrade_enabled() {
        tokio::time::sleep(DONE_DWELL).await;
        std::process::exit(0);
    }
}

/// Download, verify, and extract the frontend dist asset from `release` into
/// `staging`, returning the extracted directory that directly contains
/// `index.html`.
///
/// Returns `Ok(None)` when the release ships no `frontend-dist.tar.gz` (older
/// releases) — the caller degrades to a binary-only upgrade with a warning. A
/// release that *does* advertise the dist asset but fails to download, verify,
/// or contain `index.html` fails the upgrade.
async fn stage_frontend_dist(
    state: &AppState,
    release: &Release,
    staging: &Path,
) -> crate::upgrade::Result<Option<PathBuf>> {
    let Some(asset) = release.assets.iter().find(|a| a.name == FRONTEND_DIST_ASSET) else {
        tracing::warn!("release has no {FRONTEND_DIST_ASSET}; skipping frontend dist upgrade (binary-only)");
        return Ok(None);
    };

    set_job(state, UpgradeJobState::Downloading, "正在下载 frontend dist");
    let (dist_temp, _) = download::download_asset(&asset.download_url, staging, &asset.name, Some(asset.size)).await?;

    // The `.sha256` sidecar is optional for the dist: verify when present, warn
    // and proceed without it when absent (an older release may omit it).
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

/// Locate the subdirectory of `root` that directly contains `index.html`,
/// handling both a flat dist archive (`index.html` at the top) and a nested one
/// (`frontend/dist/index.html`).
fn find_dist_root(root: &Path) -> Option<PathBuf> {
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

/// Replace the *contents* of the live frontend dir with the staged dist —
/// never the directory itself.
///
/// The live dir (`/app/frontend/dist`) is a writable bind mount in production:
/// renaming a mount point fails with `EBUSY`, and the staged dist (extracted
/// under the system temp dir, usually `/tmp`) can live on a different
/// filesystem than `/app`, so a cross-directory `rename` would fail with
/// `EXDEV`. Both are avoided by copying the staged contents into a hidden
/// staging subdir *inside* the live dir and then swapping entries within it
/// (same filesystem, so each rename is atomic):
///
/// 1. copy staged contents → `<live>/.new-<nonce>/` (copy, so cross-device is fine)
/// 2. move every existing live entry → `<live>/.old-<nonce>/`
/// 3. move `.new-<nonce>/*` → `<live>/` (live is empty now, no collisions)
/// 4. remove `.old-<nonce>/` and `.new-<nonce>/`
///
/// Any failure after step 2 restores the parked old entries and removes the
/// partial new dir, so the live dir is never left half-swapped.
fn replace_frontend_dist(staged: &Path, frontend_dir: &Path) -> crate::upgrade::Result<()> {
    std::fs::create_dir_all(frontend_dir)?;

    let nonce: u64 = rand::random();
    let new_dir = frontend_dir.join(format!(".new-{nonce:x}"));
    let backup_dir = frontend_dir.join(format!(".old-{nonce:x}"));

    // Stage the new contents inside the live dir (same filesystem as the swap
    // targets below). Copy, never rename: the staged dist may be on /tmp.
    if let Err(e) = copy_dir_contents(staged, &new_dir) {
        let _ = std::fs::remove_dir_all(&new_dir);
        return Err(e);
    }

    // Park the old contents aside.
    if let Err(e) = park_old_contents(frontend_dir, &backup_dir, &new_dir) {
        let _ = std::fs::remove_dir_all(&new_dir);
        restore_old_contents(frontend_dir, &backup_dir);
        return Err(e);
    }

    // Bring the new contents into place (live is empty now, so these renames
    // cannot collide).
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

/// Recursively copy `src`'s contents into `dst` (permission bits included).
/// Cross-device safe: this is a plain copy, not a rename.
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

/// Move every entry of `live` except the two temp dirs into `backup_dir`.
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

/// Move the staged `.new-*` entries into `live` (expected to be empty).
fn bring_new_contents_in(live: &Path, new_dir: &Path) -> crate::upgrade::Result<()> {
    for entry in std::fs::read_dir(new_dir)? {
        let entry = entry?;
        std::fs::rename(entry.path(), live.join(entry.file_name()))?;
    }
    Ok(())
}

/// Move the parked old entries back into `live` and drop the backup dir
/// (rollback path; best-effort).
fn restore_old_contents(live: &Path, backup_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(backup_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::rename(entry.path(), live.join(entry.file_name()));
        }
    }
    let _ = std::fs::remove_dir_all(backup_dir);
}

/// Remove every non-temp entry of `live` — cleans up a half-done swap before
/// restoring the old contents (rollback path; best-effort).
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

/// Copy the new binary into `install_dir` atomically, with backup + rollback:
/// the old binary is kept until the post-replace smoke test passes.
async fn replace_binary(new_binary: &Path, install_dir: &Path, exe_name: &str) -> crate::upgrade::Result<()> {
    let exe_path = install_dir.join(exe_name);
    let nonce: u64 = rand::random();
    let backup = install_dir.join(format!(".{exe_name}.bak-{nonce:x}"));
    let staged = install_dir.join(format!(".{exe_name}.new-{nonce:x}"));

    let had_original = exe_path.exists();
    if had_original {
        std::fs::copy(&exe_path, &backup)?;
    }

    // Stage next to the target so the rename stays on one filesystem (atomic).
    std::fs::copy(new_binary, &staged)?;
    make_executable(&staged)?;

    // Atomic replace. On Windows, renaming over a running exe fails with a
    // sharing violation; restore the backup and report an actionable error.
    if let Err(e) = std::fs::rename(&staged, &exe_path) {
        let _ = std::fs::remove_file(&staged);
        if had_original {
            let _ = std::fs::rename(&backup, &exe_path);
        }
        return Err(UpgradeError::Io(e));
    }

    // Post-replace smoke: roll back if the installed binary cannot run, so a
    // future restart is not left with a broken binary.
    if !smoke_test(&exe_path).await? {
        if had_original {
            let _ = std::fs::rename(&backup, &exe_path);
        }
        return Err(UpgradeError::invalid_data("替换后冒烟测试失败，已回滚"));
    }
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

/// Run `<binary> --version` with a timeout; `Ok(true)` when it exits 0.
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

/// Recursively locate the extracted `review-engine` / `review-engine.exe`.
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

/// Ensure an executable bit set on Unix (no-op elsewhere).
fn make_executable(path: &Path) -> crate::upgrade::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Canonicalized `current_exe()` — resolves the macOS symlink-invocation path
/// (macOS `current_exe()` returns the symlink path used to exec, not the real
/// binary), mirroring `upgrade::InstallMethod::detect`. Falls back to the raw
/// path if canonicalization fails (e.g. the binary was deleted mid-run).
fn current_exe_canonical() -> Option<PathBuf> {
    let raw = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&raw).unwrap_or(raw))
}

/// Directory that receives the replaced binary: `REVIEW_UPGRADE_INSTALL_DIR`
/// override (tests / dry-run), else the *real* binary's directory — never a
/// symlink's directory, so a `reng` → `review-engine` symlink invocation
/// upgrades the actual binary, not the symlink (B1).
fn resolve_install_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("REVIEW_UPGRADE_INSTALL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    current_exe_canonical()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory that receives the replaced frontend dist:
/// `REVIEW_UPGRADE_FRONTEND_DIR` override (tests), else the container default
/// `/app/frontend/dist`. The dir need not exist yet — the entrypoint/compose
/// layer mounts it as a writable volume.
fn resolve_frontend_dir() -> PathBuf {
    std::env::var("REVIEW_UPGRADE_FRONTEND_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/app/frontend/dist"))
}

/// Whether the upgrade task should exit the process after a successful
/// container upgrade (so the compose `restart: unless-stopped` policy pulls the
/// container back up). `REVIEW_UPGRADE_EXIT_AFTER=0` disables it — the test
/// seam that keeps the spawned server alive to assert on the `done` state.
fn exit_after_upgrade_enabled() -> bool {
    std::env::var("REVIEW_UPGRADE_EXIT_AFTER")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Name of the *real* binary (canonicalized), e.g. `review-engine` even when
/// invoked through a `reng` symlink. The symlink keeps pointing at the same
/// name and stays valid after the replace (B1).
fn current_exe_name() -> String {
    current_exe_canonical()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "review-engine".to_string())
}

// ─── helpers ──────────────────────────────────────────────────────

fn set_job(state: &AppState, job_state: UpgradeJobState, message: impl Into<String>) {
    let mut job = state.upgrade.job.write().unwrap();
    job.state = job_state;
    job.message = message.into();
}

/// JSON `installMethod` value: U2's `InstallMethod` → API contract string.
fn install_method_str(method: InstallMethod) -> &'static str {
    match method {
        InstallMethod::Plain => "binary",
        InstallMethod::Brew => "brew",
        InstallMethod::Docker => "docker",
        InstallMethod::Cargo => "cargo",
        InstallMethod::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_method_mapping_matches_contract() {
        assert_eq!(install_method_str(InstallMethod::Plain), "binary");
        assert_eq!(install_method_str(InstallMethod::Brew), "brew");
        assert_eq!(install_method_str(InstallMethod::Docker), "docker");
        assert_eq!(install_method_str(InstallMethod::Cargo), "cargo");
        assert_eq!(install_method_str(InstallMethod::Unknown), "unknown");
    }

    #[test]
    fn install_dir_resolution_override_then_canonical() {
        // Single sequential test for the two env-dependent resolutions (they
        // share the same env var and must not race each other in parallel).
        let saved = std::env::var("REVIEW_UPGRADE_INSTALL_DIR").ok();

        // 1) Env override wins.
        std::env::set_var("REVIEW_UPGRADE_INSTALL_DIR", "/tmp/reng-upgrade-test");
        assert_eq!(resolve_install_dir(), PathBuf::from("/tmp/reng-upgrade-test"));

        // 2) Without override: derived from the canonicalized real binary —
        // absolute path, non-empty name.
        std::env::remove_var("REVIEW_UPGRADE_INSTALL_DIR");
        let dir = resolve_install_dir();
        assert!(dir.is_absolute(), "install dir must be absolute, got {dir:?}");
        assert!(!current_exe_name().is_empty());

        match saved {
            Some(v) => std::env::set_var("REVIEW_UPGRADE_INSTALL_DIR", v),
            None => {}
        }
    }

    // ─── Origin validation (B2) ────────────────────────────────

    fn headers_with(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(o) = origin {
            headers.insert("origin", o.parse().expect("valid origin header value"));
        }
        if let Some(h) = host {
            headers.insert("host", h.parse().expect("valid host header value"));
        }
        headers
    }

    #[test]
    fn origin_none_passes() {
        assert!(validate_origin(&headers_with(None, Some("127.0.0.1:8080"))).is_ok());
        assert!(validate_origin(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn origin_same_authority_passes() {
        assert!(validate_origin(&headers_with(Some("http://127.0.0.1:8080"), Some("127.0.0.1:8080"))).is_ok());
        assert!(validate_origin(&headers_with(Some("https://localhost:5173"), Some("localhost:5173"))).is_ok());
        // Origin default-port form vs Host without port.
        assert!(validate_origin(&headers_with(Some("http://example.com"), Some("example.com"))).is_ok());
    }

    #[test]
    fn origin_cross_site_rejected() {
        for (origin, host) in [
            ("http://evil.example", "127.0.0.1:8080"),
            ("http://127.0.0.1:9999", "127.0.0.1:8080"),
            ("https://evil.example", "localhost:5173"),
        ] {
            let err = validate_origin(&headers_with(Some(origin), Some(host))).expect_err("must reject");
            let status = err.status();
            assert_eq!(status, StatusCode::FORBIDDEN, "origin {origin} vs host {host}");
        }
        // Origin present but no Host at all → rejected (cannot be same-site).
        assert!(validate_origin(&headers_with(Some("http://evil.example"), None)).is_err());
    }

    // ─── frontend dist (container upgrade) ─────────────────────

    #[test]
    fn resolve_frontend_dir_override_then_default() {
        let saved = std::env::var("REVIEW_UPGRADE_FRONTEND_DIR").ok();

        std::env::set_var("REVIEW_UPGRADE_FRONTEND_DIR", "/tmp/reng-frontend-test");
        assert_eq!(resolve_frontend_dir(), PathBuf::from("/tmp/reng-frontend-test"));

        std::env::remove_var("REVIEW_UPGRADE_FRONTEND_DIR");
        assert_eq!(resolve_frontend_dir(), PathBuf::from("/app/frontend/dist"));

        match saved {
            Some(v) => std::env::set_var("REVIEW_UPGRADE_FRONTEND_DIR", v),
            None => {}
        }
    }

    #[test]
    fn find_dist_root_flat_nested_and_missing() {
        let dir = tempfile::tempdir().expect("temp dir");

        // Flat dist archive: index.html at the extraction root.
        let flat = dir.path().join("flat");
        std::fs::create_dir_all(&flat).expect("create flat");
        std::fs::write(flat.join("index.html"), "<html></html>").expect("write index.html");
        std::fs::write(flat.join("app.js"), "console.log(1)").expect("write app.js");
        assert_eq!(find_dist_root(&flat), Some(flat.clone()));

        // Nested dist archive: frontend/dist/index.html.
        let nested = dir.path().join("nested").join("frontend").join("dist");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(nested.join("index.html"), "<html></html>").expect("write nested index.html");
        let nested_root = dir.path().join("nested");
        assert_eq!(find_dist_root(&nested_root), Some(nested));

        // No index.html anywhere → None.
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(empty.join("assets")).expect("create empty");
        assert!(find_dist_root(&empty).is_none());
    }

    #[test]
    fn replace_frontend_dist_happy_path_and_rollback() {
        // Happy path: old content parked, staged contents (incl. nested dirs)
        // swapped in, temp dirs cleaned up.
        let dir = tempfile::tempdir().expect("temp dir");
        let live = dir.path().join("frontend").join("dist");
        std::fs::create_dir_all(&live).expect("create live");
        std::fs::write(live.join("old.txt"), "old").expect("write old");
        let staged = dir.path().join("staged-dist");
        std::fs::create_dir_all(&staged).expect("create staged");
        std::fs::write(staged.join("index.html"), "<html>new</html>").expect("write new index.html");
        std::fs::create_dir_all(staged.join("assets")).expect("create staged assets");
        std::fs::write(staged.join("assets/app.js"), "console.log(1)").expect("write staged asset");

        replace_frontend_dist(&staged, &live).expect("replace must succeed");
        assert!(live.join("index.html").exists(), "new dist must be live");
        assert!(live.join("assets/app.js").exists(), "nested asset must be live");
        assert!(!live.join("old.txt").exists(), "old dist must be gone");
        let leftovers: Vec<_> = std::fs::read_dir(&live)
            .expect("read live dir")
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(".new-") || n.starts_with(".old-")
            })
            .collect();
        assert!(leftovers.is_empty(), "temp dirs must be cleaned up, got {leftovers:?}");

        // Rollback: copy of a missing staged dir fails before any swap → live
        // dir untouched.
        let live2 = dir.path().join("frontend2").join("dist");
        std::fs::create_dir_all(&live2).expect("create live2");
        std::fs::write(live2.join("old.txt"), "old").expect("write old2");
        let missing_staged = dir.path().join("does-not-exist-dist");
        let err = replace_frontend_dist(&missing_staged, &live2).expect_err("missing staged must fail");
        assert!(matches!(err, UpgradeError::Io(_)), "got {err:?}");
        assert!(live2.join("old.txt").exists(), "old dist must be restored on failure");
    }

    /// Regression for the container **EBUSY** bug: `/app/frontend/dist` is a
    /// bind mount in production, and a mount point cannot be `rename(2)`d.
    /// `replace_frontend_dist` must never rename the live directory itself —
    /// only its contents. A real mount point cannot be created in a unit test,
    /// so we assert the mount-point semantics directly: the live dir keeps the
    /// same `dev`/`ino` across the replace, i.e. the directory (and the mount)
    /// stays in place while its contents are swapped.
    #[cfg(unix)]
    #[test]
    fn replace_frontend_dist_keeps_live_dir_on_mount_point_semantics() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let live = dir.path().join("dist");
        std::fs::create_dir_all(&live).expect("create live");
        std::fs::write(live.join("old.txt"), "old").expect("write old");
        let staged = dir.path().join("staged");
        std::fs::create_dir_all(&staged).expect("create staged");
        std::fs::write(staged.join("index.html"), "<html>new</html>").expect("write index.html");

        let before = std::fs::metadata(&live).expect("metadata before");
        replace_frontend_dist(&staged, &live).expect("replace must succeed");
        let after = std::fs::metadata(&live).expect("metadata after");

        assert_eq!(before.dev(), after.dev(), "live dir device must not change");
        assert_eq!(
            before.ino(), after.ino(),
            "live dir inode must not change — the directory itself must never be renamed/replaced (EBUSY on a mount point)"
        );
        assert!(live.join("index.html").exists());
        assert!(!live.join("old.txt").exists());
    }

    /// Regression for the container **EXDEV** bug: the staged dist is extracted
    /// under the system temp dir (typically `/tmp`), while the live dir lives
    /// under `/app` — a different filesystem. `replace_frontend_dist` must COPY
    /// the staged contents across, never `rename` a directory across devices. A
    /// true cross-device rename cannot be reproduced in a unit test (both
    /// tempdirs usually share `/tmp`), but the implementation performs no
    /// cross-directory rename at all, and this test pins the independent-
    /// location contract: staged and live live in unrelated temp trees, the
    /// replace succeeds, and the staged source is left intact (copied, not
    /// moved).
    #[test]
    fn replace_frontend_dist_accepts_staged_in_independent_location() {
        let staged_root = tempfile::tempdir().expect("staged root");
        let staged = staged_root.path().join("dist");
        std::fs::create_dir_all(staged.join("assets")).expect("create staged");
        std::fs::write(staged.join("index.html"), "<html>new</html>").expect("write index.html");
        std::fs::write(staged.join("assets/app.js"), "console.log(1)").expect("write asset");

        let live_root = tempfile::tempdir().expect("live root");
        let live = live_root.path().join("dist");
        std::fs::create_dir_all(&live).expect("create live");
        std::fs::write(live.join("old.txt"), "old").expect("write old");

        replace_frontend_dist(&staged, &live).expect("replace across independent locations must succeed");
        assert!(live.join("index.html").exists());
        assert!(live.join("assets/app.js").exists());
        assert!(!live.join("old.txt").exists());
        assert!(
            staged.join("index.html").exists(),
            "staged source must not be consumed by the replace (copy, not move)"
        );
    }

    #[test]
    fn stage_frontend_dist_returns_none_when_asset_missing() {
        // A release without `frontend-dist.tar.gz` degrades to binary-only:
        // Ok(None), no download attempted, nothing written to staging.
        let release = Release {
            tag_name: "v9.9.9".to_string(),
            html_url: "https://example.com".to_string(),
            published_at: "2026-01-01T00:00:00Z".to_string(),
            assets: vec![crate::upgrade::ReleaseAsset {
                name: "review-engine-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                download_url: "https://example.com/binary".to_string(),
                size: 1,
            }],
        };
        let state = AppState::new(vec![]);
        let staging = tempfile::tempdir().expect("temp dir");
        let staged = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(stage_frontend_dist(&state, &release, staging.path()));
        assert!(
            matches!(staged, Ok(None)),
            "no dist asset must degrade to Ok(None), got {staged:?}"
        );
        assert_eq!(
            std::fs::read_dir(staging.path()).expect("read staging").count(),
            0,
            "nothing may be downloaded when the dist asset is absent"
        );
    }

    #[test]
    fn exit_after_upgrade_gate_honors_env() {
        let saved = std::env::var("REVIEW_UPGRADE_EXIT_AFTER").ok();

        std::env::set_var("REVIEW_UPGRADE_EXIT_AFTER", "0");
        assert!(!exit_after_upgrade_enabled(), "0 must disable the exit");

        std::env::set_var("REVIEW_UPGRADE_EXIT_AFTER", "1");
        assert!(exit_after_upgrade_enabled(), "1 must keep the exit");

        std::env::remove_var("REVIEW_UPGRADE_EXIT_AFTER");
        assert!(
            exit_after_upgrade_enabled(),
            "unset must default to exiting (production)"
        );

        match saved {
            Some(v) => std::env::set_var("REVIEW_UPGRADE_EXIT_AFTER", v),
            None => {}
        }
    }
}
