//! REST API endpoints for reading, validating, and retrieving the configuration schema.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use schemars::schema_for;
use std::sync::Arc;

use crate::models::AppConfig;
use crate::server::AppState;

use super::types::{ConfigValidateRequest, ConfigValidateResponse};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(get_config).put(put_config))
        .route("/schema", get(get_schema))
        .route("/validate", post(validate_config))
        .route("/test", post(test_config))
        .route("/models", post(fetch_models))
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ui = state.ui_config.read().unwrap();
    Json(ui.clone()).into_response()
}

async fn get_schema() -> Json<serde_json::Value> {
    let schema = schema_for!(AppConfig);
    let value = serde_json::to_value(&schema).unwrap_or_default();
    Json(value)
}

async fn validate_config(Json(body): Json<ConfigValidateRequest>) -> impl IntoResponse {
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

        ui
    }
}

async fn put_config(State(state): State<Arc<AppState>>, Json(body): Json<UiConfig>) -> impl IntoResponse {
    let mut new_llm_configs = Vec::new();

    // Build LLM configs from UI fields (all non-empty keys are kept)
    if !body.llm.openai_api_key.is_empty() {
        new_llm_configs.push(crate::models::LLMConfig {
            provider: "openai".to_string(),
            model: body.llm.default_model.clone(),
            api_key: body.llm.openai_api_key.clone(),
            api_base: body.llm.api_base_url.clone(),
            max_tokens: body.llm.max_tokens,
            temperature: body.llm.temperature,
        });
    }

    // Build LLM configs from multi-provider providers Vec
    for p in &body.llm.providers {
        if !p.provider.is_empty() && !p.api_key.is_empty() {
            new_llm_configs.push(crate::models::LLMConfig {
                provider: p.provider.clone(),
                model: p.default_model.clone(),
                api_key: p.api_key.clone(),
                api_base: p.api_base_url.clone(),
                max_tokens: p.max_tokens,
                temperature: p.temperature,
            });
        }
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
