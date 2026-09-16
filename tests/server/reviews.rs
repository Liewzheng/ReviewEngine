//! Integration tests for the review submission contract (docs/rest-api.md §1):
//! GitLab credentials travel in the `X-Gitlab-Token` header (never the body,
//! never persisted), and webhook callback URLs are SSRF-validated at enqueue
//! time with a 400 on failure.
use std::time::{Duration, Instant};

use super::{
    bootstrap_authed_client, find_free_port, spawn_server_inner_with_env, unreachable_llm_config_env, wait_for_server,
    API_TOKEN,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A gitlab_mr body with a parseable but unreachable MR URL — the reserved
/// `.invalid` TLD never resolves. It is not a local address, so it passes the
/// enqueue-time URL gates (parse + RENG-33 host routing); the enqueued task
/// then fails fast on the failed lookup (no external network I/O), so these
/// tests exercise the HTTP contract only.
fn gitlab_mr_body() -> serde_json::Value {
    serde_json::json!({
        "source": {"type": "gitlab_mr", "url": "http://gitlab.invalid:8929/owner/repo/-/merge_requests/1"}
    })
}

async fn response_parts(resp: reqwest::Response) -> (reqwest::StatusCode, serde_json::Value) {
    let status = resp.status();
    let json: serde_json::Value = resp.json().await.expect("response body is JSON");
    (status, json)
}

/// Poll `GET /api/v1/reviews/{task_id}` until the task settles.
async fn poll_until_settled(base: &str, client: &reqwest::Client, task_id: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let resp = client
            .get(format!("{}/api/v1/reviews/{}", base, task_id))
            .send()
            .await
            .expect("failed to GET /api/v1/reviews/{task_id}");
        let body: serde_json::Value = resp.json().await.expect("GET body is JSON");
        match body["status"].as_str().unwrap_or("") {
            "completed" | "failed" => return body,
            _ if Instant::now() > deadline => panic!("review did not settle within 30s: {:?}", body),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

#[tokio::test]
async fn submit_gitlab_mr_accepts_x_gitlab_token_header() {
    let port = find_free_port();
    // Neutralize any inherited GITLAB_TOKEN so the header is the only credential.
    // The unreachable LLM provider passes the no-usable-LLM enqueue gate; the
    // task then fails fast at runtime, which this test does not wait for.
    let llm_config_env = unreachable_llm_config_env();
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", ""), ("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    let resp = client
        .post(format!("{}/api/v1/reviews", base))
        .header("X-Gitlab-Token", "glpat-header-token")
        .json(&gitlab_mr_body())
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "header token must be accepted, got {json}"
    );
    assert!(json["task_id"].is_string());
    let serialized = json.to_string();
    assert!(
        !serialized.contains("glpat-header-token"),
        "the credential must never appear in the response: {serialized}"
    );
}

#[tokio::test]
async fn submit_gitlab_mr_rejects_body_token() {
    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    let mut body = gitlab_mr_body();
    body["source"]["token"] = serde_json::json!("glpat-body-token");
    let resp = client
        .post(format!("{}/api/v1/reviews", base))
        .header("X-Gitlab-Token", "glpat-header-token")
        .json(&body)
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "a token in the request body must be rejected (fail-closed), got {json}"
    );
    let error = json["error"].as_str().expect("error message");
    assert!(
        error.contains("X-Gitlab-Token"),
        "error must explain the header transport: {error}"
    );
    assert!(
        !error.contains("glpat-body-token"),
        "the credential must never be echoed: {error}"
    );
}

#[tokio::test]
async fn submit_gitlab_mr_without_any_token_returns_400() {
    let port = find_free_port();
    // Deliberately no LLM_CONFIG: the credential rule (400) is a
    // request-shape/auth error and must surface BEFORE the no-usable-LLM
    // policy gate (422) — this test locks in that precedence.
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    let resp = client
        .post(format!("{}/api/v1/reviews", base))
        .json(&gitlab_mr_body())
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "missing header AND missing server-side token must be 400, got {json}"
    );
    assert!(
        json["error"].as_str().unwrap().contains("X-Gitlab-Token"),
        "error must explain the credential rule: {json}"
    );
}

