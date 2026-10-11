use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
        disabled: false,
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
        disabled: false,
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
        disabled: false,
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
            disabled: false,
        };
        let result = client.complete_direct(&config, "sys", "user").await;
        assert!(result.0.is_err());
        let err = result.0.unwrap_err().to_string();
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
        disabled: false,
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
        disabled: false,
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
        disabled: false,
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
        disabled: false,
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
            disabled: false,
        },
        LLMConfig {
            provider: "anthropic".to_string(),
            model: "claude-3".to_string(),
            api_key: "test-key".to_string(),
            api_base: String::new(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
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
        disabled: false,
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
    call_count: Arc<AtomicUsize>,
    fail_until: usize,
    error_msg: String,
}

impl MockProvider {
    fn new(name: &str, fail_until: usize, error_msg: &str) -> Self {
        Self {
            name: name.to_string(),
            call_count: Arc::new(AtomicUsize::new(0)),
            fail_until,
            error_msg: error_msg.to_string(),
        }
    }

    /// Shared handle on the call counter. `Arc` so it outlives the provider
    /// being boxed into a registry, letting a test assert the exact number of
    /// attempts the retry loop made.
    fn calls(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.call_count)
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
                finish_reason: None,
                provider: self.name.clone(),
                fallback: false,
                entry_fp: None,
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
        disabled: false,
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
        disabled: false,
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
        disabled: false,
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
    // Fail with the message the provider layer actually writes on a 400.
    let provider = MockProvider::new(
        "mock",
        999,
        "OpenAI API returned 400 Bad Request: {\"error\":\"invalid model\"}",
    );
    let calls = provider.calls();
    registry.register(Box::new(provider));
    let client = client.with_registry(Arc::new(registry));

    let configs = vec![LLMConfig {
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        api_key: "test".to_string(),
        api_base: "https://api.mock.com/v1".to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }];

    let result = client.complete_with_fallback(&configs, "system", "user").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("all LLM providers failed"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a 400 must not be retried");
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
            disabled: false,
        },
        LLMConfig {
            provider: "second".to_string(),
            model: "second-model".to_string(),
            api_key: "test".to_string(),
            api_base: "https://api.second.com/v1".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        },
    ];
    let result = client.complete_with_fallback(&configs, "system", "user").await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().content, "success");
}

/// RENG-38: the completion carries the hitting config's provider — when the
/// fallback chain succeeds on the SECOND entry, the result is attributed to
/// that entry, not the primary. This is the attribution the history snapshot
/// (`expert_reports.llm_provider`) is built from.
#[tokio::test]
async fn test_fallback_result_is_attributed_to_the_hitting_provider() {
    let client = LLMClient::new();
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(MockProvider::new("first", 999, "500")));
    registry.register(Box::new(MockProvider::new("second", 0, "unused")));
    let client = client.with_registry(Arc::new(registry));

    let config = |provider: &str| LLMConfig {
        provider: provider.to_string(),
        model: format!("{provider}-model"),
        api_key: "test".to_string(),
        api_base: format!("https://api.{provider}.com/v1"),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    };

    // Fallback hit: second config wins.
    let result = client
        .complete_with_fallback(&[config("first"), config("second")], "system", "user")
        .await
        .unwrap();
    assert_eq!(
        result.provider, "second",
        "the hitting config's provider must be recorded"
    );

    // Direct hit: attributed to the (only) config.
    let result = client.complete(&config("second"), "system", "user").await.unwrap();
    assert_eq!(result.provider, "second");
    assert_eq!(
        result.model, "second-model",
        "RENG-77: the CONFIGURED model is attributed, not the provider's echo ('mock') — every consumer looks the value up by what the card says"
    );
}

