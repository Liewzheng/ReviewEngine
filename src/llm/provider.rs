use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

/// Unified message role for LLM chat completion requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// The unified response from an LLM completion.
#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub content: String,
    pub total_tokens: u64,
    pub model: String,
}

/// Parameters for LLM completion requests.
#[derive(Debug, Clone)]
pub struct CompletionParams {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub temperature: f32,
    pub reasoning_effort: Option<String>,
}

/// Abstract interface for LLM providers.
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// The provider name (e.g., "openai", "anthropic").
    fn name(&self) -> &str;

    /// Complete a chat completion request.
    async fn complete(&self, params: &CompletionParams) -> Result<CompletionResult>;
}

// ─── OpenAI Provider ───────────────────────────

/// Provider implementation for the OpenAI chat completion API.
///
/// Supports GPT-4, GPT-4o, o1, o3 series, and any OpenAI-compatible
/// endpoint. Handles authentication via Bearer token and supports
/// the `reasoning_effort` parameter for o1/o3 models.
pub struct OpenAIProvider {
    /// OpenAI API key (sk-...).
    pub api_key: String,
    /// Base URL for the API (e.g. `https://api.openai.com/v1`).
    pub api_base: String,
    client: reqwest::Client,
}

impl OpenAIProvider {
    /// Create a new `OpenAIProvider` with the given credentials and base URL.
    #[allow(clippy::expect_used)]
    pub fn new(api_key: String, api_base: String) -> Self {
        Self {
            api_key,
            api_base,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }
}

#[async_trait]
impl LLMProvider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn complete(&self, params: &CompletionParams) -> Result<CompletionResult> {
        let url = format!("{}/chat/completions", self.api_base.trim_end_matches('/'));

        let mut body = json!({
            "model": params.model,
            "messages": params.messages.iter().map(|m| {
                json!({"role": m.role, "content": m.content})
            }).collect::<Vec<_>>(),
            "max_tokens": params.max_tokens,
            "temperature": params.temperature,
        });

        // Add reasoning_effort if set (o1/o3 models)
        if let Some(ref effort) = params.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send OpenAI request")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API returned {status}: {text}");
        }

        let value: serde_json::Value = resp.json().await.context("Failed to parse OpenAI response")?;

        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .context("OpenAI response missing choices[0].message.content")?
            .to_string();

        let total_tokens = value["usage"]["total_tokens"].as_u64().unwrap_or(0);
        let model = value["model"].as_str().unwrap_or(&params.model).to_string();

        Ok(CompletionResult {
            content,
            total_tokens,
            model,
        })
    }
}

// ─── OpenAI-Compatible Provider ────────────────

/// Provider for any OpenAI-compatible API endpoint (e.g. Ollama, vLLM, Azure).
///
/// Wraps [`OpenAIProvider`] internally and uses the given `custom_name`
/// for identification in the registry.
pub struct OpenAICompatibleProvider {
    inner: OpenAIProvider,
    custom_name: String,
}

impl OpenAICompatibleProvider {
    /// Create a new provider wrapping an OpenAI-compatible endpoint.
    pub fn new(api_key: String, api_base: String, name: String) -> Self {
        Self {
            inner: OpenAIProvider::new(api_key, api_base),
            custom_name: name,
        }
    }
}

#[async_trait]
impl LLMProvider for OpenAICompatibleProvider {
    fn name(&self) -> &str {
        &self.custom_name
    }

    async fn complete(&self, params: &CompletionParams) -> Result<CompletionResult> {
        self.inner.complete(params).await
    }
}

// ─── Anthropic Provider ────────────────────────

/// Provider implementation for the Anthropic (Claude) API.
///
/// Uses the `/v1/messages` endpoint and the `x-api-key` header for
/// authentication. Supports Claude 3 and later model families.
pub struct AnthropicProvider {
    /// Anthropic API key.
    pub api_key: String,
    /// Base URL for the API (e.g. `https://api.anthropic.com`).
    pub api_base: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Create a new `AnthropicProvider` with the given credentials and base URL.
    #[allow(clippy::expect_used)]
    pub fn new(api_key: String, api_base: String) -> Self {
        Self {
            api_key,
            api_base,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }
}

#[async_trait]
impl LLMProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, params: &CompletionParams) -> Result<CompletionResult> {
        let url = format!("{}/v1/messages", self.api_base.trim_end_matches('/'));

        // Convert unified messages to Anthropic format
        let mut system_content = String::new();
        let mut anthropic_messages: Vec<serde_json::Value> = Vec::new();

        for msg in &params.messages {
            match msg.role.as_str() {
                "system" => {
                    if system_content.is_empty() {
                        system_content = msg.content.clone();
                    } else {
                        system_content.push_str("\n");
                        system_content.push_str(&msg.content);
                    }
                }
                role => {
                    anthropic_messages.push(json!({
                        "role": role,
                        "content": msg.content,
                    }));
                }
            }
        }

        // Anthropic requires at least one user message
        if anthropic_messages.is_empty() {
            anyhow::bail!("Anthropic requires at least one user message");
        }

        let mut body = json!({
            "model": params.model,
            "messages": anthropic_messages,
            "max_tokens": params.max_tokens,
            "temperature": params.temperature,
        });

