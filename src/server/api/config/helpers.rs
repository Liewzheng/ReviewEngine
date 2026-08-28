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
    let cfg = crate::models::LLMConfig {
        provider: body.provider,
        model: body.model,
        api_key: body.api_key,
        api_base: body.api_base,
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
    };

    let start = std::time::Instant::now();
    let result = crate::llm::probe::probe_llm_connectivity(&cfg).await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let (success, error) = match result {
        Ok(_) => (true, None::<String>),
        Err(e) => (false, Some(e.to_string())),
    };

    Json(serde_json::json!({
        "success": success,
        "latencyMs": latency_ms,
        "error": error,
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
/// the stored token of the configured platform with the same baseUrl (the
/// same fallback pattern as the `fetch_models` fix). An explicit token is
/// used as-is, and a masked token with no matching platform keeps the old
/// behavior.
pub async fn test_git_platform(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TestGitPlatformRequest>,
) -> impl axum::response::IntoResponse {
    let base = body.base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Json(serde_json::json!({"ok": false, "error": "baseUrl is required"})).into_response();
    }
    // Same SSRF policy as review webhook callbacks (see the module docs):
    // subsumes the syntactic checks (parseable, http(s), host present) and
    // adds the address-range policy on the literal/resolved IPs.
    if let Err(reason) = crate::server::api::callback::validate_callback_url(&base).await {
        return Json(serde_json::json!({
            "ok": false,
            "error": format!("invalid baseUrl: {reason}")
        }))
        .into_response();
    }

    let token = if super::is_blank_or_masked(&body.token) {
        state
            .git_platforms
            .read()
            .unwrap()
            .iter()
            .find(|p| p.base_url == base)
            .map(|p| p.token.clone())
            .unwrap_or_else(|| body.token.clone())
    } else {
        body.token.clone()
    };

    let url = format!("{base}/api/v4/version");
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

    match request.send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status();
                return Json(serde_json::json!({
                    "ok": false,
                    "error": format!("HTTP {}", status),
                }))
                .into_response();
            }
            match resp.json::<GitLabVersionResponse>().await {
                Ok(parsed) => Json(serde_json::json!({ "ok": true, "version": parsed.version })).into_response(),
                Err(e) => Json(serde_json::json!({
                    "ok": false,
                    "error": format!("failed to parse response: {}", e),
                }))
                .into_response(),
            }
        }
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "error": e.to_string(),
        }))
        .into_response(),
    }
}