/// RENG-55: a hit on a later chain entry is flagged (`fallback = true`) and
/// carries THAT entry's provider, so a review served by a secondary provider
/// is distinguishable from a normal primary run. The flag is set by the chain
/// walker only — `configs[0]` hits and direct `complete` calls are `false`.
#[tokio::test]
async fn test_fallback_result_is_flagged_and_uses_the_later_config() {
    let client = LLMClient::new();
    let mut registry = ProviderRegistry::new();
    // Non-retriable failures (400) so the chain advances immediately without
    // backoff sleeps.
    registry.register(Box::new(MockProvider::new(
        "primary",
        999,
        "OpenAI API returned 400 Bad Request: {\"error\":\"invalid model\"}",
    )));
    registry.register(Box::new(MockProvider::new("secondary", 0, "unused")));
    let client = client.with_registry(Arc::new(registry));

    let config = |provider: &str| LLMConfig {
        provider: provider.to_string(),
        model: format!("{provider}-model"),
        api_key: "test".to_string(),
        api_base: format!("https://api.{provider}.com/v1"),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    };

    // Chain = [primary, secondary]: the primary answers nothing, so the used
    // provider is the SECOND config and the completion is marked a fallback.
    let chain = [config("primary"), config("secondary")];
    let result = client.complete_with_fallback(&chain, "system", "user").await.unwrap();
    assert_eq!(
        result.provider, "secondary",
        "the used provider must be the later chain entry"
    );
    assert!(result.fallback, "a non-primary hit must be flagged");

    // The very same provider as the chain HEAD is not a fallback.
    let hit = client
        .complete_with_fallback(&[config("secondary")], "system", "user")
        .await
        .unwrap();
    assert_eq!(hit.provider, "secondary");
    assert!(!hit.fallback, "a chain-head hit is not a fallback");
}

// ─── RENG-35: the retry decision comes from the HTTP status ──────────
//
// Before RENG-35 the loop searched the error text for "429"/"500"/"timeout"/
// "connection", which is wrong in both directions: a 401 whose body mentions
// "connection" or a 500-looking number burned the whole attempt budget plus
// its backoff sleeps, while a real status that the text happened to spell
// differently slipped through. The classification is now `retry_verdict`.

/// The verdict table: 408 / 429 / 5xx are retryable, every other 4xx is
/// permanent, and an error that carries no status at all keeps the historical
/// retry.
#[test]
fn test_retry_verdict_by_http_status() {
    /// Build a provider-shaped error without format-string interpretation
    /// (response bodies carry literal braces).
    fn verdict(message: &str) -> RetryVerdict {
        LLMClient::retry_verdict(&anyhow::Error::msg(message.to_string()))
    }

    for status in [408u16, 429, 500, 502, 503, 504] {
        assert_eq!(
            verdict(&format!("OpenAI API returned {status} Whatever: {{}}")),
            RetryVerdict::Retryable { status },
            "HTTP {status} is transient and must be retried"
        );
    }

    for status in [400u16, 401, 403, 404, 422] {
        assert_eq!(
            verdict(&format!("Anthropic API returned {status} Rejected: {{}}")),
            RetryVerdict::Permanent { status },
            "HTTP {status} is the provider rejecting the request and must fail fast"
        );
    }

    // A transport error never reaches the status check: no response was read.
    assert_eq!(
        verdict(
            "Failed to send OpenAI request: error sending request for url \
             (http://127.0.0.1:1/v1/chat/completions): connection refused"
        ),
        RetryVerdict::Unknown,
        "a connection failure carries no status and stays retryable"
    );
    assert_eq!(
        verdict("Failed to parse OpenAI response: expected value at line 1 column 1"),
        RetryVerdict::Unknown,
        "an unparsable error must not be mistaken for a permanent verdict"
    );
}

/// The status is read through the `.context(...)` wrappers the provider layer
/// adds (and through `anyhow`'s `all LLM providers failed` tail).
#[test]
fn test_retry_verdict_reads_the_status_through_context_wrappers() {
    let inner = anyhow::Error::msg("Anthropic API returned 403 Forbidden: {\"error\":\"revoked\"}");
    let wrapped = inner
        .context("Failed to send Anthropic request")
        .context("all LLM providers failed");
    assert_eq!(
        LLMClient::retry_verdict(&wrapped),
        RetryVerdict::Permanent { status: 403 }
    );
}

/// The false-positive class RENG-35 exists for: a 401 whose *body* contains the
/// words the old classifier searched for. It must stay permanent — the status
/// comes from the first `returned <code>` marker, so body text cannot promote a
/// credential error into a retry.
#[test]
fn test_retry_verdict_is_not_fooled_by_status_like_text() {
    let err = anyhow::Error::msg(
        "LLM API returned 401 Unauthorized: {\"error\":{\"message\":\"Invalid API key; upstream \
         connection closed before message completed, please retry after 500 ms\"}}",
    );
    assert_eq!(LLMClient::retry_verdict(&err), RetryVerdict::Permanent { status: 401 });
}

