use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;

use crate::server::state::UpgradeJobState;
use crate::server::AppState;
use crate::upgrade::{
    current_asset_spec, find_asset, find_checksum_asset, GitHubReleaseClient, UpdateCheck, UpgradeError, Version,
};

use super::start::set_job;
use super::task::check_response;

const CHECK_CACHE_TTL: Duration = Duration::from_secs(3600);
const GITHUB_API_BASE: &str = "https://api.github.com";

pub(crate) async fn check_upgrade(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    {
        let cache = state.upgrade.cache.read().unwrap();
        if let Some(c) = cache.as_ref() {
            if c.cached_at + CHECK_CACHE_TTL > Utc::now() {
                return Json(check_response(&state, &c.check)).into_response();
            }
        }
    }

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

pub(crate) async fn refresh_check(state: &AppState) -> crate::upgrade::Result<UpdateCheck> {
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
    *state.upgrade.cache.write().unwrap() = Some(crate::server::state::UpgradeCache {
        check: check.clone(),
        cached_at: Utc::now(),
    });
    Ok(check)
}

pub(crate) fn cached_at_str(state: &AppState) -> String {
    state
        .upgrade
        .cache
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.cached_at.to_rfc3339())
        .unwrap_or_default()
}
