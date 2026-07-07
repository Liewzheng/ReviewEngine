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
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg_opt = state.app_config.read().unwrap();
    let cfg = match cfg_opt.as_ref() {
        Some(c) => c,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "config not loaded"})),
            )
                .into_response()
        }
    };

    let experts: Vec<serde_json::Value> = cfg
        .build_expert_defs()
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "role": e.config.role,
                "title": e.config.title,
                "trigger": format!("{:?}", e.trigger),
                "enabled": e.config.enabled,
            })
        })
        .collect();

    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "experts": experts,
        "commands": cfg.commands,
        "max_team_size": cfg.max_team_size,
        "max_concurrent_llm_calls": cfg.max_concurrent_llm_calls,
        // NOTE: sensitive fields (llm[].api_key, webhook_secret, etc.) are intentionally excluded
    }))
    .into_response()
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

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct UiConfig {
    gitlab: Option<UiGitLabConfig>,
    llm: Option<UiLlmConfig>,
    rules: Option<UiRulesConfig>,
    advanced: Option<UiAdvancedConfig>,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct UiGitLabConfig {
    url: String,
    api_token: String,
    webhook_secret: String,
    default_project: String,
    mr_label: String,
    auto_review: bool,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct UiLlmConfig {
    primary_provider: String,
    openai_api_key: String,
    anthropic_api_key: String,
    ollama_url: String,
    default_model: String,
    max_tokens: u32,
    temperature: f32,
    timeout_seconds: u32,
    retry_attempts: u32,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct UiRulesConfig {
    min_score: u32,
    block_on_critical: bool,
    auto_comment_on_pass: bool,
    comment_template: String,
    excluded_patterns: Vec<String>,
    required_experts: Vec<String>,
    max_review_duration_seconds: u32,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct UiAdvancedConfig {
    log_level: String,
    log_retention_days: u32,
    sse_heartbeat_interval: u32,
    max_concurrent_reviews: u32,
    request_timeout: u32,
    enable_metrics: bool,
    debug_mode: bool,
}

async fn put_config(State(state): State<Arc<AppState>>, Json(body): Json<UiConfig>) -> impl IntoResponse {
    let mut new_llm_configs = Vec::new();

    if let Some(ui_llm) = body.llm {
        if !ui_llm.openai_api_key.is_empty() {
            new_llm_configs.push(crate::models::LLMConfig {
                provider: "openai".to_string(),
                model: ui_llm.default_model.clone(),
                api_key: ui_llm.openai_api_key,
                api_base: String::new(),
                max_tokens: ui_llm.max_tokens,
                temperature: ui_llm.temperature,
            });
        }
        if !ui_llm.anthropic_api_key.is_empty() {
            new_llm_configs.push(crate::models::LLMConfig {
                provider: "anthropic".to_string(),
                model: ui_llm.default_model.clone(),
                api_key: ui_llm.anthropic_api_key,
                api_base: String::new(),
                max_tokens: ui_llm.max_tokens,
                temperature: ui_llm.temperature,
            });
        }
        if !ui_llm.ollama_url.is_empty() {
            new_llm_configs.push(crate::models::LLMConfig {
                provider: "ollama".to_string(),
                model: ui_llm.default_model.clone(),
                api_key: String::new(),
                api_base: ui_llm.ollama_url,
                max_tokens: ui_llm.max_tokens,
                temperature: ui_llm.temperature,
            });
        }
    }

    let mut cfg_opt = state.app_config.write().unwrap();
    if let Some(arc) = cfg_opt.as_ref() {
        let mut new_cfg = (**arc).clone();
        if !new_llm_configs.is_empty() {
            new_cfg.llm = new_llm_configs.clone();
        }
        if let Some(ui_advanced) = body.advanced {
            new_cfg.max_concurrent_llm_calls = Some(ui_advanced.max_concurrent_reviews as usize);
            new_cfg.max_team_size = Some(ui_advanced.max_concurrent_reviews as usize);
        }
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

    Json(serde_json::json!({"status": "saved"})).into_response()
}

#[derive(Debug, serde::Deserialize)]
struct TestConfigRequest {
    provider: String,
    model: String,
    api_key: String,
}

async fn test_config(Json(body): Json<TestConfigRequest>) -> impl IntoResponse {
    let cfg = crate::models::LLMConfig {
        provider: body.provider,
        model: body.model,
        api_key: body.api_key,
        api_base: String::new(),
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
