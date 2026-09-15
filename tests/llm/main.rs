//! End-to-end coverage for the RENG-35 retry verdict: a permanent 4xx on one
//! config must give up immediately *and* still advance the fallback chain, with
//! the RENG-55 log lines making the advance visible.
//!
//! This lives in its own integration binary because it captures `tracing`
//! output through a thread-local subscriber, and the tracing callsite interest
//! cache is process-global: in a shared test binary another thread can cache
//! these callsites as disabled before the subscriber is installed, after which
//! the events are skipped no matter which subscriber is current. A process that
//! has only this test cannot race itself.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use review_engine::llm::client::LLMClient;
use review_engine::models::LLMConfig;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `tracing` writer that accumulates the subscriber's output in memory.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn config(provider: &str, api_base: &str, api_key: &str) -> LLMConfig {
    LLMConfig {
        provider: provider.to_string(),
        model: format!("{provider}-model"),
        api_key: api_key.to_string(),
        api_base: api_base.to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }
}

/// The reported symptom, end to end: the primary config's `api_key` is wrong and
/// answers 401. The call must spend exactly one request on it — no retry, no
/// backoff sleep — log that it is not retrying, and then be served by the next
/// config in the chain, flagged as a fallback.
#[tokio::test]
async fn test_permanent_401_fails_fast_then_falls_back_to_the_next_config() {
    let logs = CapturedLogs::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::INFO)
            .finish(),
    );

    // Primary: rejected credentials.
    let primary = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(
                r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#,
            ),
        )
        .expect(1)
        .mount(&primary)
        .await;

    // Secondary: a different provider, working credentials.
    let secondary = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"model":"secondary-model","choices":[{"message":{"content":"ok"}}],"usage":{"total_tokens":7}}"#,
        ))
        .expect(1)
        .mount(&secondary)
        .await;

    let client = LLMClient::new();
    let result = client
        .complete_with_fallback(
            &[
                config("primary", &primary.uri(), "sk-wrong-key"),
                config("secondary", &secondary.uri(), "sk-good-key"),
            ],
            "system",
            "user",
        )
        .await
        .expect("the secondary config must serve the call");

    assert_eq!(result.content, "ok");
    assert_eq!(
        result.provider, "secondary",
        "the hit must be attributed to the config that answered"
    );
    assert!(result.fallback, "a non-primary hit must be flagged (RENG-55)");
    assert_eq!(
        primary
            .received_requests()
            .await
            .expect("request recording enabled")
            .len(),
        1,
        "the 401 must be attempted exactly once before the chain advances"
    );

    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("LLM request failed permanently (401), not retrying"),
        "the give-up must name the status: {text}"
    );
    assert!(
        text.contains("attempt=1"),
        "the give-up must report the attempt count: {text}"
    );
    assert!(
        text.contains("falling back to the next provider in the chain"),
        "the RENG-55 chain-advance log must stay: {text}"
    );
    assert!(
        text.contains("LLM fallback engaged"),
        "the RENG-55 fallback-hit log must stay: {text}"
    );
}