#[tokio::test]
async fn submit_gitlab_mr_falls_back_to_server_env_token() {
    let port = find_free_port();
    // The server-side credential source documented in the design doc:
    // GITLAB_TOKEN (or --gitlab-token) at startup. The unreachable LLM
    // provider passes the no-usable-LLM enqueue gate.
    let llm_config_env = unreachable_llm_config_env();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[("GITLAB_TOKEN", "glpat-env-token"), ("LLM_CONFIG", &llm_config_env)],
    );
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    let resp = client
        .post(format!("{}/api/v1/reviews", base))
        .json(&gitlab_mr_body())
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "server-side configured token must satisfy the credential rule, got {json}"
    );
}

#[tokio::test]
async fn rerun_reresolves_credentials_per_request() {
    let port = find_free_port();
    // No server-side token: only the header can satisfy a rerun. The
    // unreachable LLM provider passes the no-usable-LLM enqueue gate.
    let llm_config_env = unreachable_llm_config_env();
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", ""), ("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    // Submit the original task (header token), then wait for it to fail.
    let resp = client
        .post(format!("{}/api/v1/reviews", base))
        .header("X-Gitlab-Token", "glpat-header-token")
        .json(&gitlab_mr_body())
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    let (status, json) = response_parts(resp).await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "got {json}");
    let task_id = json["task_id"].as_str().unwrap().to_string();
    let settled = poll_until_settled(&base, &client, &task_id).await;
    assert_eq!(settled["status"].as_str(), Some("failed"));

    // Rerun without the header: no credential is persisted, so this is 400.
    let resp = client
        .post(format!("{}/api/v1/reviews/{}/rerun", base, task_id))
        .send()
        .await
        .expect("failed to POST rerun");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "rerun without any credential must be 400 (nothing is persisted), got {json}"
    );

    // Rerun with the header: credentials re-resolve and a fresh task is queued.
    let resp = client
        .post(format!("{}/api/v1/reviews/{}/rerun", base, task_id))
        .header("X-Gitlab-Token", "glpat-rerun-token")
        .send()
        .await
        .expect("failed to POST rerun");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "rerun with the header must re-resolve credentials, got {json}"
    );
    let new_id = json["task_id"].as_str().unwrap();
    assert_ne!(new_id, task_id, "rerun must create a fresh task id");
}

