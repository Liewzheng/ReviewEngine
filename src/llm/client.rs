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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_client_new() {
        let client = LLMClient::new();
        // Should construct without panic
        let _ = client;
    }

    #[test]
    fn test_retry_delay_increases_with_attempt() {
        let d0 = LLMClient::retry_delay(0);
        let d1 = LLMClient::retry_delay(1);
        let d2 = LLMClient::retry_delay(2);

        assert!(d1 > d0);
        assert!(d2 > d1);
    }

    #[test]
    fn test_retry_delay_has_jitter() {
        // Attempt 1 adds jitter = (1 * 137) % 500 = 137 ms
        let d1 = LLMClient::retry_delay(1);
        // Base is 2000 ms, jitter is up to 500 ms (capped at 1000)
        assert!(d1 >= std::time::Duration::from_millis(2000));
        assert!(d1 <= std::time::Duration::from_millis(2500));
    }

    #[test]
    fn test_retry_delay_capped_at_30s() {
        let d = LLMClient::retry_delay(10);
        // base = 1000 * 2^10 = 1,024,000 ms, capped at 30,000
        assert!(d <= std::time::Duration::from_secs(31)); // 30s base + up to 1s jitter
    }

    #[test]
    fn test_build_messages_structure() {
        let msgs = LLMClient::build_messages("sys", "user");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[0].content, "sys");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content, "user");
    }

    #[test]
    fn test_complete_direct_rejects_empty_api_base() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let client = LLMClient::new();
            let config = LLMConfig {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                api_key: "sk-test".to_string(),
                api_base: String::new(),
                max_tokens: 4096,
                temperature: 0.3,
            };
            let result = client.complete_direct(&config, "sys", "user").await;
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("api_base"));
        });
    }

    #[test]
    fn test_provider_registry_from_configs_anthropic_default_url() {
        let configs = vec![LLMConfig {
            provider: "anthropic".to_string(),
            model: "claude-3".to_string(),
            api_key: "test-key".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert_eq!(order, vec!["anthropic"]);
        assert!(registry.get("anthropic").is_some());
    }

    #[test]
    fn test_provider_registry_from_configs_openai_default_url() {
        let configs = vec![LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key: "test-key".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert_eq!(order, vec!["openai"]);
        assert!(registry.get("openai").is_some());
    }

    #[test]
    fn test_provider_registry_from_configs_custom_provider_fallback() {
        let configs = vec![LLMConfig {
            provider: "custom".to_string(),
            model: "custom-model".to_string(),
            api_key: "test-key".to_string(),
            api_base: "https://api.custom.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert_eq!(order, vec!["custom"]);
        assert!(registry.get("custom").is_some());
    }

    #[test]
    fn test_provider_registry_from_configs_empty_provider_name() {
        let configs = vec![LLMConfig {
            provider: String::new(),
            model: "default-model".to_string(),
            api_key: "test-key".to_string(),
            api_base: "https://api.custom.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert_eq!(order, vec!["openai-compatible"]);
        assert!(registry.get("openai-compatible").is_some());
    }

    #[test]
    fn test_provider_registry_from_configs_multiple_providers() {
        let configs = vec![
            LLMConfig {
                provider: "openai".to_string(),
                model: "gpt-4".to_string(),
                api_key: "test-key".to_string(),
                api_base: String::new(),
                max_tokens: 4096,
                temperature: 0.3,
            },
            LLMConfig {
                provider: "anthropic".to_string(),
                model: "claude-3".to_string(),
                api_key: "test-key".to_string(),
                api_base: String::new(),
                max_tokens: 4096,
                temperature: 0.3,
            },
        ];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert_eq!(order, vec!["openai", "anthropic"]);
        assert!(registry.get("openai").is_some());
        assert!(registry.get("anthropic").is_some());
    }

    #[test]
    fn test_provider_registry_from_configs_preserves_user_api_base() {
        let configs = vec![LLMConfig {
            provider: "anthropic".to_string(),
            model: "claude-3".to_string(),
            api_key: "test-key".to_string(),
            api_base: "https://custom.anthropic.com".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, _order) = ProviderRegistry::from_configs(&configs);
        // The provider should exist; we can't directly inspect the base URL,
        // but we verify the registry construction succeeds.
        assert!(registry.get("anthropic").is_some());
    }

    #[test]
    fn test_provider_registry_empty_configs() {
        let configs: Vec<LLMConfig> = vec![];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert!(order.is_empty());
        assert!(registry.names().is_empty());
    }

    // ─── Mock provider for retry tests ───────────────────────────────────

    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockProvider {
        name: String,
        call_count: AtomicUsize,
        fail_until: usize,
        error_msg: String,
    }

    impl MockProvider {
        fn new(name: &str, fail_until: usize, error_msg: &str) -> Self {
            Self {
                name: name.to_string(),
                call_count: AtomicUsize::new(0),
                fail_until,
                error_msg: error_msg.to_string(),
            }
        }
    }

    #[async_trait]
    impl super::super::provider::LLMProvider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }

        async fn complete(&self, _params: &CompletionParams) -> Result<CompletionResult> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count < self.fail_until {
                anyhow::bail!("{}", self.error_msg)
            } else {
                Ok(CompletionResult {
                    content: "success".to_string(),
                    total_tokens: 10,
                    model: "mock".to_string(),
                })
            }
        }
    }

    #[tokio::test]
    async fn test_complete_with_fallback_success_on_first_try() {
        let client = LLMClient::new();
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(MockProvider::new("mock", 0, "unused")));
        let client = client.with_registry(Arc::new(registry));

        let configs = vec![LLMConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            api_key: "test".to_string(),
            api_base: "https://api.mock.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];

        let result = client.complete_with_fallback(&configs, "system", "user").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "success");
    }

    #[tokio::test(start_paused = true)]
    async fn test_complete_with_fallback_retries_on_retriable_error() {
        let client = LLMClient::new();
        let mut registry = ProviderRegistry::new();
        // Fail twice with "500" error, then succeed
        registry.register(Box::new(MockProvider::new("mock", 2, "500 Internal Server Error")));
        let client = client.with_registry(Arc::new(registry));

        let configs = vec![LLMConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            api_key: "test".to_string(),
            api_base: "https://api.mock.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];

        let result = client.complete_with_fallback(&configs, "system", "user").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "success");
    }

    #[tokio::test(start_paused = true)]
    async fn test_complete_with_fallback_exhausts_all_retries() {
        let client = LLMClient::new();
        let mut registry = ProviderRegistry::new();
        // Always fail with "500" error
        registry.register(Box::new(MockProvider::new("mock", 999, "500 Internal Server Error")));
        let client = client.with_registry(Arc::new(registry));

        let configs = vec![LLMConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            api_key: "test".to_string(),
            api_base: "https://api.mock.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];

        let result = client.complete_with_fallback(&configs, "system", "user").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("all LLM providers failed"));
    }

    #[tokio::test]
    async fn test_complete_with_fallback_fails_fast_on_non_retriable_error() {
        let client = LLMClient::new();
        let mut registry = ProviderRegistry::new();
        // Fail with a 400 error (not retriable)
        registry.register(Box::new(MockProvider::new("mock", 999, "400 Bad Request")));
        let client = client.with_registry(Arc::new(registry));

        let configs = vec![LLMConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            api_key: "test".to_string(),
            api_base: "https://api.mock.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];

        let result = client.complete_with_fallback(&configs, "system", "user").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("all LLM providers failed"));
    }

    #[tokio::test]
    async fn test_complete_with_fallback_fallback_to_next_provider() {
        let client = LLMClient::new();
        let mut registry = ProviderRegistry::new();
        // First provider always fails
        registry.register(Box::new(MockProvider::new("first", 999, "500")));
        // Second provider succeeds immediately
        registry.register(Box::new(MockProvider::new("second", 0, "unused")));
        let client = client.with_registry(Arc::new(registry));

        let configs = vec![
            LLMConfig {
                provider: "first".to_string(),
                model: "first-model".to_string(),
                api_key: "test".to_string(),
                api_base: "https://api.first.com/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.3,
            },
            LLMConfig {
                provider: "second".to_string(),
                model: "second-model".to_string(),
                api_key: "test".to_string(),
                api_base: "https://api.second.com/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.3,
            },
        ];

        let result = client.complete_with_fallback(&configs, "system", "user").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().content, "success");
    }
}