/// Mount a `/chat/completions` POST answering `status` for every call, and
/// record how many requests the client is expected to make.
async fn mount_chat_completions(server: &MockServer, status: u16, body: &str, expected: u64) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body.to_string()))
        .expect(expected)
        .mount(server)
        .await;
}

/// A config pointing at a wiremock server. No provider registry is installed,
/// so the client exercises its real HTTP path (`complete_direct`) and the
/// status is classified from the error that path actually produces.
fn real_config(base: &str) -> LLMConfig {
    LLMConfig {
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        api_key: "sk-wrong-key".to_string(),
        api_base: base.to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }
}

/// A config for a registry-installed mock provider of the given name.
fn mock_config(provider: &str) -> LLMConfig {
    LLMConfig {
        provider: provider.to_string(),
        model: format!("{provider}-model"),
        api_key: "test".to_string(),
        api_base: format!("https://api.{provider}.com/v1"),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }
}

async fn requests_seen(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("request recording enabled")
        .len()
}

/// RENG-35, the reported symptom: a wrong `api_key` answers 401, and the call
/// must spend exactly one attempt on it — no second request, no backoff sleep —
/// while the status and the provider's reason survive to the caller.
#[tokio::test]
async fn test_real_http_401_fails_fast_with_one_attempt() {
    let server = MockServer::start().await;
    mount_chat_completions(
        &server,
        401,
        r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#,
        1,
    )
    .await;

    let client = LLMClient::new();
    let started = tokio::time::Instant::now();
    let err = client
        .complete_with_fallback(&[real_config(&server.uri())], "system", "user")
        .await
        .expect_err("a 401 must not be reported as success");
    let elapsed = started.elapsed();

    let message = format!("{err:#}");
    assert!(message.contains("401"), "the status must reach the caller: {message}");
    assert!(
        message.contains("Incorrect API key provided"),
        "the provider's reason must reach the caller: {message}"
    );
    assert_eq!(requests_seen(&server).await, 1, "a 401 must be attempted once");
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "no backoff sleep may run for a permanent error (the first one is 1000ms); took {elapsed:?}"
    );
    server.verify().await;
}

/// A revoked / insufficient-permission key answers 403: also permanent.
#[tokio::test]
async fn test_real_http_403_fails_fast_with_one_attempt() {
    let server = MockServer::start().await;
    mount_chat_completions(
        &server,
        403,
        r#"{"error":{"message":"The API key does not have access to model gpt-4"}}"#,
        1,
    )
    .await;

    let client = LLMClient::new();
    let started = tokio::time::Instant::now();
    let err = client
        .complete_with_fallback(&[real_config(&server.uri())], "system", "user")
        .await
        .expect_err("a 403 must not be reported as success");
    let elapsed = started.elapsed();

    assert!(format!("{err:#}").contains("403"));
    assert_eq!(requests_seen(&server).await, 1, "a 403 must be attempted once");
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "no backoff sleep for a 403"
    );
    server.verify().await;
}

/// Regression for the old classifier's false positive, driven through the real
/// client: this 401 body contains "connection" and a "500"-looking number, the
/// exact text that made the substring search retry a credential error.
#[tokio::test]
async fn test_real_http_401_with_connection_and_500_text_is_not_retried() {
    let server = MockServer::start().await;
    mount_chat_completions(
        &server,
        401,
        r#"{"error":{"message":"Invalid API key (connection closed before message completed after 500 ms)","type":"authentication_error"}}"#,
        1,
    )
    .await;

    let client = LLMClient::new();
    let started = tokio::time::Instant::now();
    let err = client
        .complete_with_fallback(&[real_config(&server.uri())], "system", "user")
        .await
        .expect_err("a 401 must not be reported as success");
    let elapsed = started.elapsed();

    let message = format!("{err:#}");
    assert!(
        message.contains("connection"),
        "the body text still reaches the caller: {message}"
    );
    assert_eq!(
        requests_seen(&server).await,
        1,
        "body text must not turn a 401 into a retry"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "body text must not cause a retry delay"
    );
    server.verify().await;
}