/// RENG-88, end-to-end through the real binary: a review created by the GitLab
/// **webhook** can be re-run. Webhook-created records were persisted with
/// `request = NULL`, so 「重新评审」 answered 409 on every one of them — the
/// reported defect, which hits exactly the deployments whose reviews all come
/// from the webhook.
///
/// The mock GitLab is registered as a git platform, so the payload's external
/// URL is re-hosted onto the mock (the webhook path's normal rewrite) and both
/// the original review and its rerun fetch it from there.
#[tokio::test]
async fn webhook_created_review_can_be_rerun() {
    let gitlab = MockServer::start().await;
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
    // An empty diff settles the review ("No diff changes") without any LLM
    // call, so the test asserts the task lifecycle and nothing else.
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproj/merge_requests/7/raw_diffs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&gitlab)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/group%2Fproj/merge_requests/7/discussions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&gitlab)
        .await;

    let payload_url = "http://gitlab.reng88.invalid:8929/group/proj/-/merge_requests/7";
    // The URL the review actually fetches: the payload URL re-hosted onto the
    // registered platform's `internalBaseUrl` (the mock).
    let review_url = format!("{}/group/proj/-/merge_requests/7", gitlab.uri());
    let llm_config_env = unreachable_llm_config_env();
    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("GITLAB_WEBHOOK_SECRET", "hook-secret"),
            ("GITLAB_TOKEN", "glpat-test"),
            ("LLM_CONFIG", &llm_config_env),
        ],
    );
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    // Register the mock as the platform for the payload's host: `baseUrl` is
    // the address the payload carries, `internalBaseUrl` the one the server
    // reaches (the mock).
    let resp = client
        .put(format!("{}/api/v1/config", base))
        .json(&serde_json::json!({
            "gitPlatforms": [{
                "name": "reng88-mock",
                "type": "gitlab",
                "baseUrl": "http://gitlab.reng88.invalid:8929",
                "internalBaseUrl": gitlab.uri(),
                "token": "glpat-test",
                "webhookSecret": "hook-secret"
            }]
        }))
        .send()
        .await
        .expect("failed to PUT /api/v1/config");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "the mock GitLab must be registered as a git platform, got {}",
        resp.status()
    );

    // The webhook delivery GitLab sends for a newly opened MR.
    let resp = reqwest::Client::new()
        .post(format!("{}/webhook/gitlab", base))
        .header("X-Gitlab-Event", "Merge Request Hook")
        .header("X-Gitlab-Token", "hook-secret")
        .json(&serde_json::json!({
            "object_attributes": {
                "action": "open",
                "iid": 7,
                "title": "Fix login bug",
                "source_branch": "feature/login",
                "target_branch": "main",
                "url": payload_url,
                "last_commit": {"id": "abc123", "author": {"name": "alice"}},
            },
            "project": {"path_with_namespace": "group/proj", "web_url": "http://gitlab.reng88.invalid:8929/group/proj"},
            "user": {"name": "alice"},
        }))
        .send()
        .await
        .expect("failed to POST /webhook/gitlab");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "the webhook must be accepted, got {}",
        resp.status()
    );
    let hook_body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        hook_body["status"].as_str(),
        Some("received"),
        "the webhook must dispatch a review, got {hook_body}"
    );

    // The review is dispatched on a detached task: wait for it to settle, then
    // re-run it. `gitlabMrUrl` is the payload's external URL (what the History
    // page shows), so that is the row this delivery created.
    let deadline = Instant::now() + Duration::from_secs(60);
    let task_id = loop {
        let list: serde_json::Value = client
            .get(format!("{}/api/v1/reviews?per_page=100", base))
            .send()
            .await
            .expect("failed to GET /api/v1/reviews")
            .json()
            .await
            .expect("reviews list body is not JSON");
        let item = list["items"]
            .as_array()
            .expect("reviews.items is an array")
            .iter()
            .find(|i| {
                i["gitlabMrUrl"].as_str() == Some(payload_url) || i["gitlab_mr_url"].as_str() == Some(payload_url)
            });
        match item.map(|i| {
            (
                i["id"].as_str().unwrap_or("").to_string(),
                i["status"].as_str().unwrap_or(""),
            )
        }) {
            Some((id, "completed" | "failed")) => break id,
            _ if Instant::now() > deadline => {
                panic!("the webhook review did not settle within 60s: {list}")
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    };

    let resp = client
        .post(format!("{}/api/v1/reviews/{}/rerun", base, task_id))
        .send()
        .await
        .expect("failed to POST rerun");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "a webhook-created review must be re-runnable (RENG-88), got {json}"
    );
    let rerun_id = json["task_id"].as_str().expect("rerun returns the new task id");
    assert_ne!(rerun_id, task_id, "rerun must create a fresh task id");

    // …and the replay really runs: it settles on its own, against the same MR
    // the webhook review fetched. (`failed` is this test's expected terminal
    // state — the configured LLM provider is the unreachable discard address —
    // the point is that the stored request drives a working review at all.)
    let settled = poll_until_settled(&base, &client, rerun_id).await;
    assert_eq!(
        settled["status"].as_str(),
        Some("failed"),
        "the replayed review must run (and fail on the test's unreachable LLM), got {settled:?}"
    );
    assert_eq!(
        settled["gitlabMrUrl"].as_str(),
        Some(review_url.as_str()),
        "the replayed review must target the same MR the webhook review fetched, got {settled:?}"
    );
}

