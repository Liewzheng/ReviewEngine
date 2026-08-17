//! REST API endpoints for reading, validating, and retrieving the configuration schema.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team

mod helpers;
mod put;
#[cfg(test)]
mod tests;
pub mod types;

use axum::{
    extract::{rejection::JsonRejection, State},
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
pub use self::types::UiConfig;
use self::types::API_KEY_MASK;

pub use self::helpers::{test_config, fetch_models};
pub use self::put::{put_config, apply_gitlab_runtime_config};
pub use self::types::{
    UiAdvancedConfig, UiGitLabConfig, UiLlmConfig, UiLlmProviderConfig, UiRulesConfig,
};

pub(crate) use self::put::merge_json;

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