/// 429 is the rate limit: the one 4xx that is worth waiting out, so it spends
/// the whole budget (3 attempts, backoff 1s + 2s).
#[tokio::test]
async fn test_real_http_429_retries_up_to_the_attempt_budget() {
    let server = MockServer::start().await;
    mount_chat_completions(
        &server,
        429,
        r#"{"error":{"message":"Rate limit reached for gpt-4"}}"#,
        3,
    )
    .await;

    let client = LLMClient::new();
    let started = tokio::time::Instant::now();
    let err = client
        .complete_with_fallback(&[real_config(&server.uri())], "system", "user")
        .await
        .expect_err("a rate-limited call with no other config must fail");
    let elapsed = started.elapsed();

    assert!(format!("{err:#}").contains("429"));
    assert_eq!(requests_seen(&server).await, 3, "429 must use the attempt budget");
    assert!(
        elapsed >= std::time::Duration::from_secs(3),
        "the retries must be spaced by the backoff (1s + 2s); took {elapsed:?}"
    );
    server.verify().await;
}

/// 5xx is server-side, so it also spends the whole attempt budget. 503 is
/// exercised over real HTTP; 500 — the same `(500..600)` branch — is pinned
/// here so both are covered without paying the backoff wait twice.
#[tokio::test(start_paused = true)]
async fn test_500_retries_up_to_the_attempt_budget() {
    let mut registry = ProviderRegistry::new();
    let provider = MockProvider::new(
        "mock",
        999,
        "OpenAI API returned 500 Internal Server Error: {\"error\":\"internal error\"}",
    );
    let calls = provider.calls();
    registry.register(Box::new(provider));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let err = client
        .complete_with_fallback(&[mock_config("mock")], "system", "user")
        .await
        .expect_err("a failing server with no other config must fail");

    assert!(format!("{err:#}").contains("500"));
    assert_eq!(calls.load(Ordering::SeqCst), 3, "500 must use the attempt budget");
}

#[tokio::test]
async fn test_real_http_503_retries_up_to_the_attempt_budget() {
    let server = MockServer::start().await;
    mount_chat_completions(
        &server,
        503,
        r#"{"error":{"message":"upstream temporarily unavailable"}}"#,
        3,
    )
    .await;

    let client = LLMClient::new();
    let err = client
        .complete_with_fallback(&[real_config(&server.uri())], "system", "user")
        .await
        .expect_err("a failing server with no other config must fail");

    assert!(format!("{err:#}").contains("503"));
    assert_eq!(requests_seen(&server).await, 3, "503 must use the attempt budget");
    server.verify().await;
}

/// M42 / RENG-46 — the MR68 live failure: the upstream returns HTTP 200
/// with a body that is not JSON (a chunked timeout page, an HTML
/// proxy error, a truncated response). The old code surfaced
/// `Failed to parse LLM response: error decoding response body` with
/// nothing in the chain. The new path:
/// 1. captures the raw body excerpt so operators can see what came back,
/// 2. attaches a typed [`JsonDecodeError`] marker the compliance
///    sub-step can downcast to,
/// 3. treats the call as Unknown (retryable) — the next attempt may
///    succeed if the proxy hiccup was a one-off.
///
/// Note: this test does NOT use `start_paused` — reqwest's connection
/// pool needs real time to establish TCP connections to wiremock, and
/// the backoff sleeps are short (1s + 2s) so the test still finishes
/// quickly.
#[tokio::test]
async fn test_real_http_200_with_non_json_body_is_retried_with_body_in_error_chain() {
    let server = MockServer::start().await;
    let body = "<html><body>upstream timeout — gateway has the details</body></html>";
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_string()))
        .expect(3)
        .mount(&server)
        .await;

    let client = LLMClient::new();
    let err = client
        .complete_with_fallback(&[real_config(&server.uri())], "system", "user")
        .await
        .expect_err("a non-JSON body with no other config must fail");

    let message = format!("{err:#}");
    assert!(
        message.contains("error decoding response body"),
        "the original error message must surface: {message}"
    );
    // Typed marker is reachable via chain — the compliance sub-step
    // downcasts to it to render a categorised Note.
    let marker = err.chain().find_map(|c| c.downcast_ref::<JsonDecodeError>());
    assert!(marker.is_some(), "JsonDecodeError marker must be in the chain");
    let marker = marker.unwrap();
    assert_eq!(marker.status, 200);
    // The body excerpt is intentionally empty: reqwest's `resp.json()`
    // drains the body before we can read it as bytes. This is a known
    // limitation of the wire path; the typed marker + status code
    // alone are enough for operators to triage.
    assert!(
        marker.status == 200,
        "marker carries the HTTP status: {}",
        marker.status
    );
    assert_eq!(
        requests_seen(&server).await,
        3,
        "JSON-decode must use the attempt budget"
    );
}

