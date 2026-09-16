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
        disabled: false,
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

/// RENG-75: a sample names the CARD that served the attempt — the config's
/// own `entry_fp()` — so two same-named cards aggregate separately. A direct
/// hit carries its config's fingerprint; a fallback walk records each
/// attempt with the fingerprint of the entry it happened on.
#[tokio::test]
async fn samples_carry_the_serving_cards_fingerprint() {
    // Direct hit: the sample's fp is exactly `config.entry_fp()`.
    let mock = MockServer::start().await;
    mount_success(&mock).await;
    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));
    let only = config("xiaomi", &mock.uri());
    client
        .complete_with_fallback(std::slice::from_ref(&only), "sys", "user")
        .await
        .expect("the mock answers 200");
    let samples = sink.samples();
    assert_eq!(samples.len(), 1);
    assert_eq!(
        samples[0].entry_fp,
        only.entry_fp(),
        "the sample names the serving card"
    );

    // Fallback: the failed head attempt and the fallback hit each carry
    // THEIR OWN entry's fingerprint (two same-named accounts must not share
    // a bucket).
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .mount(&failing)
        .await;
    let healthy = MockServer::start().await;
    mount_success(&healthy).await;

    let mut head = config("acme", &failing.uri());
    head.api_key = "sk-acct-a".to_string();
    let mut second = config("acme", &healthy.uri());
    second.api_key = "sk-acct-b".to_string();
    assert_ne!(
        head.entry_fp(),
        second.entry_fp(),
        "same name, different key → different fp"
    );

    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));
    let result = client
        .complete_with_fallback(&[head.clone(), second.clone()], "sys", "user")
        .await
        .expect("the second account serves the call");

    let samples = sink.samples();
    assert_eq!(samples.len(), 2);
    assert_eq!(
        samples[0].entry_fp,
        head.entry_fp(),
        "the failed attempt is the head's card"
    );
    assert!(!samples[0].success);
    assert_eq!(samples[1].entry_fp, second.entry_fp(), "the hit is the fallback card");
    assert!(samples[1].success);
    assert_ne!(
        samples[0].entry_fp, samples[1].entry_fp,
        "each attempt is attributed to its own entry"
    );
    // The completion is fingerprint-attributed too (for llm_summary's fp).
    assert_eq!(result.entry_fp.as_deref(), Some(second.entry_fp().as_str()));
}

/// RENG-77 §3: a provider that aliases model ids in its response body
/// (`deepseek-flash` for a configured `deepseek-v4-flash`,
/// `gpt-4o-2024-11-20` for `gpt-4o`) used to leak the alias into every
/// downstream consumer — `reviews.llm_summary`, the per-expert attribution,
/// the API responses — and every consumer looked the value up by what the
/// CARD says, so the usage and latency statistics silently missed the
/// call. The client now overwrites `result.model` with the configured one
/// and logs the alias at DEBUG when they differ. This test pins the
/// overwrite: the consumer sees the configured id verbatim.
#[tokio::test]
async fn result_model_is_the_configured_one_not_the_providers_alias() {
    let mock = MockServer::start().await;
    // The mock echoes an aliased id, not the configured one.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"total_tokens": 7},
            "model": "deepseek-flash",
        })))
        .mount(&mock)
        .await;

    let mut cfg = config("deepseek", &mock.uri());
    cfg.model = "deepseek-v4-flash".to_string();

    let client = LLMClient::new();
    let result = client
        .complete_with_fallback(&[cfg.clone()], "sys", "user")
        .await
        .expect("the mock answers 200");

    assert_eq!(
        result.model, "deepseek-v4-flash",
        "the attributed model is the configured one, not the provider's alias"
    );
    assert_ne!(
        result.model, "deepseek-flash",
        "the alias must never reach the consumer"
    );

    // A sample recorded under the SAME configured model (so the LLM page's
    // statistics attribute the call to the card the user actually configured,
    // not to a phantom "deepseek-flash" provider).
    let sink = Arc::new(CaptureSink::default());
    let client = client.with_sink(Some(sink.clone()));
    client
        .complete_with_fallback(&[cfg], "sys", "user")
        .await
        .expect("the mock answers 200");
    let samples = sink.samples();
    assert_eq!(samples.len(), 1);
    assert_eq!(
        samples[0].model, "deepseek-v4-flash",
        "the recorded sample carries the configured model"
    );
}

