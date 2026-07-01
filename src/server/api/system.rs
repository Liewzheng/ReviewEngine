//! REST API endpoints for system information: expert list, version, and health status.
//!
//! @module review-engine: part of the CodeReview Board virtual engineering team
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use std::sync::Arc;

use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/experts", get(list_experts))
        .route("/version", get(version_info))
}

async fn list_experts(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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

    Json(serde_json::json!({ "experts": experts })).into_response()
}

async fn version_info() -> Json<serde_json::Value> {
    let features: Vec<String> = {
        let mut f = vec!["cli".to_string()];
        if cfg!(feature = "python") {
            f.push("python".to_string());
        }
        f
    };
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "features": features,
    }))
}
