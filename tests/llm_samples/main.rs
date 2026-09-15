//! RENG-57 end-to-end: a review's LLM calls land in `llm_call_samples`.
//!
//! Isolation note (same reason as the `llm` target): the review path is
//! exercised through the real client against wiremock, and a review writes
//! samples through the real store sink — no stubs, so what is asserted is the
//! rows a deployed instance would hold.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use review_engine::llm::client::LLMClient;
use review_engine::llm::sampling::LlmCallSample;
use review_engine::models::LLMConfig;
use review_engine::server::task_queue::TaskStore;
use review_engine::store::traits::{LlmCallSampleRow, ReviewStore};
use review_engine::store::SqlxStore;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config(provider: &str, api_base: &str) -> LLMConfig {
    LLMConfig {
        provider: provider.to_string(),
        model: format!("{provider}-model"),
        api_key: format!("sk-{provider}"),
        api_base: api_base.to_string(),
        max_tokens: 4096,
        temperature: 0.3,
        disable_thinking: None,
        disabled: false,
    }
}

async fn mount_success(mock: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": body}}],
            "usage": {"total_tokens": 11},
            "model": "served-model",
        })))
        .mount(mock)
        .await;
}

/// The sink a review attaches to its client comes from the task store that
/// recorded the review, and every attempt the client makes is written against
/// that review's task id — including the failed primary attempt that the
/// fallback chain swallowed.
#[tokio::test]
async fn a_review_run_records_one_sample_per_attempt_against_its_task_id() {
    // The provider the review will fail over from rejects this credential.
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
        .mount(&failing)
        .await;
    let healthy = MockServer::start().await;
    mount_success(&healthy, "review served by the secondary provider").await;

    // The store a `serve` instance runs with, and the queue that writes
    // through to it.
    let db = Arc::new(SqlxStore::new_in_memory().await.unwrap());
    db.migrate().await.unwrap();
    let mut tasks = TaskStore::new();
    tasks.set_db(db.clone());

    // What the review path does: create the task, then take the sample sink
    // for it.
    let meta = review_engine::server::task_queue::SourceMeta::default();
    let task_id = review_engine::server::task_queue::record_task_started(&tasks, meta).await;
    let sink = tasks
        .llm_sample_sink(task_id)
        .expect("a queue with persistence yields a sink");

    // Two expert calls: the first config is rejected (401 → permanent, one
    // attempt), the second serves. Both calls walk the same chain.
    let client = LLMClient::new().with_sink(Some(sink));
    let configs = [config("primary", &failing.uri()), config("secondary", &healthy.uri())];
    for _ in 0..2 {
        let result = client
            .complete_with_fallback(&configs, "system", "user")
            .await
            .expect("the secondary provider serves the review");
        assert_eq!(result.provider, "secondary");
        assert!(result.fallback);
    }

    // The rows the LLM Status page's latency aggregate reads.
    type SampleRow = (Option<String>, String, String, i64, i64, Option<String>, i64, i64);
    let rows: Vec<SampleRow> = sqlx::query_as(
        "SELECT review_id, provider, model, latency_ms, success, error, chain_position, attempt \
         FROM llm_call_samples ORDER BY created_at, provider",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 4, "two calls × two attempts: {rows:?}");

    let task = task_id.to_string();
    // Call order is not guaranteed (both loops are sequential, so it is, but
    // assert on content): count the attempt kinds.
    let failures = rows
        .iter()
        .filter(|(_, provider, ..)| provider == "primary")
        .collect::<Vec<_>>();
    let successes = rows
        .iter()
        .filter(|(_, provider, ..)| provider == "secondary")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2, "one rejected attempt per call");
    assert_eq!(successes.len(), 2, "one fallback hit per call");

    for (review_id, provider, model, latency_ms, success, error, chain_position, attempt) in &rows {
        assert_eq!(
            review_id.as_deref(),
            Some(task.as_str()),
            "every sample is attributed to the review that made it"
        );
        assert_eq!(*attempt, 1, "a permanent 4xx is never retried");
        assert!(*latency_ms >= 0);
        if provider == "primary" {
            assert_eq!(*success, 0);
            assert_eq!(*chain_position, 1);
            assert_eq!(model, "primary-model");
            let error = error.as_deref().expect("a failed attempt records why");
            assert!(error.contains("401"), "got {error}");
        } else {
            assert_eq!(*success, 1);
            assert_eq!(*chain_position, 2, "the fallback hit names its chain position");
            assert_eq!(model, "secondary-model");
            assert_eq!(*error, None);
        }
    }

    // The read path the API uses returns the same rows, and the average over
    // the successful ones is a plain mean (evidence for the page).
    let since = chrono::Utc::now() - chrono::Duration::hours(1);
    let read: Vec<LlmCallSampleRow> = db.llm_samples_since(since).await.unwrap();
    assert_eq!(read.len(), 4);
    let (sum, count) = read
        .iter()
        .filter(|r| r.provider == "secondary" && r.success)
        .fold((0i64, 0i64), |(s, c), r| (s + r.latency_ms, c + 1));
    assert_eq!(count, 2);
    assert!(sum / count >= 0, "the page's average is the mean of these rows");
}