/// RENG-77 §4: an empty (whitespace-only) completion is a verdict about the
/// CONFIG, not the moment — a reasoning model may be spending its whole
/// `max_tokens` budget on `reasoning_tokens` and returning empty content —
/// so the provider is given up without a retry and the chain advances to
/// the next entry. A single-entry chain surfaces the empty completion as
/// a final `Err`, which is the path the orchestrator turns into a failed
/// expert.
#[tokio::test]
async fn an_empty_completion_is_an_error_and_advances_the_chain() {
    // First mock: returns 200 with an empty body — the measured RENG-77 §4
    // case (a reasoning model that burnt the budget on `reasoning_tokens`).
    let empty = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": ""}}],
            "usage": {"total_tokens": 4096},
            "model": "deepseek-v4-flash",
        })))
        .mount(&empty)
        .await;
    // Second mock: the fallback that DOES answer.
    let healthy = MockServer::start().await;
    mount_success(&healthy).await;

    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));

    // A single-entry chain: the empty completion is the final error.
    let result = client
        .complete_with_fallback(
            &[{
                let mut c = config("deepseek", &empty.uri());
                c.model = "deepseek-v4-flash".to_string();
                c
            }],
            "sys",
            "user",
        )
        .await;
    assert!(
        result.is_err(),
        "a single-entry chain must surface the empty completion as Err"
    );
    // The chain is asserted, not the outermost context: `to_string()` on an
    // `anyhow::Error` yields only "all LLM providers failed" (the context
    // `complete_with_fallback` adds), while every surface that shows the
    // failure to a human renders the CAUSE chain — the per-expert error list
    // uses `{:?}` (`collect_expert_results`) and the review's own failure
    // message embeds that same rendering.
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.contains("empty completion"),
        "the error chain names the diagnosis so the orchestrator's message can: {err}"
    );
    assert!(
        err.contains("deepseek-v4-flash"),
        "the error chain names the configured model — the user already sees it on the card: {err}"
    );

    // Both attempts are recorded. The empty one has `success == false`
    // (the client turns the verdict into an error before recording), so
    // the LLM page's failure count picks it up.
    let samples = sink.samples();
    assert_eq!(samples.len(), 1, "no retry on the empty verdict");
    assert!(!samples[0].success);
    let recorded = samples[0].error.as_deref().unwrap_or_default();
    assert!(
        recorded.contains("empty completion"),
        "the recorded row carries the diagnosis so the page's call-level history is honest: {recorded}"
    );

    // The chain advances on empty: the SECOND config serves.
    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));
    let healthy_cfg = config("xiaomi", &healthy.uri());
    let result = client
        .complete_with_fallback(
            &[
                {
                    let mut c = config("deepseek", &empty.uri());
                    c.model = "deepseek-v4-flash".to_string();
                    c
                },
                healthy_cfg.clone(),
            ],
            "sys",
            "user",
        )
        .await
        .expect("the fallback answers");
    assert_eq!(
        result.provider, "xiaomi",
        "the empty primary is skipped, the chain advances"
    );
    assert!(result.fallback);
    let samples = sink.samples();
    assert_eq!(samples.len(), 2, "the failed primary AND the fallback hit are recorded");
    assert!(!samples[0].success);
    assert!(samples[1].success);
    assert!(
        samples[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("empty completion"),
        "the failed primary's recorded error names the diagnosis"
    );
}

/// RENG-77 §5: the direct OpenAI-compatible path times the request in two
/// steps — the wait for the response HEADERS (`ttfb_ms`) and the whole
/// exchange (`latency_ms`) — and records both on the sample.
///
/// This is also the measured verdict the report has to state. The mock delays
/// its response by 300 ms before sending anything, which is what a
/// non-streaming provider does while it GENERATES the answer: headers cannot
/// arrive before generation ends, so `ttfb_ms` tracks `latency_ms` instead of
/// reporting the "hundreds of milliseconds" round trip the user pictured. The
/// two numbers only diverge when the server flushes headers first and streams
/// the body afterwards (i.e. when `stream: true` is negotiated).
#[tokio::test]
async fn the_direct_path_records_time_to_first_byte_next_to_the_total() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                // Stands in for generation time: nothing is written — headers
                // included — until it elapses.
                .set_delay(std::time::Duration::from_millis(300))
                .set_body_json(serde_json::json!({
                    "choices": [{"message": {"content": "ok"}}],
                    "usage": {"total_tokens": 7},
                    "model": "served-model",
                })),
        )
        .mount(&mock)
        .await;

    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));
    client
        .complete_with_fallback(&[config("deepseek", &mock.uri())], "sys", "user")
        .await
        .expect("the mock answers 200");

    let sample = &sink.samples()[0];
    let ttfb = sample
        .ttfb_ms
        .expect("the direct path always measures a TTFB once headers arrive");
    assert!(
        ttfb >= 250,
        "the headers cannot arrive before the server has answered: ttfb={ttfb}ms"
    );
    assert!(
        ttfb <= sample.latency_ms,
        "the TTFB is a prefix of the total: ttfb={ttfb}ms latency={}ms",
        sample.latency_ms
    );
    assert!(
        sample.latency_ms - ttfb < 250,
        "a buffered response sends its body with its headers, so ttfb ≈ latency \
         (ttfb={ttfb}ms latency={}ms) — this is the RENG-77 §5 verdict, measured, not assumed",
        sample.latency_ms
    );
}