/// M42 / RENG-46 — single-config `complete` path: same JSON-decode
/// behaviour, with the typed marker attached. The body excerpt is
/// intentionally empty on this path (resp.json() drains first).
#[tokio::test]
async fn test_complete_with_non_json_body_attaches_typed_marker() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json".to_string()))
        .expect(1)
        .mount(&server)
        .await;

    let client = LLMClient::new();
    let err = client
        .complete(&real_config(&server.uri()), "system", "user")
        .await
        .expect_err("non-JSON body must fail");

    let message = format!("{err:#}");
    assert!(
        message.contains("error decoding response body"),
        "the original error must surface: {message}"
    );
    assert!(
        err.chain().any(|c| c.downcast_ref::<JsonDecodeError>().is_some()),
        "JsonDecodeError marker must be attached"
    );
}

/// M42 / RENG-46 — typed-marker happy path: a JsonDecodeError wrapped
/// via `Error::new(...)` and `.context(...)` is classified as Unknown
/// (retryable), not Permanent (give-up).
#[test]
fn json_decode_error_classifies_as_unknown() {
    let raw = anyhow::Error::new(JsonDecodeError { status: 200 });
    assert_eq!(LLMClient::retry_verdict(&raw), RetryVerdict::Unknown);
}

/// M42 — the typed marker survives a `.context(...)` wrapper so the
/// compliance sub-step can downcast through the chain.
#[test]
fn json_decode_error_downcast_works_through_context_chain() {
    let raw = anyhow::Error::new(JsonDecodeError { status: 200 })
        .context(anyhow::anyhow!("inner anyhow message"));
    let marker = raw.chain().find_map(|c| c.downcast_ref::<JsonDecodeError>());
    assert!(
        marker.is_some(),
        "JsonDecodeError must be reachable through any chain depth"
    );
}

/// M42 — defence-in-depth: redact_body_excerpt must not panic when the
/// 512-byte truncation cap lands inside a multi-byte UTF-8 character.
/// The MR66 live run triggered this: a non-JSON HTML body whose byte
/// 512 was inside a 3-byte CJK sequence caused the binary to panic
/// before producing any output. The fix snaps the cut to the nearest
/// char boundary.
///
/// (Removed in M42-R2 — the body-excerpt capture was reverted to the
/// pre-M42 wire path because `resp.bytes().await` failed on its own
/// in a live run, taking down every LLM call. The redaction helper is
/// gone with it. The truncation-safety assertion lives on the wire
/// side now: a 4 KiB CJK body that round-trips through reqwest
/// without panic is exactly what `resp.json()` already does. If a
/// future revision re-introduces explicit body capture, the multi-
/// byte truncation rule should be re-tested.)
#[allow(dead_code)]
const REDACT_BODY_EXCERTS_NOTE: &str = "see commit message for the truncation-safety rationale";

/// Nothing is listening on port 1, so the request fails without any HTTP
/// response (`reqwest` connection refused). It carries no status, so it keeps
/// the historical retry: two backoffs (1s + 2s) run before giving up.
#[tokio::test(start_paused = true)]
async fn test_real_transport_failure_without_status_still_retries() {
    let client = LLMClient::new();
    let started = tokio::time::Instant::now();
    let err = client
        .complete_with_fallback(&[real_config("http://127.0.0.1:1/v1")], "system", "user")
        .await
        .expect_err("an unreachable endpoint must fail");
    let elapsed = started.elapsed();

    let message = format!("{err:#}");
    assert!(
        message.contains("all LLM providers failed"),
        "the transport failure must be reported: {message}"
    );
    assert!(
        elapsed >= std::time::Duration::from_secs(3),
        "a status-less failure must still retry 3 times (1s + 2s); took {elapsed:?}"
    );
}

