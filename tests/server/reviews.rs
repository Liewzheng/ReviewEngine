//! Integration tests for the review submission contract (docs/rest-api.md §1):
//! GitLab credentials travel in the `X-Gitlab-Token` header (never the body,
//! never persisted), and webhook callback URLs are SSRF-validated at enqueue
//! time with a 400 on failure.
use std::time::{Duration, Instant};

use super::{bootstrap_authed_client, find_free_port, spawn_server_inner_with_env, wait_for_server, API_TOKEN};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A gitlab_mr body with a deliberately invalid MR URL: the enqueued task
/// fails fast at GitLab client construction (no network I/O), so these tests
/// exercise the HTTP contract only.
fn gitlab_mr_body() -> serde_json::Value {
    serde_json::json!({
        "source": {"type": "gitlab_mr", "url": "not-a-valid-url"}
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
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
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
    // GITLAB_TOKEN (or --gitlab-token) at startup.
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "glpat-env-token")]);
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
    // No server-side token: only the header can satisfy a rerun.
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
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

#[tokio::test]
async fn webhook_url_ssrf_validation_rejects_at_enqueue_time() {
    let port = find_free_port();
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
    let _guard = spawn_server_inner_with_env(port, None, &[("GITLAB_TOKEN", "")]);
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
