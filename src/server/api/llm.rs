//! REST API endpoints for LLM provider management.
//!
//! Lists configured providers, tests connectivity, and provides
//! CRUD operations for multi-provider management.

use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use std::sync::Arc;

use crate::server::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/providers", get(get_providers).post(add_provider))
        .route("/providers/{id}/test", post(test_provider))
        .route("/providers/{id}", delete(delete_provider).put(update_provider))
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
                // Echo the editable config back so the UI can prefill the edit
                // form. The API key is intentionally never returned.
                "apiBaseUrl": cfg.api_base,
                "defaultModel": cfg.model,
                "maxTokens": cfg.max_tokens,
                "temperature": cfg.temperature,
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

// ─── Add Provider ─────────────────────────────────────────────────

/// Request body for adding a new provider.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProviderRequest {
    pub provider: String,
    #[serde(default, alias = "defaultModel")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, alias = "apiBaseUrl")]
    pub api_base: String,
    #[serde(default = "default_add_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_add_temperature")]
    pub temperature: f32,
}

fn default_add_max_tokens() -> u32 {
    4096
}
fn default_add_temperature() -> f32 {
    0.7
}

async fn add_provider(
    State(state): State<Arc<AppState>>,
    body: Result<Json<AddProviderRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(json) => json,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": rejection.body_text() })),
            )
                .into_response();
        }
    };

    // Validate required fields
    if body.provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "provider name is required" })),
        )
            .into_response();
    }
    if body.api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "api_key is required" })),
        )
            .into_response();
    }

    let new_cfg = crate::models::LLMConfig {
        provider: body.provider.clone(),
        model: body.model.clone(),
        api_key: body.api_key.clone(),
        api_base: body.api_base.clone(),
        max_tokens: body.max_tokens,
        temperature: body.temperature,
    };

    // Derive the new id for the response
    let idx = {
        let guard = state.llm_configs.read().unwrap();
        guard.len()
    };
    let id = format!("{}-{}", body.provider, idx);

    // Add to state.llm_configs
    {
        let mut guard = state.llm_configs.write().unwrap();
        guard.push(new_cfg.clone());
    }

    // Sync with state.app_config if present
    {
        let mut cfg_opt = state.app_config.write().unwrap();
        if let Some(arc) = cfg_opt.as_ref() {
            let mut new_cfg_app = (**arc).clone();
            new_cfg_app.llm.push(new_cfg);
            *cfg_opt = Some(Arc::new(new_cfg_app));
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "provider": body.provider,
            "model": body.model,
            "configured": true,
        })),
    )
        .into_response()
}

// ─── Delete Provider ──────────────────────────────────────────────

async fn delete_provider(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> impl IntoResponse {
    // Parse id: expects format "{provider}-{idx}"
    let (provider, idx_str) = match id.rsplit_once('-') {
        Some((p, i)) => (p.to_string(), i.to_string()),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id format" })),
            )
                .into_response();
        }
    };

    let idx: usize = match idx_str.parse() {
        Ok(i) => i,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id index" })),
            )
                .into_response();
        }
    };

    let removed = {
        let mut guard = state.llm_configs.write().unwrap();
        if idx >= guard.len() {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider not found" })),
            )
                .into_response();
        }
        // Verify the provider at this index matches the expected provider name
        let actual_provider = &guard[idx].provider;
        if *actual_provider != provider {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider id mismatch" })),
            )
                .into_response();
        }
        let removed_cfg = guard.remove(idx);
        // Rebuild: keep providers contiguous — the id scheme {provider}-{i}
        // relies on index, so we keep the list as-is after removal (indices
        // shift for subsequent entries, but that's acceptable).
        removed_cfg
    };

    // Sync with state.app_config if present
    {
        let mut cfg_opt = state.app_config.write().unwrap();
        if let Some(arc) = cfg_opt.as_ref() {
            let mut new_cfg = (**arc).clone();
            new_cfg
                .llm
                .retain(|c| c.provider != removed.provider || c.api_key != removed.api_key);
            *cfg_opt = Some(Arc::new(new_cfg));
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "deleted", "id": id })),
    )
        .into_response()
}

// ─── Update Provider ──────────────────────────────────────────────

/// Request body for updating an existing provider.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderRequest {
    #[serde(default, alias = "defaultModel")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, alias = "apiBaseUrl")]
    pub api_base: String,
    #[serde(default = "default_add_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_add_temperature")]
    pub temperature: f32,
}

async fn update_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Result<Json<UpdateProviderRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(body) = match body {
        Ok(json) => json,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": rejection.body_text() })),
            )
                .into_response();
        }
    };

    let (provider, idx_str) = match id.rsplit_once('-') {
        Some((p, i)) => (p.to_string(), i.to_string()),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id format" })),
            )
                .into_response();
        }
    };

    let idx: usize = match idx_str.parse() {
        Ok(i) => i,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid provider id index" })),
            )
                .into_response();
        }
    };

    let updated = {
        let mut guard = state.llm_configs.write().unwrap();
        if idx >= guard.len() {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider not found" })),
            )
                .into_response();
        }
        let actual_provider = &guard[idx].provider;
        if *actual_provider != provider {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Provider id mismatch" })),
            )
                .into_response();
        }

        let cfg = &mut guard[idx];
        if !body.model.is_empty() {
            cfg.model = body.model.clone();
        }
        if !body.api_key.is_empty() {
            cfg.api_key = body.api_key.clone();
        }
        if !body.api_base.is_empty() {
            cfg.api_base = body.api_base.clone();
        }
        cfg.max_tokens = body.max_tokens;
        cfg.temperature = body.temperature;
        cfg.clone()
    };

    // Sync with state.app_config if present
    {
        let mut cfg_opt = state.app_config.write().unwrap();
        if let Some(arc) = cfg_opt.as_ref() {
            let mut new_cfg = (**arc).clone();
            if idx < new_cfg.llm.len() {
                new_cfg.llm[idx] = updated.clone();
            }
            *cfg_opt = Some(Arc::new(new_cfg));
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "updated",
            "id": id,
            "provider": provider,
            "model": updated.model,
        })),
    )
        .into_response()
}

// ─── Test Provider ────────────────────────────────────────────────

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