/// A permanent verdict is a verdict about THAT config, so the chain still
/// advances to the next entry (a different provider with its own credentials)
/// and the hit is flagged as a fallback (RENG-55). The log lines that make the
/// advance visible are asserted end-to-end in `tests/llm/main.rs`, whose own
/// process can capture `tracing` output deterministically.
#[tokio::test(start_paused = true)]
async fn test_permanent_401_advances_the_chain_without_retrying() {
    let mut registry = ProviderRegistry::new();
    let primary = MockProvider::new(
        "primary",
        999,
        "OpenAI API returned 401 Unauthorized: {\"error\":\"Incorrect API key provided\"}",
    );
    let primary_calls = primary.calls();
    registry.register(Box::new(primary));
    registry.register(Box::new(MockProvider::new("secondary", 0, "unused")));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let result = client
        .complete_with_fallback(&[mock_config("primary"), mock_config("secondary")], "system", "user")
        .await
        .expect("the secondary provider must still serve the call");

    assert_eq!(result.provider, "secondary", "the chain must advance");
    assert!(result.fallback, "a later-chain hit must be flagged");
    assert_eq!(
        primary_calls.load(Ordering::SeqCst),
        1,
        "the credential error must be attempted exactly once before the chain advances"
    );
}

/// RENG-66 r2 (F-1) — the acceptance criterion of this round: a card the probe
/// calls `healthy` must be a card whose completions actually run.
///
/// The builtin catalog prefills Anthropic's base WITH the version
/// (`https://api.anthropic.com/v1`, `catalog::normalize_api_base` passes it
/// through), and that is exactly where the two ends drifted: the probe
/// normalized it to `/v1/models` and read healthy, while `AnthropicProvider`
/// built `/v1/v1/messages` and 404'd on every completion. The mock answers 200
/// on `/v1/messages` and on nothing else, so a doubled version cannot pass (a
/// 404 is a permanent verdict, so no retry can rescue it). The card is exercised
/// the way a user would: probe it, then complete through the registry its config
/// resolves to.
#[tokio::test]
async fn a_probe_healthy_anthropic_card_with_a_versioned_base_completes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "reviewed"}],
            "usage": {"input_tokens": 7, "output_tokens": 3},
            "model": "claude-3-5-sonnet"
        })))
        .mount(&server)
        .await;

    let config = LLMConfig {
        provider: "anthropic".to_string(),
        model: "claude-3-5-sonnet".to_string(),
        api_key: "sk-ant-good".to_string(),
        // The catalog prefill, and what a user copying Anthropic's docs types.
        api_base: format!("{}/v1", server.uri()),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    };

    crate::llm::probe::probe_llm_connectivity(&config)
        .await
        .expect("the probe must read this card healthy");

    let configs = vec![config];
    let (registry, _order) = ProviderRegistry::from_configs(&configs);
    let client = LLMClient::new().with_registry(Arc::new(registry));
    let result = client
        .complete_with_fallback(&configs, "system", "user")
        .await
        .expect("a card the probe calls healthy must complete");
    assert_eq!(result.content, "reviewed");
    assert_eq!(result.provider, "anthropic");

    let requests = server.received_requests().await.expect("request recording enabled");
    assert_eq!(requests.len(), 2, "one probe GET, one completion POST");
    assert!(
        requests
            .iter()
            .all(|r| r.url.path() == "/v1/models" || r.url.path() == "/v1/messages"),
        "no request may carry a doubled version: {:?}",
        requests.iter().map(|r| r.url.path()).collect::<Vec<_>>()
    );
    let completion = requests
        .iter()
        .find(|r| r.url.path() == "/v1/messages")
        .expect("the completion must go to /v1/messages");
    assert_eq!(
        completion.headers.get("x-api-key").map(|v| v.to_str().unwrap()),
        Some("sk-ant-good"),
        "the completion authenticates the way the probe does"
    );
    assert!(
        completion.headers.get("authorization").is_none(),
        "Anthropic must not receive a bearer header"
    );
}

