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
        .route("/", get(get_config))
        .route("/schema", get(get_schema))
        .route("/validate", post(validate_config))
}

async fn get_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = match &state.app_config {
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
    match crate::config::parse_toml(&body.body) {
        Ok(parsed) => match crate::config::merge_default(parsed) {
            Ok(config) => {
                let count = config.build_expert_defs().len();
                (
                    StatusCode::OK,
                    Json(ConfigValidateResponse {
                        valid: true,
                        experts_count: Some(count),
                        errors: Vec::new(),
                    }),
                )
                    .into_response()
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
