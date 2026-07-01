use anyhow::{Context, Result};
use std::sync::Arc;

use super::provider::{CompletionParams, CompletionResult, Message, ProviderRegistry};
use crate::models::*;

/// Client for LLM completion requests with multi-provider support.
#[derive(Clone)]
pub struct LLMClient {
    inner: reqwest::Client,
    provider_registry: Option<Arc<ProviderRegistry>>,
}

impl LLMClient {
    /// Create a new `LLMClient` with a default reqwest HTTP client (120s timeout).
    ///
    /// The provider registry is initially `None`; call [`with_registry`](Self::with_registry)
    /// to enable provider-based routing.
    #[allow(clippy::expect_used)]
    pub fn new() -> Self {
        Self {
            inner: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
            provider_registry: None,
        }
    }

    /// Set a provider registry for provider-based routing.
    pub fn with_registry(mut self, registry: Arc<ProviderRegistry>) -> Self {
        self.provider_registry = Some(registry);
        self
    }

    fn build_messages(system_prompt: &str, user_prompt: &str) -> Vec<Message> {
        vec![
            Message {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user_prompt.to_string(),
            },
        ]
    }

    fn record_llm_metrics(provider: &str, model: &str, success: bool) {
        let status = if success { "success" } else { "error" };
        crate::metrics::LLM_REQUESTS
            .with_label_values(&[provider, model, status])
            .inc();
    }

    /// Exponential backoff with jitter: base * 2^attempt + pseudo-random jitter.
    fn retry_delay(attempt: u32) -> std::time::Duration {
        let base_ms = 1000u64 * 2u64.pow(attempt);
        let jitter_ms = (attempt as u64 * 137) % 500; // pseudo-random jitter
        std::time::Duration::from_millis(base_ms.min(30_000) + jitter_ms.min(1000))
    }

    /// Complete using a specific LLM config (backward-compatible API).
    pub async fn complete(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionResult> {
        // If we have a provider registry, use it for better routing
        if let Some(ref registry) = self.provider_registry {
            if let Some(provider) = registry.get(&config.provider) {
                let params = CompletionParams {
                    model: config.model.clone(),
                    messages: Self::build_messages(system_prompt, user_prompt),
                    max_tokens: config.max_tokens,
                    temperature: config.temperature,
                    reasoning_effort: None,
                };
                let result = provider.complete(&params).await;
                Self::record_llm_metrics(&config.provider, &config.model, result.is_ok());
                return result;
            }
        }

        // Fallback: use the direct OpenAI-compatible HTTP approach (original behavior)
        let result = self.complete_direct(config, system_prompt, user_prompt).await;
        Self::record_llm_metrics(&config.provider, &config.model, result.is_ok());
        result
    }

    /// Direct HTTP-based completion (backward compat, OpenAI-compatible only).
    async fn complete_direct(
        &self,
        config: &LLMConfig,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionResult> {
        let _start = std::time::Instant::now();

        // Validate API base URL early so we give a helpful error instead of
        // reqwest's cryptic "builder error".
        let base = config.api_base.trim();
        if base.is_empty() || !base.starts_with("http") {
            // If api_base is empty, check if the user might have used `base_url`
            // (a common alias that we support via serde(alias)).
            anyhow::bail!(
                "LLM config '{}' has no api_base set. \
                 Use api_base = \"https://api.example.com/v1\" or \
                 LLM_CONFIG environment variable.",
                config.provider,
            );
        }
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": config.model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_prompt}
            ],
            "max_tokens": config.max_tokens,
            "temperature": config.temperature,
        });

        let latency_send = _start.elapsed();
        let resp = self
            .inner
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("builder error") {
                    anyhow::anyhow!(
                        "LLM request failed: invalid API base URL '{}'. \
                         Check api_base in your config — it should be like \
                         https://api.deepseek.com or https://api.openai.com/v1",
                        config.api_base,
                    )
                } else if msg.contains("dns error") || msg.contains("DNS") {
                    anyhow::anyhow!(
                        "LLM request failed: DNS resolution error for '{}'. \
                         Check api_base and network connectivity.",
                        config.api_base,
                    )
                } else if msg.contains("tls") || msg.contains("certificate") {
                    anyhow::anyhow!(
                        "LLM request failed: TLS error when connecting to '{}'. \
                         Try using http:// instead of https:// for local endpoints.",
                        config.api_base,
                    )
                } else {
                    anyhow::anyhow!("LLM request failed: {e}")
                }
            })?;

        let latency_resp = _start.elapsed();
        tracing::debug!(
            "LLM call to {}: send={:?} resp={:?} total={:?}",
            config.model,
            latency_send,
            latency_resp - latency_send,
            _start.elapsed()
        );

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("LLM API returned {status}: {text}");
        }

        tracing::debug!("parsing JSON at {:?}", _start.elapsed());
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {}", e))?;
        tracing::debug!("JSON parsed at {:?}", _start.elapsed());

        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("LLM response missing content"))?
            .to_string();

        let total_tokens = value["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let model = value["model"].as_str().unwrap_or(&config.model).to_string();

        Ok(CompletionResult {
            content,
            total_tokens,
            model,
        })
    }

    /// Complete with fallback across multiple configs.
    ///
    /// For each provider, retries up to 3 times with exponential backoff + jitter on
    /// rate-limit (429) and server-error (5xx) responses. Other 4xx errors fail fast.
    pub async fn complete_with_fallback(
        &self,
        configs: &[LLMConfig],
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionResult> {
        let mut last_error = anyhow::anyhow!("no LLM configs provided");
        let _cf_start = std::time::Instant::now();
        let max_retries = 3u32;

        tracing::debug!(
            "complete_with_fallback: {} config(s), system={}b user={}b",
            configs.len(),
            system_prompt.len(),
            user_prompt.len()
        );

        for (i, config) in configs.iter().enumerate() {
            for attempt in 0..max_retries {
                let _attempt_start = std::time::Instant::now();
                let result = self.complete(config, system_prompt, user_prompt).await;
                let attempt_dur = _attempt_start.elapsed();

                match result {
                    Ok(r) => {
                        tracing::debug!(
                            "Fallback attempt {}/{} SUCCESS: model={} took={:?} total={:?}",
                            i + 1,
                            attempt + 1,
                            config.model,
                            attempt_dur,
                            _cf_start.elapsed()
                        );
                        return Ok(r);
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let is_retriable = err_str.contains("429")
                            || err_str.contains("500")
                            || err_str.contains("502")
                            || err_str.contains("503")
                            || err_str.contains("504")
                            || err_str.contains("timeout")
                            || err_str.contains("connection")
                            || err_str.contains("spurious network");

                        if is_retriable && attempt + 1 < max_retries {
                            let delay = Self::retry_delay(attempt);
                            tracing::warn!(
                                provider = %config.provider,
                                model = %config.model,
                                attempt = attempt + 1,
                                max_retries = max_retries,
                                retry_delay_ms = delay.as_millis(),
                                error = %e,
                                "LLM request failed, retrying with backoff"
                            );
                            tokio::time::sleep(delay).await;
                            last_error = e;
                            continue;
                        }

                        tracing::warn!(
                            provider = %config.provider,
                            model = %config.model,
                            attempt = attempt + 1,
                            took = ?attempt_dur,
                            error = %e,
                            "LLM request failed, trying next fallback"
                        );
                        last_error = e;
                        break; // try next config
                    }
                }
            }
        }

        tracing::error!("all LLM configs exhausted");
        Err(last_error).context("all LLM providers failed")
    }
}
