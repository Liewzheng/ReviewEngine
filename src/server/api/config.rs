//! REST API endpoints for reading, validating, and retrieving the configuration schema.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use schemars::schema_for;
use std::sync::Arc;

use crate::models::AppConfig;
use crate::server::AppState;

use super::types::{ConfigValidateRequest, ConfigValidateResponse};

/// Sentinel masking a configured API key in `GET /config`. The frontend
/// renders it as "configured" and treats `""` or this sentinel as "leave
/// unchanged" on save (see `put_config`), so masking never destroys state.
const API_KEY_MASK: &str = "***";

/// A UI-supplied key means "keep the existing one" when it is empty (frontend
/// "leave blank = unchanged") or carries the mask sentinel `GET /config`
/// returned for a configured key.
fn is_blank_or_masked(key: &str) -> bool {
    key.is_empty() || key == API_KEY_MASK
}

/// Replace live API keys with the mask sentinel before serializing to the UI.
/// `GET /config` must never return a real LLM key or the GitLab API token.
fn mask_secrets(ui: &mut UiConfig) {
    if !ui.llm.openai_api_key.is_empty() {
        ui.llm.openai_api_key = API_KEY_MASK.to_string();
    }
    for provider in &mut ui.llm.providers {
        if !provider.api_key.is_empty() {
            provider.api_key = API_KEY_MASK.to_string();
        }
    }
    if !ui.gitlab.api_token.is_empty() {
        ui.gitlab.api_token = API_KEY_MASK.to_string();
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(get_config).put(put_config))
        .route("/schema", get(get_schema))
        .route("/validate", post(validate_config))
        .route("/test", post(test_config))
        .route("/models", post(fetch_models))
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut ui = state.ui_config.read().unwrap().clone();
    // A GitLab token configured outside the UI (CLI `--gitlab-token` /
    // `GITLAB_TOKEN` at startup) lives only in the runtime config and is never
    // mapped into `ui_config`. Surface it as the mask so the frontend never
    // shows "not set" for a configured token — and, since an empty `apiToken`
    // on PUT clears the token, an unrelated save can never silently wipe a
    // CLI/env-configured token.
    let runtime_has_token = crate::server::gitlab::gitlab_runtime()
        .read()
        .map(|rt| !rt.token.is_empty())
        .unwrap_or(false);
    if runtime_has_token && ui.gitlab.api_token.is_empty() {
        ui.gitlab.api_token = API_KEY_MASK.to_string();
    }
    // Never leak a live LLM API key or the GitLab API token to the UI: a
    // configured key comes back as the `***` mask, which the frontend treats
    // as "leave unchanged" on save.
    mask_secrets(&mut ui);
    Json(ui).into_response()
}

async fn get_schema() -> Json<serde_json::Value> {
    let schema = schema_for!(AppConfig);
    let value = serde_json::to_value(&schema).unwrap_or_default();
    Json(value)
}

