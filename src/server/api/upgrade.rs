//! Self-upgrade REST endpoints under `/api/v1/system/upgrade`.
//!
//! - `GET  /api/v1/system/upgrade/check`  — latest version + install hints (1h cache)
//! - `POST /api/v1/system/upgrade`        — start a binary upgrade (single-flight)
//! - `GET  /api/v1/system/upgrade/status` — job state machine
//!
//! Reuses the `crate::upgrade` core library (U2). The running process is
//! **never restarted**: after a successful binary replace the job reports
//! `done` with the message "服务需重启后生效".
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
    InstallMethod, UpdateCheck, UpgradeError, Version,
};

/// GitHub check results are cached server-side for 1h — the unauthenticated
/// GitHub API is rate limited to 60 requests/hour per IP.
const CHECK_CACHE_TTL: Duration = Duration::from_secs(3600);
/// Timeout for running the new binary's `--version` smoke test.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Default GitHub API base; overridable via `REVIEW_UPGRADE_API_BASE`
/// (self-hosted mirror or wiremock in tests).
const GITHUB_API_BASE: &str = "https://api.github.com";

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
        // Containers: the binary lives inside the image; self-replacement would
        // be wiped on the next container start. Delegate to the host.
        InstallMethod::Docker => {
            let instructions = InstallMethod::Docker.upgrade_command();
            set_job(
                &state,
                UpgradeJobState::NotSupported,
                "容器内不支持自替换，请在宿主机执行：git pull && docker compose up -d --build",
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "notSupported",
                    "instructions": instructions,
                    "note": "容器内请勿自替换二进制，请在宿主机拉取新镜像并重建容器",
                })),
            )
                .into_response()
        }
        InstallMethod::Plain => start_binary_upgrade(state).await,
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

/// Start a binary upgrade for a plain install. Single-flight: only one upgrade
/// may be in flight at a time (409 otherwise).
async fn start_binary_upgrade(state: Arc<AppState>) -> axum::response::Response {
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
        run_binary_upgrade_task(task_state, check, install_dir).await;
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

/// Runs download → verify → extract → replace → smoke. The running process is
/// never restarted; `done` means the on-disk binary was replaced and a restart
/// is required for it to take effect.
async fn run_binary_upgrade_task(state: Arc<AppState>, check: UpdateCheck, install_dir: PathBuf) {
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

        let exe_name = current_exe_name();
        replace_binary(&new_binary, &install_dir, &exe_name).await?;

        Ok::<(), UpgradeError>(())
    }
    .await;

    match result {
        Ok(()) => {
            set_job(&state, UpgradeJobState::Done, "升级完成，服务需重启后生效");
        }
        Err(e) => {
            tracing::warn!(error = %e, "binary upgrade failed");
            set_job(&state, UpgradeJobState::Failed, format!("升级失败：{e}"));
        }
    }

    // Best-effort staging cleanup so failed/interrupted upgrades do not leave
    // temp dirs accumulating on disk.
    let _ = std::fs::remove_dir_all(&staging);
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
}