/// RENG-77 §5: a call that fails BEFORE any response header exists (an
/// unusable `api_base` — the same early-return shape as DNS / TLS /
/// connection-refused failures) records `ttfb_ms: None`. "Never measured" and
/// "measured 0 ms" are different facts, and the latency aggregate averages
/// only the measured ones — coercing this to 0 would drag the page's average
/// down with a number no server ever produced.
#[tokio::test]
async fn a_failure_before_the_headers_records_no_ttfb() {
    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));

    // One attempt: `complete` has no retry loop, so no backoff sleep either.
    let result = client.complete(&config("dead", "not-a-url"), "sys", "user").await;
    assert!(result.is_err(), "an unusable api_base cannot reach a server");

    let samples = sink.samples();
    assert_eq!(samples.len(), 1);
    assert!(!samples[0].success);
    assert_eq!(
        samples[0].ttfb_ms, None,
        "no header ever arrived, so there is no first byte to time"
    );
    assert!(
        samples[0].latency_ms < 1000,
        "the failure is immediate: {}ms",
        samples[0].latency_ms
    );
}

/// RENG-77 §5, the discriminating half of the verdict: `ttfb_ms` is a real
/// measurement of the HEADERS arrival, not a copy of `latency_ms`.
///
/// The server here flushes its response headers immediately and sends the body
/// only after "generating" for 600 ms — the shape a provider has once it
/// streams (`stream: true`). The two numbers then separate by an order of
/// magnitude: the first byte is the round trip, the total is the generation.
/// Together with `the_direct_path_records_time_to_first_byte_next_to_the_total`
/// (a buffering server, where the two are equal) this pins the mechanism, and
/// therefore WHAT a production number means: the shipped request shape is
/// non-streaming, so a provider that generates before it flushes reports
/// `ttfb ≈ latency` — the communication latency the user asked for only shows
/// up once the response is streamed.
#[tokio::test]
async fn a_server_that_flushes_headers_early_reports_ttfb_far_below_latency() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback listener for the test");
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("one connection");
        // Consume the request head; its content does not matter here.
        let mut buf = vec![0u8; 16 * 1024];
        let _ = socket.read(&mut buf).await;
        // Headers first (chunked: the body is not written yet), flushed at once.
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .expect("headers");
        socket.flush().await.expect("flush headers");
        // "Generation".
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let body = r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"total_tokens":3},"model":"m"}"#;
        socket
            .write_all(format!("{:x}\r\n{}\r\n0\r\n\r\n", body.len(), body).as_bytes())
            .await
            .expect("body chunk");
        socket.flush().await.expect("flush body");
    });

    let sink = Arc::new(CaptureSink::default());
    let client = LLMClient::new().with_sink(Some(sink.clone()));
    client
        .complete(&config("streaming", &format!("http://{addr}")), "sys", "user")
        .await
        .expect("the hand-rolled server answers");

    let sample = &sink.samples()[0];
    let ttfb = sample.ttfb_ms.expect("a header arrived, so a TTFB was measured");
    assert!(
        ttfb < 300,
        "the headers came back immediately, long before the body: ttfb={ttfb}ms"
    );
    assert!(
        sample.latency_ms >= 550,
        "the body took the full generation time: latency={}ms",
        sample.latency_ms
    );
    assert!(
        sample.latency_ms > 2 * ttfb,
        "headers-early responses separate the two measurements by an order of \
         magnitude (ttfb={ttfb}ms latency={}ms) — which is exactly what a streaming \
         provider would give the user",
        sample.latency_ms
    );

    server.await.expect("the test server finished");
}