async fn validate_config(body: Result<Json<ConfigValidateRequest>, JsonRejection>) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(json) => json,
        Err(rejection) => {
            // Malformed/missing body: keep the 422 status but return the same
            // JSON error shape as every other endpoint.
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({ "error": rejection.body_text() })),
            )
                .into_response();
        }
    };
    let mut errors = Vec::new();

    match crate::config::parse_toml(&body.body) {
        Ok(parsed) => match crate::config::merge_default(parsed) {
            Ok(config) => {
                if let Err(e) = crate::config::resolver::validate_experts(&config) {
                    errors.push(e.to_string());
                }
                let count = config.build_expert_defs().len();
                if errors.is_empty() {
                    (
                        StatusCode::OK,
                        Json(ConfigValidateResponse {
                            valid: true,
                            experts_count: Some(count),
                            errors: Vec::new(),
                        }),
                    )
                        .into_response()
                } else {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(ConfigValidateResponse {
                            valid: false,
                            experts_count: Some(count),
                            errors,
                        }),
                    )
                        .into_response()
                }
            }
            Err(e) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ConfigValidateResponse {
                    valid: false,
                    experts_count: None,
                    errors: vec![e.to_string()],
                }),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ConfigValidateResponse {
                valid: false,
                experts_count: None,
                errors: vec![e.to_string()],
            }),
        )
            .into_response(),
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConfig {
    #[serde(default)]
    pub gitlab: UiGitLabConfig,
    #[serde(default)]
    pub llm: UiLlmConfig,
    #[serde(default)]
    pub rules: UiRulesConfig,
    #[serde(default)]
    pub advanced: UiAdvancedConfig,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiGitLabConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub api_token: String,
    #[serde(default)]
    pub webhook_secret: String,
    #[serde(default)]
    pub webhook_signing_secret: String,
    #[serde(default)]
    pub default_project: String,
    #[serde(default)]
    pub mr_label: String,
    #[serde(default)]
    pub auto_review: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiLlmProviderConfig {
    pub provider: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_base_url: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiLlmConfig {
    #[serde(default)]
    pub primary_provider: String,
    #[serde(default)]
    pub openai_api_key: String,
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
    /// Multi-provider support — additive to the legacy single fields.
    #[serde(default)]
    pub providers: Vec<UiLlmProviderConfig>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiRulesConfig {
    #[serde(default = "default_min_score")]
    pub min_score: u32,
    #[serde(default)]
    pub block_on_critical: bool,
    #[serde(default)]
    pub auto_comment_on_pass: bool,
    #[serde(default = "default_comment_template")]
    pub comment_template: String,
    #[serde(default)]
    pub excluded_patterns: Vec<String>,
    #[serde(default = "default_required_experts")]
    pub required_experts: Vec<String>,
    #[serde(default = "default_max_review_duration_seconds")]
    pub max_review_duration_seconds: u32,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiAdvancedConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_log_retention_days")]
    pub log_retention_days: u32,
    #[serde(default = "default_sse_heartbeat_interval")]
    pub sse_heartbeat_interval: u32,
    #[serde(default = "default_max_concurrent_reviews")]
    pub max_concurrent_reviews: u32,
    #[serde(default = "default_request_timeout")]
    pub request_timeout: u32,
    #[serde(default = "default_enable_metrics")]
    pub enable_metrics: bool,
    #[serde(default)]
    pub debug_mode: bool,
}

fn default_max_tokens() -> u32 {
    4096
}
fn default_api_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_temperature() -> f32 {
    0.7
}
fn default_timeout_seconds() -> u32 {
    60
}
fn default_retry_attempts() -> u32 {
    3
}
fn default_min_score() -> u32 {
    75
}
fn default_comment_template() -> String {
    "Code review completed. Overall score: {{score}}/100. {{summary}}".to_string()
}
fn default_required_experts() -> Vec<String> {
    vec!["Security".to_string(), "Performance".to_string(), "Quality".to_string()]
}
fn default_max_review_duration_seconds() -> u32 {
    300
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_log_retention_days() -> u32 {
    30
}
fn default_sse_heartbeat_interval() -> u32 {
    15
}
fn default_max_concurrent_reviews() -> u32 {
    5
}
fn default_request_timeout() -> u32 {
    120
}
fn default_enable_metrics() -> bool {
    true
}

impl UiConfig {
    /// Build a `UiConfig` from the backend-native `AppConfig`, filling in
    /// sensible defaults for fields that only exist in the UI layer.
    pub fn from_app_config(app: &crate::models::AppConfig) -> Self {
        let mut ui = UiConfig::default();

        // Map LLM configs — legacy single fields
        for l in &app.llm {
            match l.provider.as_str() {
                "openai" => {
                    ui.llm.primary_provider = "openai".to_string();
                    ui.llm.openai_api_key = l.api_key.clone();
                    ui.llm.api_base_url = if l.api_base.is_empty() {
                        "https://api.openai.com/v1".to_string()
                    } else {
                        l.api_base.clone()
                    };
                    ui.llm.default_model = l.model.clone();
                    ui.llm.max_tokens = l.max_tokens;
                    ui.llm.temperature = l.temperature;
                }
                _ => {}
            }
        }
        // If primary_provider is still empty but we have at least one config
        if ui.llm.primary_provider.is_empty() {
            if let Some(first) = app.llm.first() {
                ui.llm.primary_provider = first.provider.clone();
                ui.llm.openai_api_key = first.api_key.clone();
                ui.llm.api_base_url = if first.api_base.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    first.api_base.clone()
                };
                ui.llm.default_model = first.model.clone();
                ui.llm.max_tokens = first.max_tokens;
                ui.llm.temperature = first.temperature;
            }
        }

        // Map all LLM configs as providers (multi-provider support)
        for l in &app.llm {
            ui.llm.providers.push(UiLlmProviderConfig {
                provider: l.provider.clone(),
                api_key: l.api_key.clone(),
                api_base_url: l.api_base.clone(),
                default_model: l.model.clone(),
                max_tokens: l.max_tokens,
                temperature: l.temperature,
                timeout_seconds: 60,
                retry_attempts: 3,
            });
        }

        // Map advanced settings
        ui.advanced.max_concurrent_reviews = app.max_concurrent_llm_calls.unwrap_or(5) as u32;
        ui.advanced.enable_metrics = true; // Default, overridden at runtime if needed

        // Apply defaults for fields not mapped from AppConfig
        if ui.llm.temperature == 0.0 {
            ui.llm.temperature = default_temperature();
        }
        if ui.rules.min_score == 0 {
            ui.rules.min_score = default_min_score();
        }

        ui
    }
}

/// Deep-merge `patch` into `base` (both JSON values), returning the result.
///
/// Object leaves merge key-by-key: a key present in `patch` overwrites the
/// same key in `base`, a key absent from `patch` keeps `base`'s value.
/// Non-object values (scalars, arrays, `null`) replace the base wholesale.
/// This gives `PUT /config` partial-update semantics: omitted fields keep
/// their stored value instead of being reset to a serde default.
fn merge_json(base: &serde_json::Value, patch: &serde_json::Value) -> serde_json::Value {
    match (base, patch) {
        (serde_json::Value::Object(base), serde_json::Value::Object(patch)) => {
            let mut merged = base.clone();
            for (key, value) in patch {
                let next = match merged.get(key) {
                    Some(existing) => merge_json(existing, value),
                    None => value.clone(),
                };
                merged.insert(key.clone(), next);
            }
            serde_json::Value::Object(merged)
        }
        (_, patch) => patch.clone(),
    }
}

/// Apply the submitted GitLab UI section to the runtime config, resolving the
/// API token with masking semantics (contract-4, aligned with LLM keys):
/// - a real value replaces the stored token;
/// - the mask sentinel `***` keeps the stored token;
/// - an empty string clears it.
///
/// Returns the resolved token (empty = unset) so the caller can persist the
/// mask/empty projection in `ui_config` and `GET /config` never leaks a live
/// token (see `mask_secrets`). Webhook secrets keep their existing
/// "non-empty overwrites, empty keeps" behavior.
fn apply_gitlab_runtime_config(
    gl_rt: &mut crate::server::gitlab::GitLabRuntimeConfig,
    ui_gl: &UiGitLabConfig,
) -> String {
    let submitted = ui_gl.api_token.clone();
    let new_token = if submitted.is_empty() {
        // Empty string explicitly clears the token.
        String::new()
    } else if submitted == API_KEY_MASK {
        // Mask sentinel means "keep the stored token".
        gl_rt.token.clone()
    } else {
        // A real token replaces the stored one.
        submitted
    };
    gl_rt.token = new_token.clone();

    if !ui_gl.webhook_secret.is_empty() {
        gl_rt.webhook_secret = ui_gl.webhook_secret.clone();
    }
    if !ui_gl.webhook_signing_secret.is_empty() {
        let s = ui_gl.webhook_signing_secret.clone();
        let signing_key = s
            .strip_prefix("whsec_")
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());
        gl_rt.signing_secret = Some(s);
        gl_rt.signing_key = signing_key;
    }

    new_token
}

async fn put_config(State(state): State<Arc<AppState>>, Json(payload): Json<serde_json::Value>) -> impl IntoResponse {
    // Contract: `PUT /config` is a PARTIAL update. Only fields present in the
    // request JSON overwrite the stored config; omitted fields keep their
    // current values. A sparse PUT (e.g. just `{"rules":{"minScore":90}}`)
    // must never silently zero temperature/minScore/maxConcurrentReviews/
    // enableMetrics or drop `llm.providers`. We merge the request over a
    // snapshot of the stored UI config, then run the existing save pipeline
    // unchanged — a full-form PUT (every field present) deep-merges to exactly
    // the request, so behaviour is identical to the old wholesale replace.
    if !payload.is_object() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "config update must be a JSON object"})),
        )
            .into_response();
    }
    let mut body: UiConfig = {
        let stored = state.ui_config.read().unwrap().clone();
        // UiConfig is a plain struct of serde-native types, so serializing the
        // stored config cannot fail; the fallback is unreachable defensive code.
        let stored_json = serde_json::to_value(&stored).unwrap_or_else(|_| serde_json::json!({}));
        match serde_json::from_value(merge_json(&stored_json, &payload)) {
            Ok(ui) => ui,
            Err(e) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({"error": format!("invalid config update: {e}")})),
                )
                    .into_response();
            }
        }
    };
    // Snapshot of currently-stored LLM configs, used to resolve "keep
    // unchanged" when the UI submits an empty or masked key (frontend "leave
    // blank = unchanged"; `GET /config` returns `***` for a configured key).
    let existing_llm = {
        let cfg_opt = state.app_config.read().unwrap();
        cfg_opt.as_ref().map(|arc| arc.llm.clone()).unwrap_or_default()
    };
    let existing_key_for = |provider: &str| -> String {
        existing_llm
            .iter()
            .find(|c| c.provider == provider)
            .map(|c| c.api_key.clone())
            .unwrap_or_default()
    };

    let mut new_llm_configs = Vec::new();

    // Legacy primary (openai): an empty or masked key means "keep the stored
    // key"; a real key replaces it.
    let mut primary_provider: Option<&str> = None;
    let openai_key = if is_blank_or_masked(&body.llm.openai_api_key) {
        existing_key_for("openai")
    } else {
        body.llm.openai_api_key.clone()
    };
    if !openai_key.is_empty() {
        primary_provider = Some("openai");
        new_llm_configs.push(crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: body.llm.default_model.clone(),
            api_key: openai_key,
            api_base: body.llm.api_base_url.clone(),
            max_tokens: body.llm.max_tokens,
            temperature: body.llm.temperature,
            disable_thinking: None,
        });
    }

    // Build LLM configs from multi-provider providers Vec. GET /config maps
    // every backend LLM config — including the primary — into `llm.providers`,
    // so a UI round-trip echoes the primary back inside this array. The primary
    // is authoritatively expressed by the legacy fields above; skip providers
    // entries with the same provider name or every save would add one more
    // duplicate (the `{provider}-{i}` id scheme cannot tell them apart anyway).
    for p in &body.llm.providers {
        if p.provider.is_empty() {
            continue;
        }
        if primary_provider == Some(p.provider.as_str()) {
            continue;
        }
        // Same "keep unchanged" semantics as the legacy field: a masked key
        // must never overwrite the stored secret with the `***` sentinel.
        let key = if is_blank_or_masked(&p.api_key) {
            existing_key_for(&p.provider)
        } else {
            p.api_key.clone()
        };
        if key.is_empty() {
            continue;
        }
        new_llm_configs.push(crate::models::LLMConfig {
            provider: p.provider.clone(),
            model: p.default_model.clone(),
            api_key: key,
            api_base: p.api_base_url.clone(),
            max_tokens: p.max_tokens,
            temperature: p.temperature,
            disable_thinking: None,
        });
    }

    // Sync the persisted UI config's key fields with what was actually stored:
    // a configured provider is recorded as the mask sentinel (never a live
    // key, never a blank that would read as "unconfigured"), so GET /config
    // stays self-consistent across "leave blank = unchanged" saves.
    let has_stored_key = |provider: &str| -> bool {
        new_llm_configs
            .iter()
            .any(|c| c.provider == provider && !c.api_key.is_empty())
    };
    body.llm.openai_api_key = if has_stored_key("openai") {
        API_KEY_MASK.to_string()
    } else {
        String::new()
    };
    for p in &mut body.llm.providers {
        p.api_key = if has_stored_key(&p.provider) {
            API_KEY_MASK.to_string()
        } else {
            String::new()
        };
    }

    let mut cfg_opt = state.app_config.write().unwrap();
    if let Some(arc) = cfg_opt.as_ref() {
        let mut new_cfg = (**arc).clone();
        if !new_llm_configs.is_empty() {
            new_cfg.llm = new_llm_configs.clone();
        }
        new_cfg.max_concurrent_llm_calls = Some(body.advanced.max_concurrent_reviews as usize);
        new_cfg.max_team_size = Some(body.advanced.max_concurrent_reviews as usize);
        *cfg_opt = Some(Arc::new(new_cfg));
    } else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "config not loaded"})),
        )
            .into_response();
    }
    drop(cfg_opt);

    if !new_llm_configs.is_empty() {
        let mut llm = state.llm_configs.write().unwrap();
        *llm = new_llm_configs;
    }

    // Persist full UI config so GET /config returns exactly what was saved
    let mut ui = state.ui_config.write().unwrap();
    *ui = body;

    // Sync GitLab config to the global runtime so webhook handler picks up changes
    // without requiring a restart. The API token follows LLM-key masking
    // semantics (`***` keeps, empty clears, a real value replaces); the real
    // token lives in the runtime only, and `ui_config` persists the mask/empty
    // projection so `GET /config` never echoes it (see `mask_secrets`).
    {
        let rt = crate::server::gitlab::gitlab_runtime();
        let mut gl_rt = rt.write().unwrap();
        let resolved_token = apply_gitlab_runtime_config(&mut gl_rt, &ui.gitlab);
        ui.gitlab.api_token = if resolved_token.is_empty() {
            String::new()
        } else {
            API_KEY_MASK.to_string()
        };
    }

    Json(serde_json::json!({"status": "saved"})).into_response()
}

