use std::time::{Duration, Instant};

use super::{
    bootstrap_authed_client, find_free_port, spawn_server, spawn_server_inner_with_env, wait_for_server, API_TOKEN,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── llm_configs 回退语义 (defect fix: state.llm_configs) ─────────

/// Mount a mock OpenAI-compatible `/chat/completions` endpoint returning a
/// fixed, empty-findings completion body. `parse_llm_response` is fail-soft, so
/// every expert task succeeds (reports stay non-empty) and the review settles
/// as `completed` — exactly what a real provider would produce for a 1-line diff.
async fn mount_mock_llm(mock: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": "findings: []"}}],
            "usage": {"total_tokens": 8},
            "model": "gpt-4o"
        })))
        .mount(mock)
        .await;
}

/// An `LLMConfig`-shaped provider JSON with `api_base` pointing at the given
/// endpoint — used both for the server-side `LLM_CONFIG` env and for the
/// request's explicit `llm_configs`.
pub(super) fn mock_llm_provider(api_base: &str) -> serde_json::Value {
    serde_json::json!({
        "provider": "openai",
        "model": "gpt-4o",
        "api_key": "sk-test",
        "api_base": api_base,
        "max_tokens": 2048,
        "temperature": 0.3
    })
}

/// POST a review from a static diff (optionally carrying explicit
/// `llm_configs`) and poll `GET /api/v1/reviews/{task_id}` until the task
/// settles, returning the final body.
async fn post_review_and_poll(
    base: &str,
    client: &reqwest::Client,
    llm_configs: Option<serde_json::Value>,
) -> serde_json::Value {
    let url = format!("{}/api/v1/reviews", base);
    let mut body = serde_json::json!({
        "source": {"type": "static_diff", "diff": "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-f()\n+g()\n"}
    });
    if let Some(configs) = llm_configs {
        body["llm_configs"] = configs;
    }
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "POST /api/v1/reviews returned {}",
        resp.status()
    );
    let created: serde_json::Value = resp.json().await.expect("POST response body is not JSON");
    let task_id = created["task_id"].as_str().expect("POST response missing task_id");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let resp = client
            .get(format!("{}/api/v1/reviews/{}", base, task_id))
            .send()
            .await
            .expect("failed to GET /api/v1/reviews/{task_id}");
        let body: serde_json::Value = resp.json().await.expect("GET response body is not JSON");
        match body["status"].as_str().unwrap_or("") {
            "completed" => return body,
            "failed" => return body,
            _ if Instant::now() > deadline => panic!("review did not settle within 60s: {:?}", body),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Defect regression: the frontend's `POST /api/v1/reviews` never sends
/// `llm_configs`, so the server must fall back to `state.llm_configs` (seeded
/// from the `LLM_CONFIG` env — the Docker compose standard form). Before the
/// fix the review ran with zero providers and failed with "LLM config
/// 'default' has no api_base set".
#[tokio::test]
async fn review_falls_back_to_server_llm_configs_when_request_omits_them() {
    let mock = MockServer::start().await;
    mount_mock_llm(&mock).await;
    // `LLM_CONFIG` must be a JSON array of provider objects (Docker compose
    // standard form); `mock_llm_provider` returns a single object.
    let llm_config_env = serde_json::json!([mock_llm_provider(&mock.uri())]).to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);
    // No `llm_configs` in the body — exactly what the frontend sends.
    let final_body = post_review_and_poll(&base, &client, None).await;

    assert_eq!(
        final_body["status"].as_str(),
        Some("completed"),
        "review without request llm_configs must complete via the server-side provider, got {:?}",
        final_body
    );
    assert!(
        final_body["error"].is_null(),
        "completed task must not carry an error, got {:?}",
        final_body["error"]
    );

    let requests = mock.received_requests().await.expect("received requests");
    let llm_hits = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .count();
    assert!(
        llm_hits >= 1,
        "the server-side LLM provider must actually have been called"
    );
}

