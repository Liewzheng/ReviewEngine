use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use std::sync::Arc;

use crate::server::state::UpgradeJobState;
use crate::server::AppState;
use crate::upgrade::InstallMethod;

use super::check::refresh_check;
use super::task::{resolve_install_dir, run_upgrade_task, UpgradeMode};

pub(crate) fn set_job(state: &AppState, job_state: UpgradeJobState, message: impl Into<String>) {
    let mut job = state.upgrade.job.write().unwrap();
    job.state = job_state;
    job.message = message.into();
}

pub(crate) async fn start_upgrade(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(resp) = validate_origin(&headers) {
        return *resp;
    }

    match state.upgrade.install_method {
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

pub(crate) fn validate_origin(headers: &HeaderMap) -> Result<(), Box<axum::response::Response>> {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
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

pub(crate) async fn start_upgrade_inner(state: Arc<AppState>, mode: UpgradeMode) -> axum::response::Response {
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
        job.download = None;
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

    if check.asset.is_none() {
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
    if check.checksum_asset.is_none() {
        set_job(
            &state,
            UpgradeJobState::Failed,
            "release 缺少 sha256 校验资产，无法自动升级",
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "release has no checksum asset for this platform" })),
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

pub(crate) async fn upgrade_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let job = state.upgrade.job.read().unwrap();
    Json(serde_json::json!({
        "state": job.state,
        "message": job.message,
        "currentVersion": job.current_version,
        "targetVersion": job.target_version,
        "download": job.download,
    }))
}
