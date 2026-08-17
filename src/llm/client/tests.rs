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
fn test_build_chat_request_body_omits_thinking_by_default() {
    let config = LLMConfig {
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        api_key: "sk-test".to_string(),
        api_base: String::new(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
    };
    let body = LLMClient::build_chat_request_body(&config, "sys", "user");
    assert!(
        body.get("thinking").is_none(),
        "thinking must be omitted unless opted in"
    );
    assert_eq!(body["model"], "gpt-4");
    assert_eq!(body["max_tokens"], 4096);
}

#[test]
fn test_build_chat_request_body_includes_thinking_when_disabled() {
    let config = LLMConfig {
        provider: "openai".to_string(),
        model: "deepseek-v4-flash".to_string(),
        api_key: "sk-test".to_string(),
        api_base: String::new(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: Some(true),
    };
    let body = LLMClient::build_chat_request_body(&config, "sys", "user");
    assert_eq!(body["thinking"], serde_json::json!({"type": "disabled"}));
}

#[test]
fn test_build_chat_request_body_thinking_false_is_omitted() {
    // Explicitly false must behave like unset: no unknown field sent.
    let config = LLMConfig {
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        api_key: "sk-test".to_string(),
        api_base: String::new(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: Some(false),
    };
    let body = LLMClient::build_chat_request_body(&config, "sys", "user");
    assert!(body.get("thinking").is_none());
}

#[test]
fn test_llm_config_deserializes_without_disable_thinking() {
    // Legacy LLM_CONFIG JSON (or TOML) without the new field must still parse.
    let config: LLMConfig = serde_json::from_str(
        r#"{"provider":"openai","model":"gpt-4","api_key":"k","api_base":"https://api.openai.com/v1","max_tokens":4096,"temperature":0.3}"#,
    )
    .unwrap();
    assert_eq!(config.disable_thinking, None);
    // And the absent field must not be re-serialized (clean contract).
    let json = serde_json::to_value(&config).unwrap();
    assert!(json.get("disable_thinking").is_none());
}

#[test]
fn test_llm_config_deserializes_disable_thinking_true() {
    let config: LLMConfig = serde_json::from_str(
        r#"{"provider":"openai","model":"deepseek-v4-flash","api_key":"k","api_base":"https://api.deepseek.com","max_tokens":4096,"temperature":0.3,"disable_thinking":true}"#,
    )
    .unwrap();
    assert_eq!(config.disable_thinking, Some(true));
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
            disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
            disable_thinking: None,
        },
        LLMConfig {
            provider: "anthropic".to_string(),
            model: "claude-3".to_string(),
            api_key: "test-key".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
        disable_thinking: None,
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
            disable_thinking: None,
        },
        LLMConfig {
            provider: "second".to_string(),
            model: "second-model".to_string(),
            api_key: "test".to_string(),
            api_base: "https://api.second.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
        },
    ];

    let result = client.complete_with_fallback(&configs, "system", "user").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().content, "success");
}