/// Request-explicit `llm_configs` take priority over the server-side state:
/// when both a `LLM_CONFIG` provider and an in-body provider exist, only the
/// request's endpoint is used.
#[tokio::test]
async fn review_request_llm_configs_take_priority_over_server_state() {
    let server_mock = MockServer::start().await;
    mount_mock_llm(&server_mock).await;
    let request_mock = MockServer::start().await;
    mount_mock_llm(&request_mock).await;

    let server_provider = mock_llm_provider(&server_mock.uri());
    let request_provider = mock_llm_provider(&request_mock.uri());
    // Both providers are configured: one server-side (LLM_CONFIG), one carried
    // explicitly in the request body. The request's must win.
    let server_llm_config_env = serde_json::json!([server_provider]).to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("LLM_CONFIG", &server_llm_config_env)]);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    // The server-side provider must actually be registered (so the 0-hits
    // assertion below is meaningful, not vacuous).
    let providers: serde_json::Value = client
        .get(format!("{}/api/v1/llm/providers", base))
        .send()
        .await
        .expect("GET /api/v1/llm/providers")
        .json()
        .await
        .expect("providers body is JSON");
    let items = providers["items"].as_array().expect("providers.items is an array");
    assert_eq!(
        items.len(),
        1,
        "server-side LLM_CONFIG provider must be registered, got {:?}",
        providers
    );

    let final_body = post_review_and_poll(&base, &client, Some(serde_json::json!([request_provider]))).await;

    assert_eq!(
        final_body["status"].as_str(),
        Some("completed"),
        "review with explicit request llm_configs must complete, got {:?}",
        final_body
    );

    let request_hits = request_mock.received_requests().await.expect("request-mock requests");
    let server_hits = server_mock.received_requests().await.expect("server-mock requests");
    let request_llm_hits = request_hits
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .count();
    let server_llm_hits = server_hits
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .count();
    assert!(request_llm_hits >= 1, "the request-provided provider must be used");
    assert_eq!(
        server_llm_hits, 0,
        "the server-side provider must not be used when the request provides its own"
    );
}

/// Unit 1 (integration): a completed review emits structured lifecycle log
/// entries carrying `metadata.reviewId`/`requestId`/`durationMs`, so the
/// log-page badges are no longer dead fields.
#[tokio::test]
async fn review_lifecycle_logs_carry_metadata() {
    let mock = MockServer::start().await;
    mount_mock_llm(&mock).await;
    let llm_config_env = serde_json::json!([mock_llm_provider(&mock.uri())]).to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);
    let final_body = post_review_and_poll(&base, &client, None).await;
    assert_eq!(final_body["status"].as_str(), Some("completed"));
    let task_id = final_body["task_id"].as_str().expect("task_id").to_string();

    // The completed lifecycle entry is pushed before the store update, so once
    // the task reports `completed` the log must already carry its metadata.
    let logs = client
        .get(format!("{}/api/v1/logs/download", base))
        .send()
        .await
        .expect("GET /api/v1/logs/download")
        .text()
        .await
        .expect("logs body");

    let mut found_review_meta = false;
    let mut found_duration = false;
    for line in logs.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap_or(serde_json::Value::Null);
        if value["metadata"]["reviewId"].as_str() == Some(&task_id) {
            found_review_meta = true;
            if value["metadata"]["durationMs"].is_number() {
                found_duration = true;
            }
        }
    }
    assert!(
        found_review_meta,
        "logs must contain a lifecycle entry with metadata.reviewId == {task_id}, got:\n{}",
        logs
    );
    assert!(
        found_duration,
        "the completed lifecycle entry must carry metadata.durationMs, got:\n{}",
        logs
    );
}

/// Unit 10: `POST /config/validate` with a malformed/missing body returns a JSON
/// `{"error": ...}` payload (not axum's default plain-text 422).
#[tokio::test]
async fn config_validate_missing_body_returns_json_error() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/v1/config/validate", port))
        .json(&serde_json::json!({})) // missing required `body` field
        .send()
        .await
        .expect("failed to POST /api/v1/config/validate");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY,
        "expected 422 for a body missing `body`, got {}",
        resp.status()
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("application/json"),
        "error response must be JSON, got content-type {}",
        content_type
    );
    let body: serde_json::Value = resp.json().await.expect("422 body must be JSON");
    assert!(
        body.get("error").is_some(),
        "422 body must contain an `error` key, got {:?}",
        body
    );
}

// ─── 0.10.1: webhook path falls back to state.llm_configs ─────────

