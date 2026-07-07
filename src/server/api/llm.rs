//! REST API endpoints for LLM provider management.
//!
//! Lists configured providers and tests connectivity.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;

use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/providers", get(get_providers))
        .route("/providers/{id}/test", post(test_provider))
}

async fn get_providers(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let llm_configs = state.llm_configs.read().unwrap();
    let items: Vec<serde_json::Value> = llm_configs
        .iter()
        .enumerate()
        .map(|(i, cfg)| {
            let id = format!("{}-{}", cfg.provider, i);
            serde_json::json!({
                "id": id,
                "name": cfg.provider,
                "logo": logo_for_provider(&cfg.provider),
                "status": if !cfg.api_key.is_empty() { "healthy" } else { "offline" },
                "configured": !cfg.api_key.is_empty(),
                "latencyMs": 0,
                "errorRate": 0.0,
                "requestCount": 0,
                "usagePercent": 0,
                "sparkline": [],
                "lastChecked": chrono::Utc::now().to_rfc3339(),
            })
        })
        .collect();

    Json(serde_json::json!({ "items": items }))
}

async fn test_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(_): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let cfg = {
        let guard = state.llm_configs.read().unwrap();
        guard
            .iter()
            .enumerate()
            .find(|(i, c)| format!("{}-{}", c.provider, i) == id)
            .map(|(_, c)| c.clone())
    };

    let cfg = match cfg {
        Some(c) => c,
        None => {
            return Json(serde_json::json!({
                "success": false,
                "error": "Provider not found",
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }));
        }
    };

    if cfg.api_key.is_empty() {
        return Json(serde_json::json!({
            "success": false,
            "error": "Missing API key",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }));
    }

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

fn logo_for_provider(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "openai" => "OpenAI",
        "anthropic" => "Anthropic",
        "ollama" => "Ollama",
        "azure" => "Azure",
        "google" => "Google",
        "cohere" => "Cohere",
        _ => "Generic",
    }
    .to_string()
}