        // Claude requires non-empty system prompt; if empty, omit the field
        if !system_content.is_empty() {
            body["system"] = json!(system_content);
        }

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send Anthropic request")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API returned {status}: {text}");
        }

        let value: serde_json::Value = resp.json().await.context("Failed to parse Anthropic response")?;

        let content = value["content"][0]["text"]
            .as_str()
            .context("Anthropic response missing content[0].text")?
            .to_string();

        let total_tokens = value["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + value["usage"]["output_tokens"].as_u64().unwrap_or(0);
        let model = value["model"].as_str().unwrap_or(&params.model).to_string();

        Ok(CompletionResult {
            content,
            total_tokens,
            model,
        })
    }
}

// ─── Provider Registry ─────────────────────────

/// Registry of available LLM provider implementations.
///
/// Providers are registered by name and can be looked up at runtime
/// by [`LLMClient`](super::client::LLMClient) for multi-provider routing.
/// Built-in support includes OpenAI, Anthropic, and OpenAI-compatible endpoints.
pub struct ProviderRegistry {
    providers: HashMap<String, Box<dyn LLMProvider>>,
}

impl ProviderRegistry {
    /// Create an empty provider registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register a provider implementation by its name.
    pub fn register(&mut self, provider: Box<dyn LLMProvider>) {
        self.providers.insert(provider.name().to_string(), provider);
    }

    /// Look up a provider by name.
    pub fn get(&self, name: &str) -> Option<&dyn LLMProvider> {
        self.providers.get(name).map(|p| p.as_ref())
    }

    /// Return the list of registered provider names.
    pub fn names(&self) -> Vec<&str> {
        self.providers.keys().map(|k| k.as_str()).collect()
    }

    /// Build providers from LLM configs.
    /// Returns a ProviderRegistry and the first available name for fallback.
    pub fn from_configs(configs: &[crate::models::LLMConfig]) -> (Self, Vec<String>) {
        let mut registry = Self::new();
        let mut order = Vec::new();

        for config in configs {
            let provider: Box<dyn LLMProvider> = match config.provider.as_str() {
                "anthropic" => Box::new(AnthropicProvider::new(
                    config.api_key.clone(),
                    if config.api_base.is_empty() {
                        "https://api.anthropic.com".to_string()
                    } else {
                        config.api_base.clone()
                    },
                )),
                "openai" => Box::new(OpenAIProvider::new(
                    config.api_key.clone(),
                    if config.api_base.is_empty() {
                        "https://api.openai.com/v1".to_string()
                    } else {
                        config.api_base.clone()
                    },
                )),
                _ => {
                    // Treat as OpenAI-compatible
                    let name = if config.provider.is_empty() {
                        "openai-compatible"
                    } else {
                        &config.provider
                    };
                    Box::new(OpenAICompatibleProvider::new(
                        config.api_key.clone(),
                        config.api_base.clone(),
                        name.to_string(),
                    ))
                }
            };
            let name = provider.name().to_string();
            registry.register(provider);
            order.push(name);
        }

        (registry, order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LLMConfig;

    #[test]
    fn test_provider_registry_from_configs_empty() {
        let (registry, order) = ProviderRegistry::from_configs(&[]);
        assert!(registry.names().is_empty());
        assert!(order.is_empty());
    }

    #[test]
    fn test_provider_registry_from_configs_openai() {
        let configs = vec![LLMConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            api_key: "sk-test".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert!(registry.get("openai").is_some());
        assert_eq!(order, vec!["openai"]);
    }

    #[test]
    fn test_provider_registry_from_configs_anthropic() {
        let configs = vec![LLMConfig {
            provider: "anthropic".to_string(),
            model: "claude-3".to_string(),
            api_key: "test-key".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        assert!(registry.get("anthropic").is_some());
        assert_eq!(order, vec!["anthropic"]);
    }

    #[test]
    fn test_provider_registry_from_configs_unknown_fallback() {
        let configs = vec![LLMConfig {
            provider: "unknown_provider".to_string(),
            model: "test-model".to_string(),
            api_key: "test-key".to_string(),
            api_base: "https://custom.example.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
        }];
        let (registry, order) = ProviderRegistry::from_configs(&configs);
        // Unknown providers fall back to OpenAI-compatible
        assert!(registry.get("unknown_provider").is_some());
        assert_eq!(order, vec!["unknown_provider"]);
    }

    #[test]
    fn test_provider_registry_get_missing() {
        let registry = ProviderRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_provider_registry_names() {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(OpenAIProvider::new(
            "key".to_string(),
            "https://api.openai.com/v1".to_string(),
        )));
        let names = registry.names();
        assert_eq!(names.len(), 1);
        assert_eq!(names[0], "openai");
    }

    #[test]
    fn test_provider_registry_from_configs_multiple() {
        let configs = vec![
            LLMConfig {
                provider: "openai".to_string(),
                model: "gpt-4".to_string(),
                api_key: "sk-test".to_string(),
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
        assert!(registry.get("openai").is_some());
        assert!(registry.get("anthropic").is_some());
        assert_eq!(order, vec!["openai", "anthropic"]);
    }
}
