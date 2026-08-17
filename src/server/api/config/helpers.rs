use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

use super::types::UiConfig;

#[derive(Debug, serde::Deserialize)]
pub(crate) struct TestConfigRequest {
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
pub(crate) struct ModelsRequest {
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

pub async fn fetch_models(Json(body): Json<ModelsRequest>) -> impl axum::response::IntoResponse {
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

pub(crate) async fn test_llm_connectivity(cfg: &crate::models::LLMConfig) -> anyhow::Result<()> {
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