#[tokio::test]
async fn webhook_url_ssrf_validation_rejects_at_enqueue_time() {
    let port = find_free_port();
    // Deliberately no LLM_CONFIG: webhook SSRF validation (400) is a security
    // check and must run BEFORE the no-usable-LLM policy gate (422) — this
    // test locks in that precedence.
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    let cases = [
        "https://169.254.169.254/latest/meta-data", // cloud metadata
        "http://169.254.169.254/hook",
        "http://0.0.0.0:9000/hook",
        "http://[fe80::1]/hook",
        "http://93.184.216.34/hook", // http to a public host
        "ftp://example.com/hook",
        "file:///etc/passwd",
        "not-a-url",
    ];
    for webhook in cases {
        let resp = client
            .post(format!("{}/api/v1/reviews", base))
            .json(&serde_json::json!({
                "source": {"type": "static_diff", "diff": "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-f()\n+g()\n"},
                "webhook": webhook,
            }))
            .send()
            .await
            .expect("failed to POST /api/v1/reviews");
        let (status, json) = response_parts(resp).await;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "webhook {webhook} must be rejected at enqueue time with 400, got {json}"
        );
        let error = json["error"].as_str().unwrap();
        assert!(
            error.starts_with("invalid webhook url:"),
            "error must carry the documented prefix: {error}"
        );
    }
}

#[tokio::test]
async fn webhook_loopback_http_is_accepted_and_delivered() {
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&hook)
        .await;

    let port = find_free_port();
    // The unreachable LLM provider passes the no-usable-LLM enqueue gate, so
    // the task is accepted, fails fast on the unreachable MR URL, and the
    // failure callback is delivered to the loopback webhook.
    let llm_config_env = unreachable_llm_config_env();
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", ""), ("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    let mut body = gitlab_mr_body();
    body["webhook"] = serde_json::json!(format!("{}/hook", hook.uri()));
    let resp = client
        .post(format!("{}/api/v1/reviews", base))
        .header("X-Gitlab-Token", "glpat-header-token")
        .json(&body)
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    let (status, json) = response_parts(resp).await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "a loopback http webhook must pass enqueue validation, got {json}"
    );
    let task_id = json["task_id"].as_str().unwrap().to_string();

    // The task fails fast (invalid MR URL), then the failure callback is
    // delivered to the loopback webhook.
    let settled = poll_until_settled(&base, &client, &task_id).await;
    assert_eq!(settled["status"].as_str(), Some("failed"));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !hook.received_requests().await.unwrap().is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "webhook callback was not delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let requests = hook.received_requests().await.unwrap();
    let callback: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(callback["task_id"], task_id);
    assert_eq!(callback["status"], "failed");
}

/// RENG-29 regression: there is no `/reviews/history` route — the paginated
/// history list is `GET /api/v1/reviews` (docs/rest-api.md). A request to the
/// non-existent sub-path is captured by `/{task_id}` and rejected at
/// path-parameter validation with 400, which is the documented contract: the
/// error names `task_id`, so a mistyped path surfaces as a parameter error,
/// never as a routed "history" handler.
#[tokio::test]
async fn reviews_history_subpath_is_not_a_route() {
    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
    wait_for_server(port).await;
    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    // The real history list endpoint answers 200 with the paginated envelope.
    let resp = client
        .get(format!("{}/api/v1/reviews", base))
        .send()
        .await
        .expect("failed to GET /api/v1/reviews");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "the history list endpoint is GET /api/v1/reviews"
    );
    let json: serde_json::Value = resp.json().await.expect("list body is JSON");
    for key in ["items", "total", "page", "per_page"] {
        assert!(json.get(key).is_some(), "list envelope must carry {key}: {json}");
    }

    // The mistaken path hits `/{task_id}`: `history` is not a UUID, so path
    // validation fails with 400 before any handler runs.
    let resp = client
        .get(format!("{}/api/v1/reviews/history", base))
        .send()
        .await
        .expect("failed to GET /api/v1/reviews/history");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "a non-UUID task_id must fail path validation with 400"
    );
    let body = resp.text().await.expect("400 body");
    assert!(
        body.contains("task_id"),
        "the 400 must name the failing path parameter: {body}"
    );
}