/// Defect regression (0.10.1): the webhook review path
/// (`run_review_common` via `POST /webhook/gitlab`) resolved LLM providers
/// from only the config file `[[llm]]` and the `LLM_CONFIG` env — never the
/// hot-applied `state.llm_configs`. A provider added through the WebUI
/// (`POST /api/v1/llm/providers`, the exact flow reproduced below) therefore
/// had no effect on webhook-triggered reviews, which failed with
/// "LLM config 'default' has no api_base set".
///
/// Setup: a mock GitLab instance serves the MR metadata/diff, a mock
/// OpenAI-compatible endpoint serves the LLM calls, and the server is spawned
/// with NEITHER `LLM_CONFIG` nor a config file (fresh HOME). The provider
/// exists only in `state.llm_configs`. The webhook-dispatched review must
/// complete and actually call that provider.
#[tokio::test]
async fn webhook_review_uses_hot_applied_server_llm_configs() {
    let gitlab = MockServer::start().await;
    let llm = MockServer::start().await;
    mount_mock_llm(&llm).await;

    // Minimal GitLab API surface for resolve_review_source + publish:
    // MR metadata, the raw diff, and the discussion/notes publish calls.
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproj/merge_requests/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "title": "Fix login bug",
            "description": "",
            "source_branch": "feature/login",
            "target_branch": "main",
            "author": {"id": 1, "username": "alice", "name": "Alice"},
            "diff_refs": {"base_sha": "base1", "head_sha": "abc123", "start_sha": "base1"}
        })))
        .mount(&gitlab)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproj/merge_requests/7/raw_diffs"))
        .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-f()\n+g()\n"))
        .mount(&gitlab)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproj/merge_requests/7/discussions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&gitlab)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/projects/group%2Fproj/merge_requests/7/notes"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id": 1})))
        .mount(&gitlab)
        .await;

    let mr_url = format!("{}/group/proj/-/merge_requests/7", gitlab.uri());

    // No LLM_CONFIG env, no config file (fresh HOME in the spawned server):
    // the ONLY LLM provider source is the hot-applied server state below.
    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[("GITLAB_WEBHOOK_SECRET", "hook-secret"), ("GITLAB_TOKEN", "glpat-test")],
    );
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    // The WebUI "add provider" flow: hot-applies into `state.llm_configs`
    // without touching the config file or env.
    let resp = client
        .post(format!("{}/api/v1/llm/providers", base))
        .json(&serde_json::json!({
            "provider": "openai",
            "model": "gpt-4o",
            "apiKey": "sk-test",
            "apiBaseUrl": llm.uri(),
            "maxTokens": 2048,
            "temperature": 0.3,
        }))
        .send()
        .await
        .expect("failed to POST /api/v1/llm/providers");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "POST /api/v1/llm/providers returned {}",
        resp.status()
    );

    // Fire the GitLab MR webhook (verified via the legacy secret token).
    let hook_body = serde_json::json!({
        "object_attributes": {
            "action": "open",
            "iid": 7,
            "title": "Fix login bug",
            "source_branch": "feature/login",
            "target_branch": "main",
            "url": mr_url,
            "last_commit": {"id": "abc123", "author": {"name": "alice"}},
        },
        "project": {
            "path_with_namespace": "group/proj",
            "web_url": format!("{}/group/proj", gitlab.uri()),
        },
        "user": {"name": "alice"},
    });
    let resp = reqwest::Client::new()
        .post(format!("{}/webhook/gitlab", base))
        .header("X-Gitlab-Event", "Merge Request Hook")
        .header("X-Gitlab-Token", "hook-secret")
        .json(&hook_body)
        .send()
        .await
        .expect("failed to POST /webhook/gitlab");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "webhook must be accepted, got {}",
        resp.status()
    );

    // The review runs on a detached task: poll the history list until the
    // webhook task settles. A fresh server has exactly this one task.
    let deadline = Instant::now() + Duration::from_secs(60);
    let final_item = loop {
        let list: serde_json::Value = client
            .get(format!("{}/api/v1/reviews?per_page=100", base))
            .send()
            .await
            .expect("failed to GET /api/v1/reviews")
            .json()
            .await
            .expect("reviews list body is not JSON");
        let items = list["items"].as_array().expect("reviews.items is an array");
        let item = items.iter().find(|i| {
            i["gitlabMrUrl"].as_str() == Some(mr_url.as_str()) || i["gitlab_mr_url"].as_str() == Some(mr_url.as_str())
        });
        match item.map(|i| i["status"].as_str().unwrap_or("")) {
            Some("completed") | Some("failed") => break item.unwrap().clone(),
            _ if Instant::now() > deadline => {
                panic!("webhook review did not settle within 60s: {:?}", list)
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    };

    assert_eq!(
        final_item["status"].as_str(),
        Some("completed"),
        "webhook-triggered review must complete via the hot-applied provider, got {:?}",
        final_item
    );

    let requests = llm.received_requests().await.expect("received requests");
    let llm_hits = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .count();
    assert!(
        llm_hits >= 1,
        "the WebUI-configured LLM provider must actually have been called by the webhook review"
    );
}