// ─── M26: empty-completion auto-retry with disable_thinking=true ───
//
// A reasoning model that burns its `max_tokens` budget on
// `reasoning_tokens` (e.g. `mimo-v2.5-pro`, `deepseek-v4-flash`) returns
// HTTP 200 with an empty `choices[0].message.content`. The M24 recall run
// hit this 37 times in 18 instances. The M26 mitigation: before
// advancing the chain, re-send the SAME request once with
// `disable_thinking = true`. The retry must:
//
// 1. Recover the answer when the model behaves with `thinking: disabled`.
// 2. Skip when the config already has `disable_thinking = true` (no
//    useful second attempt — same body would repeat the empty response).
// 3. Bounded to ONE attempt per provider (no chain of retries on empty).
// 4. Not run when the response is non-empty (no extra round trip on the
//    happy path).

/// Scripted provider: each call returns the body at the matching index
/// (capped at the last body). Tracks every `disable_thinking` flag so
/// the test can prove the second call set it to `Some(true)` while the
/// first left it as `None`.
struct ScriptedToggleProvider {
    bodies: Vec<String>,
    call_count: AtomicUsize,
    disable_thinking_seen: Arc<std::sync::Mutex<Vec<Option<bool>>>>,
}

#[async_trait]
impl super::super::provider::LLMProvider for ScriptedToggleProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn complete(
        &self,
        params: &super::super::provider::CompletionParams,
    ) -> Result<super::super::provider::CompletionResult, anyhow::Error> {
        let i = self.call_count.fetch_add(1, Ordering::SeqCst);
        self.disable_thinking_seen.lock().unwrap().push(params.disable_thinking);
        let body = &self.bodies[i.min(self.bodies.len() - 1)];
        Ok(super::super::provider::CompletionResult {
            content: body.clone(),
            total_tokens: 1,
            model: "mock".to_string(),
            finish_reason: None,
            provider: "scripted".to_string(),
            fallback: false,
            entry_fp: None,
        })
    }
}

#[tokio::test]
async fn empty_completion_triggers_a_disable_thinking_retry_that_recovers() {
    // Body 0: empty string (the reasoning-model failure shape).
    // Body 1: a real, schema-conforming payload that the disable_thinking
    // retry will recover. If the client surfaces it, the call succeeds.
    let bodies = vec![String::new(), "score: 88\nsummary: \"ok\"\nfindings: []\n".to_string()];
    let disable_thinking_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = ScriptedToggleProvider {
        bodies: bodies.clone(),
        call_count: AtomicUsize::new(0),
        disable_thinking_seen: disable_thinking_seen.clone(),
    };

    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(provider));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let configs = vec![LLMConfig {
        provider: "scripted".to_string(),
        model: "mock-reasoning".to_string(),
        api_key: "k".to_string(),
        api_base: "https://api.example".to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None, // <- the M26 trigger condition
        disabled: false,
    }];

    let result = client
        .complete_with_fallback(&configs, "system", "user")
        .await
        .expect("the disable_thinking retry must convert the empty completion into a success");
    assert_eq!(result.content, bodies[1]);

    let seen = disable_thinking_seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "exactly two attempts: the empty call + the disable_thinking retry"
    );
    assert_eq!(seen[0], None, "the original call did not opt in to disable_thinking");
    assert_eq!(seen[1], Some(true), "the M26 retry must carry disable_thinking=true");
}

#[tokio::test]
async fn empty_completion_with_disable_thinking_already_set_does_not_retry() {
    // Body 0: empty. The retry would repeat the same empty — the config
    // already opts in, so the M26 path skips the retry and the chain
    // surfaces the error.
    let bodies = vec![String::new()];
    let disable_thinking_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = ScriptedToggleProvider {
        bodies,
        call_count: AtomicUsize::new(0),
        disable_thinking_seen: disable_thinking_seen.clone(),
    };
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(provider));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let configs = vec![LLMConfig {
        provider: "scripted".to_string(),
        model: "mock-reasoning".to_string(),
        api_key: "k".to_string(),
        api_base: "https://api.example".to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: Some(true), // <- already opted in: no M26 retry
        disabled: false,
    }];

    let err = client
        .complete_with_fallback(&configs, "system", "user")
        .await
        .expect_err("a config that already has disable_thinking set does not get a second retry");
    let message = format!("{err:#}");
    assert!(
        message.contains("empty completion"),
        "the diagnosis must reach the caller: {message}"
    );

    let seen = disable_thinking_seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        1,
        "the M26 retry must skip a config that already opts in to disable_thinking; saw {seen:?}"
    );
    assert_eq!(
        seen[0],
        Some(true),
        "the original call still had disable_thinking=true; no second attempt ran"
    );
}