#[derive(Debug, serde::Deserialize)]
struct TestConfigRequest {
    provider: String,
    model: String,
    api_key: String,
    api_base: String,
}

async fn test_config(Json(body): Json<TestConfigRequest>) -> impl IntoResponse {
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
    let result = test_llm_connectivity(&cfg).await;
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
struct ModelsRequest {
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

async fn fetch_models(Json(body): Json<ModelsRequest>) -> impl IntoResponse {
    use reqwest::Client;
    let client = Client::new();

    let base = if body.api_base.is_empty() {
        "https://api.openai.com/v1".to_string()
    } else {
        body.api_base.clone()
    };

    let url = format!("{}/models", base);
    let result = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", body.api_key))
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

async fn test_llm_connectivity(cfg: &crate::models::LLMConfig) -> anyhow::Result<()> {
    use reqwest::Client;
    let client = Client::new();

    let base = if cfg.api_base.is_empty() {
        match cfg.provider.to_lowercase().as_str() {
            "openai" => "https://api.openai.com/v1",
            "anthropic" => "https://api.anthropic.com",
            "ollama" => "http://localhost:11434",
            _ => "https://api.openai.com/v1",
        }
    } else {
        &cfg.api_base
    };

    let url = format!("{}/models", base);
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::response::IntoResponse;

    /// Seed an `AppState` with one openai provider carrying `key`, wired the
    /// same way `serve` does: `app_config.llm` + `ui_config` built from it.
    fn state_with_openai(key: &str) -> Arc<AppState> {
        let app: crate::models::AppConfig = serde_json::from_value(serde_json::json!({
            "llm": [{
                "provider": "openai",
                "model": "gpt-4o",
                "api_key": key,
                "api_base": "https://api.openai.com/v1",
                "max_tokens": 4096,
                "temperature": 0.7
            }]
        }))
        .expect("minimal AppConfig must deserialize");
        let state = Arc::new(AppState::new(app.llm.clone()));
        *state.app_config.write().unwrap() = Some(Arc::new(app.clone()));
        *state.ui_config.write().unwrap() = UiConfig::from_app_config(&app);
        state
    }

    fn stored_openai_key(state: &Arc<AppState>) -> String {
        state
            .app_config
            .read()
            .unwrap()
            .as_ref()
            .expect("app_config seeded")
            .llm
            .iter()
            .find(|c| c.provider == "openai")
            .map(|c| c.api_key.clone())
            .unwrap_or_default()
    }

    /// Security regression: `GET /config` must never return a live LLM key.
    #[tokio::test]
    async fn get_config_never_leaks_llm_api_key() {
        let state = state_with_openai("sk-super-secret");
        let resp = get_config(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["llm"]["openaiApiKey"], API_KEY_MASK);
        assert_ne!(body["llm"]["openaiApiKey"], "sk-super-secret");
        let providers = body["llm"]["providers"].as_array().expect("providers array");
        assert_eq!(providers[0]["apiKey"], API_KEY_MASK);
        assert_ne!(providers[0]["apiKey"], "sk-super-secret");
        // No field anywhere in the response may carry the secret.
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(!serialized.contains("sk-super-secret"), "secret leaked in {serialized}");
    }

    /// A GET → PUT round trip with masked keys (`***`) must keep the real key
    /// server-side — never replace it with the mask.
    #[tokio::test]
    async fn put_config_masked_round_trip_preserves_key() {
        let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
        let state = state_with_openai("sk-primary");
        let mut ui = state.ui_config.read().unwrap().clone();
        mask_secrets(&mut ui);

        let resp = put_config(
            State(state.clone()),
            Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(stored_openai_key(&state), "sk-primary");
        // And the persisted UI config still surfaces the mask, not the secret.
        assert_eq!(state.ui_config.read().unwrap().llm.openai_api_key, API_KEY_MASK);
    }

    /// "Leave blank = unchanged": a PUT with an empty key keeps the stored key.
    #[tokio::test]
    async fn put_config_blank_key_keeps_existing() {
        let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
        let state = state_with_openai("sk-primary");
        let mut ui = state.ui_config.read().unwrap().clone();
        ui.llm.openai_api_key = String::new();
        for p in &mut ui.llm.providers {
            p.api_key = String::new();
        }

        let resp = put_config(
            State(state.clone()),
            Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(stored_openai_key(&state), "sk-primary");
    }

    /// A real new key in a PUT replaces the stored one.
    #[tokio::test]
    async fn put_config_new_key_replaces_stored() {
        let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
        let state = state_with_openai("sk-old");
        let mut ui = state.ui_config.read().unwrap().clone();
        ui.llm.openai_api_key = "sk-new".to_string();
        ui.llm.providers[0].api_key = "sk-new".to_string();

        let resp = put_config(
            State(state.clone()),
            Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(stored_openai_key(&state), "sk-new");
    }

    /// Partial-update regression: a sparse PUT (only `rules.minScore` present)
    /// must update that field and keep every omitted field at its stored value —
    /// not reset it to a serde default. The old `*ui = body` replaced the whole
    /// config, so this request silently zeroed temperature / enableMetrics /
    /// maxConcurrentReviews and dropped `llm.providers`.
    #[tokio::test]
    async fn put_config_sparse_patch_preserves_omitted_fields() {
        let state = state_with_openai("sk-primary");
        {
            let ui = state.ui_config.read().unwrap();
            assert_eq!(ui.rules.min_score, 75, "baseline min_score");
            assert_eq!(ui.llm.temperature, 0.7, "baseline temperature");
            assert!(ui.advanced.enable_metrics, "baseline enable_metrics");
            assert_eq!(ui.advanced.max_concurrent_reviews, 5, "baseline max concurrent");
            assert_eq!(ui.llm.providers.len(), 1, "baseline providers");
        }

        let resp = put_config(
            State(state.clone()),
            Json(serde_json::json!({ "rules": { "minScore": 90 } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let ui = state.ui_config.read().unwrap();
        // Provided field updated...
        assert_eq!(ui.rules.min_score, 90);
        // ...every omitted field keeps its stored value.
        assert_eq!(ui.llm.temperature, 0.7, "omitted temperature must not reset");
        assert!(ui.advanced.enable_metrics, "omitted enableMetrics must not reset");
        assert_eq!(
            ui.advanced.max_concurrent_reviews, 5,
            "omitted maxConcurrentReviews must not reset"
        );
        assert_eq!(ui.llm.providers.len(), 1, "omitted providers must not be dropped");
        assert_eq!(ui.llm.openai_api_key, API_KEY_MASK, "stored key stays masked");
        assert_eq!(stored_openai_key(&state), "sk-primary", "live key preserved");
    }

    /// A sparse LLM patch must not clobber the stored key: with only
    /// `llm.temperature` present, the merged config carries the masked key,
    /// which resolves to the stored live key ("leave unchanged"), and the
    /// provider list survives untouched.
    #[tokio::test]
    async fn put_config_sparse_llm_patch_keeps_key_and_providers() {
        let state = state_with_openai("sk-primary");
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::json!({ "llm": { "temperature": 0.2 } })),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);

        let ui = state.ui_config.read().unwrap();
        assert_eq!(ui.llm.temperature, 0.2);
        assert_eq!(ui.llm.openai_api_key, API_KEY_MASK);
        assert_eq!(ui.llm.providers.len(), 1);
        assert_eq!(stored_openai_key(&state), "sk-primary");
    }

    /// An empty object `{}` is the degenerate sparse case: a no-op save that
    /// keeps every field, never a wipe.
    #[tokio::test]
    async fn put_config_empty_object_is_noop() {
        let state = state_with_openai("sk-primary");
        let before = state.ui_config.read().unwrap().clone();
        let resp = put_config(State(state.clone()), Json(serde_json::json!({})))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let after = state.ui_config.read().unwrap().clone();
        assert_eq!(before.rules.min_score, after.rules.min_score);
        assert_eq!(before.llm.temperature, after.llm.temperature);
        assert_eq!(before.llm.providers.len(), after.llm.providers.len());
        assert_eq!(stored_openai_key(&state), "sk-primary");
    }

    /// A non-object or type-invalid update must be rejected with 422, not
    /// silently accepted: `null`/`[]` are not a config patch, and a wrong-typed
    /// field (e.g. `"minScore": "high"`) fails the merged deserialization.
    #[tokio::test]
    async fn put_config_malformed_update_rejected() {
        let state = state_with_openai("sk-primary");
        for payload in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({ "rules": { "minScore": "high" } }),
        ] {
            let resp = put_config(State(state.clone()), Json(payload)).await.into_response();
            assert_eq!(
                resp.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "malformed patch must be rejected"
            );
        }
        // And nothing was persisted by any of the rejected patches.
        let ui = state.ui_config.read().unwrap();
        assert_eq!(ui.rules.min_score, 75);
        assert_eq!(ui.llm.temperature, 0.7);
        assert_eq!(stored_openai_key(&state), "sk-primary");
    }

    #[test]
    fn is_blank_or_masked_treats_empty_and_mask_as_keep() {
        assert!(is_blank_or_masked(""));
        assert!(is_blank_or_masked(API_KEY_MASK));
        assert!(!is_blank_or_masked("sk-real"));
    }

    // ── GitLab apiToken masking (contract-4) ──────────────────────────────

    /// Snapshot/restore guard for the global GitLab runtime, so a round-trip
    /// test can seed `gl_rt.token` without leaking state into the parallel
    /// webhook-handler tests, which read the same global via
    /// `effective_config`.
    struct GitLabRuntimeGuard(crate::server::gitlab::GitLabRuntimeConfig);

    impl GitLabRuntimeGuard {
        fn new() -> Self {
            Self(crate::server::gitlab::gitlab_runtime().read().unwrap().clone())
        }
    }

    impl Drop for GitLabRuntimeGuard {
        fn drop(&mut self) {
            let mut rt = crate::server::gitlab::gitlab_runtime().write().unwrap();
            *rt = self.0.clone();
        }
    }

    /// Every `put_config` call writes the global GitLab runtime (an empty
    /// submitted `apiToken` clears it), so any test that drives `put_config`
    /// races with the others on `gl_rt.token`. Serialize them with an
    /// async-aware mutex (held across the awaited handlers); tests that only
    /// read `ui_config`/`get_config` never take this lock.
    static GITLAB_RUNTIME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn gitlab_runtime_token() -> String {
        crate::server::gitlab::gitlab_runtime().read().unwrap().token.clone()
    }

    async fn config_response_body(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Security regression: `GET /config` must never return the GitLab API
    /// token in plaintext — a configured token comes back as the mask.
    #[tokio::test]
    async fn get_config_never_leaks_gitlab_api_token() {
        let state = state_with_openai("sk-super-secret");
        state.ui_config.write().unwrap().gitlab.api_token = "glpat-super-secret".to_string();

        let body = config_response_body(get_config(State(state)).await.into_response()).await;
        assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
        assert_ne!(body["gitlab"]["apiToken"], "glpat-super-secret");
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("glpat-super-secret"),
            "secret leaked in {serialized}"
        );
    }

    /// A token configured outside the UI (runtime only, e.g. CLI/env at
    /// startup) is surfaced as the mask by GET, so the frontend never shows
    /// "not set" for a configured token and an unrelated save cannot clear it.
    #[tokio::test]
    async fn get_config_surfaces_runtime_only_gitlab_token_as_mask() {
        let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
        let _guard = GitLabRuntimeGuard::new();
        let state = state_with_openai("sk-primary");
        // ui_config carries no GitLab token; only the runtime does.
        crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-cli".to_string();

        let body = config_response_body(get_config(State(state)).await.into_response()).await;
        assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(!serialized.contains("glpat-cli"), "secret leaked in {serialized}");
    }

    /// Pure semantics: the mask sentinel `***` keeps the stored token.
    #[test]
    fn gitlab_runtime_token_resolution_keeps_on_mask() {
        let mut rt = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: "glpat-stored".to_string(),
        };
        let mut ui = UiGitLabConfig::default();
        ui.api_token = API_KEY_MASK.to_string();

        let resolved = apply_gitlab_runtime_config(&mut rt, &ui);
        assert_eq!(resolved, "glpat-stored");
        assert_eq!(rt.token, "glpat-stored");
    }

    /// Pure semantics: an empty string clears the stored token.
    #[test]
    fn gitlab_runtime_token_resolution_clears_on_empty() {
        let mut rt = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: "glpat-stored".to_string(),
        };
        let ui = UiGitLabConfig::default(); // api_token = ""

        let resolved = apply_gitlab_runtime_config(&mut rt, &ui);
        assert!(resolved.is_empty());
        assert!(rt.token.is_empty());
    }

    /// Pure semantics: a real value replaces the stored token.
    #[test]
    fn gitlab_runtime_token_resolution_replaces_on_real_value() {
        let mut rt = crate::server::gitlab::GitLabRuntimeConfig {
            webhook_secret: String::new(),
            signing_secret: None,
            signing_key: None,
            token: "glpat-old".to_string(),
        };
        let mut ui = UiGitLabConfig::default();
        ui.api_token = "glpat-new".to_string();

        let resolved = apply_gitlab_runtime_config(&mut rt, &ui);
        assert_eq!(resolved, "glpat-new");
        assert_eq!(rt.token, "glpat-new");
    }

    /// End-to-end round trip through `PUT /config` / `GET /config`: GET masks
    /// the configured token, a masked (`***`) PUT keeps it, an empty PUT
    /// clears it, a real PUT replaces it — and the plaintext never leaks.
    #[tokio::test]
    async fn gitlab_api_token_mask_round_trip() {
        let _rt_lock = GITLAB_RUNTIME_LOCK.lock().await;
        let _guard = GitLabRuntimeGuard::new();
        let state = state_with_openai("sk-primary");

        // Seed a configured GitLab token, as the runtime would hold it.
        crate::server::gitlab::gitlab_runtime().write().unwrap().token = "glpat-stored".to_string();
        state.ui_config.write().unwrap().gitlab.api_token = API_KEY_MASK.to_string();

        // 1. GET returns the mask, never the plaintext.
        let body = config_response_body(get_config(State(state.clone())).await.into_response()).await;
        assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(!serialized.contains("glpat-stored"), "secret leaked in {serialized}");

        // 2. PUT with `***` keeps the stored token.
        let mut ui = state.ui_config.read().unwrap().clone();
        ui.gitlab.api_token = API_KEY_MASK.to_string();
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(gitlab_runtime_token(), "glpat-stored");
        assert_eq!(state.ui_config.read().unwrap().gitlab.api_token, API_KEY_MASK);

        // 3. PUT with an empty string clears the token.
        let mut ui = state.ui_config.read().unwrap().clone();
        ui.gitlab.api_token = String::new();
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(gitlab_runtime_token().is_empty());
        assert!(state.ui_config.read().unwrap().gitlab.api_token.is_empty());

        // 4. PUT with a real token replaces it and is never echoed back.
        let mut ui = state.ui_config.read().unwrap().clone();
        ui.gitlab.api_token = "glpat-new".to_string();
        let resp = put_config(
            State(state.clone()),
            Json(serde_json::to_value(&ui).expect("UiConfig must serialize")),
        )
        .await
        .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(gitlab_runtime_token(), "glpat-new");
        assert_eq!(state.ui_config.read().unwrap().gitlab.api_token, API_KEY_MASK);

        let body = config_response_body(get_config(State(state)).await.into_response()).await;
        assert_eq!(body["gitlab"]["apiToken"], API_KEY_MASK);
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(!serialized.contains("glpat-new"), "secret leaked in {serialized}");
    }
}
