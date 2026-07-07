use axum::extract::State;
use axum::Json;
use serde_json::json;
use std::sync::Arc;

use crate::server::AppState;

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn health_ready(State(state): State<Arc<AppState>>) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    if state.llm_configs.read().unwrap().is_empty() {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not ready", "reason": "no LLM configs configured" })),
        );
    }
    (axum::http::StatusCode::OK, Json(json!({ "status": "ready" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LLMConfig;

    #[tokio::test]
    async fn test_health_handler_returns_ok() {
        let res = health().await;
        assert_eq!(res.0["status"], "ok");
    }

    #[tokio::test]
    async fn test_health_ready_no_config() {
        let state = Arc::new(AppState::new(vec![]));
        let (status, body) = health_ready(axum::extract::State(state)).await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.0.get("status").unwrap(), "not ready");
    }

    #[tokio::test]
    async fn test_health_ready_with_config() {
        let state = Arc::new(AppState::new(vec![LLMConfig {
            provider: "test".to_string(),
            model: "test".to_string(),
            api_key: String::new(),
            api_base: String::new(),
            max_tokens: 100,
            temperature: 0.0,
        }]));
        let (status, body) = health_ready(axum::extract::State(state)).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body.0.get("status").unwrap(), "ready");
    }
}
