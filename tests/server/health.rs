use super::{find_free_port, spawn_server, wait_for_server};

#[tokio::test]
async fn health_endpoint() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/health", port))
        .await
        .expect("failed to call /health");
    assert!(resp.status().is_success(), "/health returned {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("/health body is not JSON");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn health_ready_no_llm() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/health/ready", port))
        .await
        .expect("failed to call /health/ready");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "expected 503 from /health/ready without LLM config"
    );
    let body: serde_json::Value = resp.json().await.expect("/health/ready body is not JSON");
    assert_eq!(body["status"], "not ready");
}

#[tokio::test]
async fn metrics_endpoint() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let resp = reqwest::get(format!("http://127.0.0.1:{}/metrics", port))
        .await
        .expect("failed to call /metrics");
    assert!(resp.status().is_success(), "/metrics returned {}", resp.status());
    let body = resp.text().await.expect("/metrics body is not text");
    assert!(
        body.contains("review_engine") || body.contains("process_"),
        "metrics did not contain expected prefix: {}",
        body
    );
}
