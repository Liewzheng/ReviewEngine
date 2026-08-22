use std::time::{Duration, Instant};

use super::{
    bootstrap_authed_client, find_free_port, spawn_server, spawn_server_inner_with_env, unreachable_llm_config_env,
    wait_for_server, API_TOKEN,
};

// ─── Repo Scan ────────────────────────────────────────────────────

#[tokio::test]
async fn repo_scan_rejects_invalid_paths() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let url = format!("http://127.0.0.1:{}/api/v1/repo-scan", port);

    // Nonexistent path → 400
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"path": "/nonexistent-repo-scan-path-xyz-12345"}))
        .send()
        .await
        .expect("failed to POST /api/v1/repo-scan");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "expected 400 for a nonexistent path, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("400 response body should be JSON");
    let error = body["error"].as_str().expect("400 body must contain an `error` string");
    assert!(error.contains("does not exist"), "unexpected error message: {}", error);

    // Parent-directory traversal → 400
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"path": "../somewhere"}))
        .send()
        .await
        .expect("failed to POST /api/v1/repo-scan");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "expected 400 for a path containing '..', got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("400 response body should be JSON");
    let error = body["error"].as_str().expect("400 body must contain an `error` string");
    assert!(error.contains(".."), "unexpected error message: {}", error);

    // A regular file (not a directory) → 400
    let file = tempfile::NamedTempFile::new().expect("failed to create temp file");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"path": file.path()}))
        .send()
        .await
        .expect("failed to POST /api/v1/repo-scan");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "expected 400 for a file path, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("400 response body should be JSON");
    let error = body["error"].as_str().expect("400 body must contain an `error` string");
    assert!(error.contains("not a directory"), "unexpected error message: {}", error);
}

#[tokio::test]
async fn repo_scan_unknown_task_returns_404() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/api/v1/repo-scan/{}",
            port,
            uuid::Uuid::new_v4()
        ))
        .send()
        .await
        .expect("failed to GET /api/v1/repo-scan/{task_id}");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "expected 404 for an unknown task_id, got {}",
        resp.status()
    );
}

/// End-to-end scan of a small local directory. The spawned server runs with
/// HOME pointing at a temp dir, so no LLM is configured and the scan takes
/// the static-only path (`run_local_repo_review`) — no external LLM calls.
#[tokio::test]
async fn repo_scan_completes_and_returns_health_score() {
    let repo = tempfile::tempdir().expect("failed to create temp repo dir");
    std::fs::write(repo.path().join("main.rs"), "fn main() { println!(\"hi\"); }\n").expect("write main.rs");
    std::fs::write(repo.path().join("README.md"), "# demo\n").expect("write README.md");
    std::fs::write(repo.path().join("lib.py"), "def f():\n    return 1\n").expect("write lib.py");

    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let url = format!("http://127.0.0.1:{}/api/v1/repo-scan", port);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"path": repo.path()}))
        .send()
        .await
        .expect("failed to POST /api/v1/repo-scan");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "POST /api/v1/repo-scan returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("POST response body is not JSON");
    let task_id = body["task_id"].as_str().expect("POST response missing task_id");
    assert_eq!(body["status"], "pending");

    let deadline = Instant::now() + Duration::from_secs(60);
    let final_body = loop {
        let resp = client
            .get(format!("{}/{}", url, task_id))
            .send()
            .await
            .expect("failed to GET /api/v1/repo-scan/{task_id}");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("GET response body is not JSON");
        match body["status"].as_str().unwrap_or("") {
            "completed" => break body,
            "failed" => panic!("repo scan failed: {:?}", body["error"]),
            _ if Instant::now() > deadline => panic!("repo scan did not complete within 60s: {:?}", body),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    };

    let health_score = &final_body["result"]["overview"]["health_score"];
    assert!(
        health_score.is_number(),
        "completed scan result must contain overview.health_score, got {:?}",
        final_body["result"]
    );
    let output: review_engine::actions::repo_review::RepoReviewOutput =
        serde_json::from_value(final_body["result"].clone()).expect("result is not a RepoReviewOutput");
    assert_eq!(
        output.overview.total_files, 3,
        "scan should have classified the 3 small files"
    );
    assert!(output.overview.total_experts > 0, "static experts should have run");
}

/// Unit 7: a review whose every expert fails must be recorded as `failed`
/// with a descriptive error, not `completed` with an empty report set. The
/// enqueue-time no-usable-LLM gate would reject a review with no usable LLM
/// at 422, so the fixture seeds a usable-but-unreachable provider (loopback
/// discard port 127.0.0.1:9): the gate passes, every LLM-backed expert then
/// fails on connection refused at runtime, and `run_experts` bails.
#[tokio::test]
async fn review_zero_output_with_expert_failures_marks_failed() {
    let port = find_free_port();
    let llm_config_env = unreachable_llm_config_env();
    let _guard = spawn_server_inner_with_env(port, None, &[("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let url = format!("http://127.0.0.1:{}/api/v1/reviews", port);
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "source": {"type": "static_diff", "diff": "diff --git a/a.rs b/a.rs\n@@ -1 +1 @@\n-f()\n+g()\n"}
        }))
        .send()
        .await
        .expect("failed to POST /api/v1/reviews");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "POST /api/v1/reviews returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("POST response body is not JSON");
    let task_id = body["task_id"].as_str().expect("POST response missing task_id");

    let deadline = Instant::now() + Duration::from_secs(30);
    let final_body = loop {
        let resp = client
            .get(format!("{}/{}", url, task_id))
            .send()
            .await
            .expect("failed to GET /api/v1/reviews/{task_id}");
        let body: serde_json::Value = resp.json().await.expect("GET response body is not JSON");
        match body["status"].as_str().unwrap_or("") {
            "failed" => break body,
            "completed" => panic!("empty-report review must not be recorded as completed"),
            _ if Instant::now() > deadline => panic!("review did not settle within 30s: {:?}", body),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    };
    assert!(
        final_body["result"].is_null(),
        "failed task must not carry a result, got {:?}",
        final_body["result"]
    );
    let error = final_body["error"].as_str().expect("failed task must carry an error");
    assert!(
        error.contains("all experts failed"),
        "error should summarize the expert failures, got: {}",
        error
    );
}
