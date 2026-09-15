//! RENG-57: the client records one latency sample per call ATTEMPT, including
//! the retries and the failed fallback attempts that never reach a caller.
//!
//! These tests drive the real `complete_with_fallback` against wiremock, so
//! what is asserted is the recording of actual HTTP attempts, not of a stub.

use super::*;
use crate::llm::sampling::{LlmCallSample, LlmCallSink};
use async_trait::async_trait;
use std::sync::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Collects every sample the client hands over, in order.
#[derive(Default)]
struct CaptureSink {
    samples: Mutex<Vec<LlmCallSample>>,
}

impl CaptureSink {
    fn samples(&self) -> Vec<LlmCallSample> {
        self.samples.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmCallSink for CaptureSink {
    async fn record(&self, sample: &LlmCallSample) {
        self.samples.lock().unwrap().push(sample.clone());
    }
}

fn config(provider: &str, api_base: &str) -> LLMConfig {
    LLMConfig {
        provider: provider.to_string(),
        model: format!("{provider}-model"),
        api_key: "sk-test".to_string(),
        api_base: api_base.to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
    }
}

/// A 200 with an OpenAI-shaped body.
async fn mount_success(mock: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"total_tokens": 7},
            "model": "served-model",
        })))
        .mount(mock)
        .await;
}

/// A successful call records exactly one sample, attributed to the config that
/// served it, with the chain position of a single-entry chain.
#[tokio::test]
async fn one_successful_call_records_one_sample() {
    let mock = MockServer::start().await;
    mount_success(&mock).await;
    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));

    let result = client
        .complete_with_fallback(&[config("xiaomi", &mock.uri())], "sys", "user")
        .await
        .expect("the mock answers 200");
    assert_eq!(result.provider, "xiaomi");

    let samples = sink.samples();
    assert_eq!(samples.len(), 1, "one attempt, one sample");
    assert_eq!(samples[0].provider, "xiaomi");
    assert_eq!(samples[0].model, "xiaomi-model");
    assert!(samples[0].success);
    assert_eq!(samples[0].error, None);
    assert_eq!(samples[0].chain_position, 1);
    assert_eq!(samples[0].attempt, 1);
    assert!(!samples[0].is_fallback());
}

/// A 401 is permanent (RENG-35): one attempt on the first config, then the
/// second config answers. BOTH attempts are recorded — the failure the chain
/// swallowed and the fallback hit — which is the data that did not exist
/// before RENG-57.
#[tokio::test]
async fn a_failed_attempt_and_the_fallback_hit_are_both_recorded() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .mount(&mock)
        .await;
    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));

    let result = client
        .complete_with_fallback(&[config("primary", &mock.uri())], "sys", "user")
        .await;
    assert!(result.is_err(), "the only config rejects the credential");

    let samples = sink.samples();
    assert_eq!(samples.len(), 1, "a permanent 4xx is one attempt");
    assert!(!samples[0].success);
    assert_eq!(samples[0].chain_position, 1);
    let error = samples[0].error.as_deref().expect("a failure carries why");
    assert!(
        error.contains("401"),
        "the sample names the failure, not just 'it failed': {error}"
    );
}

/// Every retry is its own sample, numbered: a retriable failure costs three
/// attempts on one config (the RENG-35 retry budget) and all three are rows.
///
/// The `attempt` numbers are what let the page (or a reader of the table)
/// tell "the provider is slow on the third try" from "the provider answered
/// once"; they also keep a retried-then-failed call from looking like a single
/// instantaneous rejection.
#[tokio::test]
async fn every_retry_is_recorded_under_its_attempt_number() {
    let mock = MockServer::start().await;
    // 500 is retriable; the client gives up after `max_retries` attempts.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;
    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));

    let result = client
        .complete_with_fallback(&[config("flaky", &mock.uri())], "sys", "user")
        .await;
    assert!(result.is_err());

    let samples = sink.samples();
    assert_eq!(samples.len(), 3, "the retry budget is three attempts, all recorded");
    let attempts: Vec<u32> = samples.iter().map(|s| s.attempt).collect();
    assert_eq!(attempts, vec![1, 2, 3]);
    assert!(samples.iter().all(|s| !s.success));
    assert!(samples.iter().all(|s| s.chain_position == 1));
    assert_eq!(
        mock.received_requests().await.unwrap().len(),
        3,
        "the recorded attempts are the requests actually sent"
    );
}

/// Two configs, the second answering: the walk is recorded with the chain
/// position each attempt happened at, so a fallback is visible in the data and
/// not only in the logs.
#[tokio::test]
async fn chain_positions_are_recorded_per_attempt() {
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
        .mount(&failing)
        .await;
    let healthy = MockServer::start().await;
    mount_success(&healthy).await;

    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));
    let result = client
        .complete_with_fallback(
            &[config("primary", &failing.uri()), config("secondary", &healthy.uri())],
            "sys",
            "user",
        )
        .await
        .expect("the secondary provider serves the call");
    assert_eq!(result.provider, "secondary");
    assert!(result.fallback);

    let samples = sink.samples();
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0].provider, "primary");
    assert_eq!(samples[0].chain_position, 1);
    assert!(!samples[0].success);
    assert!(!samples[0].is_fallback());
    assert_eq!(samples[1].provider, "secondary");
    assert_eq!(samples[1].chain_position, 2);
    assert!(samples[1].success);
    assert!(samples[1].is_fallback(), "a hit behind the head is a fallback");
}

/// A client with no sink (the CLI, every pre-RENG-57 caller) records nothing
/// and behaves exactly as before.
#[tokio::test]
async fn a_client_without_a_sink_still_works() {
    let mock = MockServer::start().await;
    mount_success(&mock).await;
    let client = LLMClient::new();
    let result = client
        .complete_with_fallback(&[config("xiaomi", &mock.uri())], "sys", "user")
        .await
        .expect("the call is unaffected by the absence of a sink");
    assert_eq!(result.provider, "xiaomi");
}