#[tokio::test]
async fn empty_completion_retry_is_bounded_to_one_attempt_when_still_empty() {
    // Body 0: empty (the trigger).
    // Body 1: empty (the disable_thinking retry — a reasoning model that
    // ignores `thinking: disabled` will still answer empty).
    // Body 2: irrelevant; the client must NOT call it.
    let bodies = vec![
        String::new(),
        String::new(),
        "score: 80\nsummary: \"x\"\nfindings: []\n".to_string(),
    ];
    let disable_thinking_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = ScriptedToggleProvider {
        bodies,
        call_count: AtomicUsize::new(0),
        disable_thinking_seen: disable_thinking_seen.clone(),
    };
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(provider));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let configs = vec![LLMConfig {
        provider: "scripted".to_string(),
        model: "mock-reasoning".to_string(),
        api_key: "k".to_string(),
        api_base: "https://api.example".to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }];

    let err = client
        .complete_with_fallback(&configs, "system", "user")
        .await
        .expect_err("a single-entry chain must surface the empty twice (original + retry)");
    let message = format!("{err:#}");
    assert!(
        message.contains("empty completion"),
        "the diagnosis must reach the caller: {message}"
    );

    let seen = disable_thinking_seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "exactly two attempts: the original + one disable_thinking retry; saw {seen:?}"
    );
}

#[tokio::test]
async fn empty_completion_retry_only_fires_on_empty_verdicts_not_on_5xx() {
    // Body 0: 500-class error (Retryable, not Empty). The M26 retry is
    // empty-specific — a 5xx must continue through the normal
    // backoff retry path, NOT trigger an extra disable_thinking call.
    let provider = MockProvider::new("mock", 999, "OpenAI API returned 500 Internal Server Error: {}");
    let calls = provider.calls();
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(provider));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let err = client
        .complete_with_fallback(&[mock_config("mock")], "system", "user")
        .await
        .expect_err("a 500 with no other config must fail");
    let message = format!("{err:#}");
    assert!(
        message.contains("500"),
        "the 500 status must reach the caller: {message}"
    );

    // The 500 path uses the normal backoff retry budget (3 attempts),
    // NOT the M26 disable_thinking retry — the M26 retry is empty-only.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "500 must use the standard attempt budget, not the empty-specific M26 retry"
    );
}

#[tokio::test]
async fn empty_completion_retry_advances_to_next_provider_when_still_empty() {
    // Two providers: both answer empty. The M26 retry runs once on each
    // before the chain gives up. Two providers × two attempts each = 4
    // recorded attempts (the script enforces this ordering).
    let bodies = vec![
        String::new(), // primary: empty
        String::new(), // primary disable_thinking retry: still empty
        String::new(), // fallback: empty
        String::new(), // fallback disable_thinking retry: still empty
    ];
    let disable_thinking_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = ScriptedToggleProvider {
        bodies,
        call_count: AtomicUsize::new(0),
        disable_thinking_seen: disable_thinking_seen.clone(),
    };
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(provider));
    let client = LLMClient::new().with_registry(Arc::new(registry));

    let configs = vec![
        LLMConfig {
            provider: "scripted".to_string(),
            model: "primary".to_string(),
            api_key: "k".to_string(),
            api_base: "https://api.example".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        },
        LLMConfig {
            provider: "scripted".to_string(),
            model: "fallback".to_string(),
            api_key: "k".to_string(),
            api_base: "https://api.example".to_string(),
            max_tokens: 4096,
            temperature: 0.3,
            disable_thinking: None,
            disabled: false,
        },
    ];

    let err = client
        .complete_with_fallback(&configs, "system", "user")
        .await
        .expect_err("both providers empty across both retries must fail");
    let message = format!("{err:#}");
    assert!(message.contains("empty completion"));

    let seen = disable_thinking_seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        4,
        "each provider gets its M26 disable_thinking retry: 2 providers × 2 attempts; saw {seen:?}"
    );
    // Sequence: [None, Some(true), None, Some(true)] — primary + retry, then fallback + retry.
    assert_eq!(seen[0], None);
    assert_eq!(seen[1], Some(true));
    assert_eq!(seen[2], None);
    assert_eq!(seen[3], Some(true));
}
