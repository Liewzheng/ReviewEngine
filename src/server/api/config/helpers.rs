use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use std::sync::Arc;

use crate::server::AppState;

#[derive(Debug, serde::Deserialize)]
pub struct TestConfigRequest {
    provider: String,
    model: String,
    api_key: String,
    api_base: String,
}

pub async fn test_config(Json(body): Json<TestConfigRequest>) -> impl axum::response::IntoResponse {
    // RENG-36: deliberately NOT recorded in the provider health cache. This
    // endpoint probes a SUBMITTED config that may never be saved (and whose
    // provider/model/apiBase the dialog may still edit), so its verdict says
    // nothing about a stored provider. The stored-config test
    // (`POST /api/v1/llm/providers/{id}/test`) is the one that records, and the
    // next read of `/llm/providers` probes the saved config anyway.
    let cfg = crate::models::LLMConfig {
        provider: body.provider,
        model: body.model,
        api_key: body.api_key,
        api_base: body.api_base,
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    };

    let start = std::time::Instant::now();
    let result = crate::llm::probe::probe_llm_connectivity(&cfg).await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let (success, error, resolved_base) = match result {
        Ok(outcome) => (true, None::<String>, Some(outcome.resolved_base)),
        Err(e) => (
            false,
            Some(e.to_string()),
            crate::llm::probe::resolve_api_base(&cfg).ok(),
        ),
    };

    Json(serde_json::json!({
        "success": success,
        "latencyMs": latency_ms,
        "error": error,
        "resolvedApiBase": resolved_base,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

#[derive(Debug, serde::Deserialize)]
pub struct ModelsRequest {
    api_base: String,
    api_key: String,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModel>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiModel {
    id: String,
}

pub async fn fetch_models(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ModelsRequest>,
) -> impl axum::response::IntoResponse {
    use reqwest::Client;
    let client = Client::new();

    let base = if body.api_base.is_empty() {
        "https://api.openai.com/v1".to_string()
    } else {
        body.api_base.clone()
    };

    // The UI never sees real keys (`GET /config` masks them as `***`), so a
    // blank or masked probe key means "use the server-side one": fall back to
    // the effective configured key for the same api_base (seeded from env
    // LLM_CONFIG or saved via PUT /config). An explicit key is used as-is,
    // and a masked key with no matching config keeps the old behavior.
    let api_key = if super::is_blank_or_masked(&body.api_key) {
        state
            .llm_configs
            .read()
            .unwrap()
            .iter()
            .find(|c| c.api_base == body.api_base)
            .map(|c| c.api_key.clone())
            .unwrap_or_else(|| body.api_key.clone())
    } else {
        body.api_key.clone()
    };

    let url = format!("{}/models", base);
    let result = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    match result {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                return Json(serde_json::json!({
                    "models": [],
                    "error": format!("HTTP {}", status),
                }))
                .into_response();
            }
            match resp.json::<OpenAiModelsResponse>().await {
                Ok(parsed) => {
                    let mut models: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
                    models.sort();
                    Json(serde_json::json!({ "models": models })).into_response()
                }
                Err(e) => Json(serde_json::json!({
                    "models": [],
                    "error": format!("failed to parse response: {}", e),
                }))
                .into_response(),
            }
        }
        Err(e) => Json(serde_json::json!({
            "models": [],
            "error": e.to_string(),
        }))
        .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestGitPlatformRequest {
    base_url: String,
    #[serde(default)]
    token: String,
    /// RENG-96: the entry's stable id, when the caller knows it. Used first
    /// to resolve a masked/blank probe token to the stored secret — the same
    /// id-first identity rule as the config save path, so repointing
    /// `baseUrl` does not orphan the probe.
    #[serde(default)]
    id: String,
    /// RENG-101: the container-reachable address of a possibly-unsaved edit.
    /// When set (and non-empty after trimming), it is the address the probe
    /// hits — mirroring `review_base_url()` — falling back to the matched
    /// stored entry's `internal_base_url`, then to `base_url`.
    #[serde(default)]
    internal_base_url: String,
}

#[derive(Debug, serde::Deserialize)]
struct GitLabVersionResponse {
    version: String,
}

/// Probe a git platform instance: `GET {baseUrl}/api/v4/version` with the
/// supplied token. Always answers 200 — probe failures are reported in the
/// body (`{"ok": false, "error": "..."}`), matching the `fetch_models`
/// pattern.
///
/// SSRF: the target is validated with the exact guard used for review
/// webhook callbacks ([`crate::server::api::callback::validate_callback_url`])
/// — `http` only for loopback/private targets, link-local/metadata/
/// unspecified always blocked (literal IP and DNS-resolved, fail-closed), so
/// the probe cannot be aimed at e.g. the cloud metadata endpoint, while
/// private-network GitLab instances (the primary use case) keep working.
///
/// The UI never sees real tokens (`GET /config` masks them as `***`), so a
/// blank or masked probe token means "use the server-side one": fall back to
/// the stored token of the configured platform with the same id (RENG-96,
/// when the caller carries it), else the same baseUrl (the same fallback
/// pattern as the `fetch_models` fix). An explicit token is used as-is, and
/// a masked token with no matching platform keeps the old behavior.
///
/// RENG-101: the probed address is NOT necessarily `baseUrl`. It mirrors
/// `review_base_url()` — the submitted `internalBaseUrl` (a possibly-unsaved
/// edit) wins, else the matched stored entry's `internal_base_url`, else
/// `baseUrl` — so the probe exercises the address the review would actually
/// fetch. The health verdict is still recorded under the entry's external
/// `base_url` (the store's cache key and `(base_url, token)` fingerprint),
/// and every response carries `probedUrl` = the address that was actually
/// hit.
pub async fn test_git_platform(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TestGitPlatformRequest>,
) -> impl axum::response::IntoResponse {
    let base = body.base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Json(serde_json::json!({"ok": false, "error": "baseUrl is required"})).into_response();
    }

    // Resolve the stored entry ONCE (id first, then baseUrl — the same
    // identity rule as the config save path) and reuse it for both the
    // masked-token fallback and the probe-target fallback.
    let stored = {
        let platforms = state.git_platforms.read().unwrap();
        if body.id.is_empty() {
            platforms.iter().find(|p| p.base_url == base).cloned()
        } else {
            platforms
                .iter()
                .find(|p| p.id == body.id)
                .or_else(|| platforms.iter().find(|p| p.base_url == base))
                .cloned()
        }
    };

    // Probe target resolution order (mirrors `review_base_url()`): submitted
    // internalBaseUrl → matched stored entry's internal_base_url → baseUrl,
    // each trimmed of whitespace and trailing `/` before the non-empty check.
    let submitted_internal = body.internal_base_url.trim().trim_end_matches('/').to_string();
    let probe_target = if !submitted_internal.is_empty() {
        submitted_internal
    } else if let Some(entry) = &stored {
        let entry_internal = entry.internal_base_url.trim().trim_end_matches('/').to_string();
        if entry_internal.is_empty() {
            base.clone()
        } else {
            entry_internal
        }
    } else {
        base.clone()
    };

    // Same SSRF policy as review webhook callbacks (see the module docs):
    // subsumes the syntactic checks (parseable, http(s), host present) and
    // adds the address-range policy on the literal/resolved IPs. The error
    // names which field supplied the invalid target.
    let target_field = if probe_target == base {
        "baseUrl"
    } else {
        "internalBaseUrl"
    };
    if let Err(reason) = crate::server::api::callback::validate_callback_url(&probe_target).await {
        return Json(serde_json::json!({
            "ok": false,
            "error": format!("invalid {target_field}: {reason}"),
            "probedUrl": probe_target,
        }))
        .into_response();
    }

    let token = if super::is_blank_or_masked(&body.token) {
        stored
            .as_ref()
            .map(|p| p.token.clone())
            .unwrap_or_else(|| body.token.clone())
    } else {
        body.token.clone()
    };

    let url = format!("{probe_target}/api/v4/version");
    let request = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10));
    // GitLab accepts PATs via Bearer (the review client authenticates the
    // same way); an empty token probes unauthenticated, surfacing the 401.
    let request = if token.is_empty() {
        request
    } else {
        request.header("Authorization", format!("Bearer {token}"))
    };

    let started = std::time::Instant::now();
    match request.send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                let error = format!("HTTP {}", status);
                // RENG-97: this probe IS the single source of truth for the
                // integration's health — record the outcome so the dashboard
                // and `/system/health` report the same failure instead of
                // "Configured" from entry presence. RENG-101: keyed on the
                // entry's external base_url, not the (possibly internal)
                // probed address.
                state.git_health.record(
                    &base,
                    &token,
                    crate::server::api::git_health::GitPlatformHealth::error(error.clone()),
                );
                return Json(serde_json::json!({
                    "ok": false,
                    "error": error,
                    "probedUrl": probe_target,
                }))
                .into_response();
            }
            match resp.json::<GitLabVersionResponse>().await {
                Ok(parsed) => {
                    let latency_ms = started.elapsed().as_millis() as u64;
                    state.git_health.record(
                        &base,
                        &token,
                        crate::server::api::git_health::GitPlatformHealth::healthy(
                            Some(parsed.version.clone()),
                            latency_ms,
                        ),
                    );
                    Json(serde_json::json!({ "ok": true, "version": parsed.version, "probedUrl": probe_target }))
                        .into_response()
                }
                Err(e) => {
                    let error = format!("failed to parse response: {}", e);
                    state.git_health.record(
                        &base,
                        &token,
                        crate::server::api::git_health::GitPlatformHealth::error(error.clone()),
                    );
                    Json(serde_json::json!({
                        "ok": false,
                        "error": error,
                        "probedUrl": probe_target,
                    }))
                    .into_response()
                }
            }
        }
        Err(e) => {
            let error = e.to_string();
            state.git_health.record(
                &base,
                &token,
                crate::server::api::git_health::GitPlatformHealth::error(error.clone()),
            );
            Json(serde_json::json!({
                "ok": false,
                "error": error,
                "probedUrl": probe_target,
            }))
            .into_response()
        }
    }
}