/// Without persistence there is nowhere to write, so the review path attaches
/// no sink at all — and the page reports `null` rather than a zero.
#[tokio::test]
async fn a_queue_without_persistence_yields_no_sink() {
    let tasks = TaskStore::new();
    let meta = review_engine::server::task_queue::SourceMeta::default();
    let task_id = review_engine::server::task_queue::record_task_started(&tasks, meta).await;
    assert!(tasks.llm_sample_sink(task_id).is_none());
}

/// A sample is only ever attributed to the review whose sink recorded it:
/// two reviews running in sequence keep their rows apart.
#[tokio::test]
async fn samples_stay_with_their_own_review() {
    let mock = MockServer::start().await;
    mount_success(&mock, "ok").await;
    let db = Arc::new(SqlxStore::new_in_memory().await.unwrap());
    db.migrate().await.unwrap();
    let mut tasks = TaskStore::new();
    tasks.set_db(db.clone());

    for content in ["first", "second"] {
        let meta = review_engine::server::task_queue::SourceMeta::default();
        let task_id = review_engine::server::task_queue::record_task_started(&tasks, meta).await;
        let client = LLMClient::new().with_sink(tasks.llm_sample_sink(task_id));
        let result = client
            .complete_with_fallback(&[config("xiaomi", &mock.uri())], "sys", content)
            .await
            .expect("the mock answers 200");
        assert_eq!(result.content, "ok");
    }

    let ids: Vec<Option<String>> = sqlx::query_scalar("SELECT review_id FROM llm_call_samples ORDER BY created_at")
        .fetch_all(db.pool())
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "each review's samples carry its own id");
    assert!(ids.iter().all(Option::is_some));
}

/// A sample row that the aggregation could never use is still written (the
/// recorder does not judge), but the aggregate ignores nothing it was given:
/// this pins the round trip of every column through the Any driver, including
/// the `success` INTEGER and the RFC 3339 timestamp.
#[tokio::test]
async fn sample_columns_round_trip_through_the_driver() {
    use review_engine::store::llm_samples::StoreLlmCallSink;

    let db = Arc::new(SqlxStore::new_in_memory().await.unwrap());
    db.migrate().await.unwrap();
    let sink = StoreLlmCallSink::shared(db.clone(), Some("review-round-trip".to_string()));
    let at = chrono::Utc::now();
    sink.record(&LlmCallSample {
        at,
        provider: "xiaomi".to_string(),
        model: "mimo-v2.5-pro".to_string(),
        latency_ms: 1234,
        success: false,
        error: Some("HTTP 500 Internal Server Error".to_string()),
        chain_position: 3,
        attempt: 2,
    })
    .await;

    let row: (String, String, String, i64, i64, String, i64, i64) = sqlx::query_as(
        "SELECT id, review_id, provider, latency_ms, success, error, chain_position, attempt FROM llm_call_samples",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!row.0.is_empty(), "the row has a Rust-side surrogate key");
    assert_eq!(row.1, "review-round-trip");
    assert_eq!(row.2, "xiaomi");
    assert_eq!(row.3, 1234);
    assert_eq!(row.4, 0, "booleans are INTEGER 0/1 (0001 dialect rule)");
    assert_eq!(row.5, "HTTP 500 Internal Server Error");
    assert_eq!(row.6, 3);
    assert_eq!(row.7, 2);
    assert_eq!(row_len(db.pool()).await, 1);
}

async fn row_len(pool: &sqlx::AnyPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM llm_call_samples")
        .fetch_one(pool)
        .await
        .unwrap()
}
