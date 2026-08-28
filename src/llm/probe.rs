//! Connectivity probe for LLM providers.
//!
//! Shared by the server provider-test endpoints
//! (`POST /api/v1/llm/providers/{id}/test`, `POST /api/v1/config/test`) and
//! the CLI `reng config provider test` command, so all three probe a
//! provider in exactly the same way.

use anyhow::Result;

use crate::models::LLMConfig;

/// The result of a successful probe, carrying the base URL that was
/// actually probed so callers can show the user exactly where the stored
/// key was sent.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    /// The effective base URL after applying the well-known defaults.
    pub resolved_base: String,
}

/// Resolve the effective base URL for a provider.
///
/// An explicit `api_base` always wins. When it is empty, the well-known
/// defaults for `openai` / `anthropic` / `ollama` apply — and nothing
/// else: for any other provider the stored bearer key would otherwise be
/// silently sent to `api.openai.com` with zero indication, so the probe
/// fails fast instead of making any request.
pub fn resolve_api_base(cfg: &LLMConfig) -> Result<String> {
    if !cfg.api_base.is_empty() {
        return Ok(cfg.api_base.clone());
    }
    match cfg.provider.to_lowercase().as_str() {
        "openai" => Ok("https://api.openai.com/v1".to_string()),
        "anthropic" => Ok("https://api.anthropic.com".to_string()),
        "ollama" => Ok("http://localhost:11434".to_string()),
        _ => anyhow::bail!(
            "api_base is required for provider \"{}\" (no well-known default)",
            cfg.provider
        ),
    }
}

/// Probe a provider with `GET {api_base}/models` using the stored bearer key.
///
/// When `api_base` is empty, falls back to the well-known base URL for
/// `openai` / `anthropic` / `ollama`; any other provider fails fast via
/// [`resolve_api_base`] before any network call is made. Succeeds on any
/// 2xx response; fails with `HTTP <status>` otherwise, or with the
/// underlying transport error (DNS, connect refused, timeout after 10s, …).
pub async fn probe_llm_connectivity(cfg: &LLMConfig) -> Result<ProbeOutcome> {
    use reqwest::Client;
    let client = Client::new();

    let base = resolve_api_base(cfg)?;

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
    Ok(ProbeOutcome { resolved_base: base })
}
