//! Connectivity probe for LLM providers.
//!
//! Shared by the server provider-test endpoints
//! (`POST /api/v1/llm/providers/{id}/test`, `POST /api/v1/config/test`) and
//! the CLI `reng config provider test` command, so all three probe a
//! provider in exactly the same way.

use anyhow::Result;

use crate::models::LLMConfig;

/// Probe a provider with `GET {api_base}/models` using the stored bearer key.
///
/// When `api_base` is empty, falls back to the well-known base URL for
/// `openai` / `anthropic` / `ollama` (default: OpenAI). Succeeds on any 2xx
/// response; fails with `HTTP <status>` otherwise, or with the underlying
/// transport error (DNS, connect refused, timeout after 10s, …).
pub async fn probe_llm_connectivity(cfg: &LLMConfig) -> Result<()> {
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
