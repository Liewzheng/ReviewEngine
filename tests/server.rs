#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn bin_path() -> String {
    std::env::var("CARGO_BIN_EXE_review-engine").unwrap_or_else(|_| "target/debug/review-engine".to_string())
}

fn find_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind to find free port");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

struct ServerGuard {
    child: Child,
    _temp_dir: tempfile::TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(port: u16) -> ServerGuard {
    spawn_server_inner(port, None)
}

/// Spawn `serve` with `REVIEW_API_TOKEN` set when `token` is `Some`, enabling
/// the API auth middleware (a token is honored on loopback binds too).
fn spawn_server_with_token(port: u16, token: &str) -> ServerGuard {
    spawn_server_inner(port, Some(token))
}

fn spawn_server_inner(port: u16, token: Option<&str>) -> ServerGuard {
    spawn_server_inner_with_env(port, token, &[])
}

fn spawn_server_inner_with_env(port: u16, token: Option<&str>, extra_env: &[(&str, &str)]) -> ServerGuard {
    spawn_server_with_bin(&bin_path(), port, token, extra_env)
}

/// Spawn `serve` from an explicit binary path (used by the symlink-layout
/// upgrade test, which launches the server through a `reng` symlink).
fn spawn_server_with_bin(bin: &str, port: u16, token: Option<&str>, extra_env: &[(&str, &str)]) -> ServerGuard {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut cmd = Command::new(bin);
    cmd.arg("serve")
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env("HOME", temp_dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(token) = token {
        cmd.env("REVIEW_API_TOKEN", token);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let child = cmd.spawn().expect("failed to spawn review-engine serve");
    ServerGuard {
        child,
        _temp_dir: temp_dir,
    }
}

async fn wait_for_server(port: u16) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client
            .get(format!("http://127.0.0.1:{}/health", port))
            .timeout(Duration::from_millis(200))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => break,
            _ if Instant::now() > deadline => panic!("server did not start within 10 seconds"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

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

// ─── LLM Provider CRUD ────────────────────────────────────────────

/// The frontend sends `apiBaseUrl`/`defaultModel`; the backend must map them
/// onto `api_base`/`model` via serde aliases. The primary camelCase names
/// (`apiBase`/`model`) must keep working as well.
#[test]
fn provider_requests_accept_frontend_field_aliases() {
    use review_engine::server::api::llm::{AddProviderRequest, UpdateProviderRequest};

    let add: AddProviderRequest = serde_json::from_value(serde_json::json!({
        "provider": "openai",
        "apiKey": "sk-test",
        "apiBaseUrl": "https://llm.example.test/v1",
        "defaultModel": "gpt-4o-test",
        "maxTokens": 8192,
        "temperature": 0.3,
    }))
    .expect("AddProviderRequest should accept frontend field names");
    assert_eq!(add.provider, "openai");
    assert_eq!(add.api_key, "sk-test");
    assert_eq!(add.api_base, "https://llm.example.test/v1");
    assert_eq!(add.model, "gpt-4o-test");
    assert_eq!(add.max_tokens, 8192);
    assert!((add.temperature - 0.3).abs() < f32::EPSILON);

    let add_primary: AddProviderRequest = serde_json::from_value(serde_json::json!({
        "provider": "openai",
        "apiKey": "sk-test",
        "apiBase": "https://primary.example.test/v1",
        "model": "gpt-4o-primary",
    }))
    .expect("AddProviderRequest should keep its primary camelCase names");
    assert_eq!(add_primary.api_base, "https://primary.example.test/v1");
    assert_eq!(add_primary.model, "gpt-4o-primary");

    let update: UpdateProviderRequest = serde_json::from_value(serde_json::json!({
        "apiBaseUrl": "https://update.example.test/v1",
        "defaultModel": "gpt-4o-update",
    }))
    .expect("UpdateProviderRequest should accept frontend field names");
    assert_eq!(update.api_base, "https://update.example.test/v1");
    assert_eq!(update.model, "gpt-4o-update");
}

#[tokio::test]
async fn add_provider_accepts_frontend_field_names() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/v1/llm/providers", port))
        .json(&serde_json::json!({
            "provider": "openai",
            "apiKey": "sk-test",
            "apiBaseUrl": "https://llm.example.test/v1",
            "defaultModel": "gpt-4o-test",
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
    let body: serde_json::Value = resp.json().await.expect("POST provider body is not JSON");
    // `defaultModel` must land in `model` — without the alias this would be "".
    assert_eq!(body["model"], "gpt-4o-test");
    assert_eq!(body["configured"], true);

    // The provider must be listed afterwards and marked as configured.
    let resp = reqwest::get(format!("http://127.0.0.1:{}/api/v1/llm/providers", port))
        .await
        .expect("failed to GET /api/v1/llm/providers");
    assert!(resp.status().is_success(), "GET providers returned {}", resp.status());
    let body: serde_json::Value = resp.json().await.expect("GET providers body is not JSON");
    let items = body["items"].as_array().expect("items is not an array");
    let added = items
        .iter()
        .find(|item| item["name"] == "openai")
        .expect("added provider missing from GET /providers");
    assert_eq!(added["configured"], true);
    // GET /providers must echo the editable config so the UI can prefill the
    // edit form instead of falling back to fake defaults.
    assert_eq!(added["apiBaseUrl"], "https://llm.example.test/v1");
    assert_eq!(added["defaultModel"], "gpt-4o-test");
    assert_eq!(added["maxTokens"], 4096);
    // temperature is stored as f32, so it round-trips through JSON as
    // 0.699999988079071; compare with a tolerance instead of exact equality.
    let temperature = added["temperature"].as_f64().expect("temperature is not a number");
    assert!(
        (temperature - 0.7).abs() < 1e-6,
        "temperature should round-trip to 0.7, got {temperature}"
    );
    // The API key must never be returned.
    assert!(added.get("apiKey").is_none(), "GET /providers leaks apiKey");
    assert!(added.get("api_key").is_none(), "GET /providers leaks api_key");
}

#[tokio::test]
async fn add_provider_missing_provider_field_returns_400_json() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/v1/llm/providers", port))
        .json(&serde_json::json!({
            "apiKey": "sk-test",
        }))
        .send()
        .await
        .expect("failed to POST /api/v1/llm/providers");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "expected 400 for a body missing `provider`, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("400 response body should be JSON");
    let error = body["error"].as_str().expect("400 body must contain an `error` string");
    assert!(
        error.contains("provider"),
        "error message should mention the missing field: {}",
        error
    );
}

#[tokio::test]
async fn update_provider_accepts_frontend_field_names() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{}/api/v1/llm/providers", port))
        .json(&serde_json::json!({
            "provider": "openai",
            "apiKey": "sk-test",
            "defaultModel": "gpt-4o-test",
        }))
        .send()
        .await
        .expect("failed to POST /api/v1/llm/providers");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = resp.json().await.expect("POST provider body is not JSON");
    let id = body["id"].as_str().expect("POST response missing `id`").to_string();

    let resp = client
        .put(format!("http://127.0.0.1:{}/api/v1/llm/providers/{}", port, id))
        .json(&serde_json::json!({
            "apiBaseUrl": "https://llm-update.example.test/v1",
            "defaultModel": "gpt-4o-updated",
        }))
        .send()
        .await
        .expect("failed to PUT /api/v1/llm/providers/{id}");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "PUT /api/v1/llm/providers/{} returned {}",
        id,
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("PUT provider body is not JSON");
    assert_eq!(body["status"], "updated");
    // `defaultModel` must land in `model` — without the alias it would stay "gpt-4o-test".
    assert_eq!(body["model"], "gpt-4o-updated");
}

/// Regression test: `GET /config` maps the primary provider into BOTH the
/// legacy `llm.*` fields and `llm.providers`, so when the UI saves the config
/// back unchanged, `PUT /config` used to rebuild `llm_configs` from both
/// sources and appended one more copy of the primary on every save
/// (`openai-0` + `openai-1` duplicates in `GET /llm/providers`). The PUT must
/// skip providers entries that duplicate the primary, keeping saves idempotent.
#[tokio::test]
async fn put_config_round_trip_does_not_duplicate_primary_provider() {
    // Seed a user-level config with one primary openai provider; the spawned
    // server runs with HOME pointing at this temp dir, so startup maps it into
    // the legacy fields and `llm.providers` — exactly what the UI round-trips.
    let temp_home = tempfile::tempdir().expect("failed to create temp home");
    let user_config_dir = temp_home.path().join(".config").join("review-engine");
    std::fs::create_dir_all(&user_config_dir).expect("failed to create user config dir");
    std::fs::write(
        user_config_dir.join(".code-audit-config.toml"),
        "[[llm]]\nprovider = \"openai\"\nmodel = \"gpt-4o\"\napi_key = \"sk-primary\"\n",
    )
    .expect("failed to write user config");

    let port = find_free_port();
    let child = Command::new(bin_path())
        .arg("serve")
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env("HOME", temp_home.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn review-engine serve");
    let _guard = ServerGuard {
        child,
        _temp_dir: temp_home,
    };
    wait_for_server(port).await;

    let base = format!("http://127.0.0.1:{}", port);

    // Sanity: exactly one provider configured before any save.
    let resp = reqwest::get(format!("{}/api/v1/llm/providers", base))
        .await
        .expect("failed to GET /api/v1/llm/providers");
    let body: serde_json::Value = resp.json().await.expect("GET providers body is not JSON");
    let items = body["items"].as_array().expect("items is not an array");
    assert_eq!(items.len(), 1, "expected exactly one seeded provider, got {:?}", items);

    // GET /config exposes the primary in both the legacy fields and providers.
    let resp = reqwest::get(format!("{}/api/v1/config", base))
        .await
        .expect("failed to GET /api/v1/config");
    let config: serde_json::Value = resp.json().await.expect("GET /config body is not JSON");
    assert_eq!(config["llm"]["openaiApiKey"], "sk-primary");
    let providers = config["llm"]["providers"]
        .as_array()
        .expect("llm.providers is not an array");
    assert_eq!(
        providers.len(),
        1,
        "GET /config should map the primary into llm.providers"
    );
    assert_eq!(providers[0]["provider"], "openai");

    // Save the config back unchanged, twice — each save must keep the provider
    // list at exactly one entry (no duplication, idempotent).
    let client = reqwest::Client::new();
    for round in 1..=2 {
        let resp = client
            .put(format!("{}/api/v1/config", base))
            .json(&config)
            .send()
            .await
            .expect("failed to PUT /api/v1/config");
        assert!(
            resp.status().is_success(),
            "PUT /api/v1/config round {} returned {}",
            round,
            resp.status()
        );

        let resp = reqwest::get(format!("{}/api/v1/llm/providers", base))
            .await
            .expect("failed to GET /api/v1/llm/providers");
        let body: serde_json::Value = resp.json().await.expect("GET providers body is not JSON");
        let items = body["items"].as_array().expect("items is not an array");
        assert_eq!(
            items.len(),
            1,
            "save round {} duplicated the primary provider: {:?}",
            round,
            items
        );
        assert_eq!(items[0]["id"], "openai-0");
        assert_eq!(items[0]["name"], "openai");
    }
}

// ─── Repo Scan ────────────────────────────────────────────────────

#[tokio::test]
async fn repo_scan_rejects_invalid_paths() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
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

    let resp = reqwest::get(format!(
        "http://127.0.0.1:{}/api/v1/repo-scan/{}",
        port,
        uuid::Uuid::new_v4()
    ))
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

    let client = reqwest::Client::new();
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

/// Unit 7: a review whose every expert fails (no valid LLM configured) must be
/// recorded as `failed` with a descriptive error, not `completed` with an empty
/// report set. The spawned server runs with a temp HOME, so the default config's
/// 11 LLM-backed experts all fail and `run_experts` bails.
#[tokio::test]
async fn review_zero_output_with_expert_failures_marks_failed() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
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
fn mock_llm_provider(api_base: &str) -> serde_json::Value {
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

    let client = reqwest::Client::new();
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

    let client = reqwest::Client::new();
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

    let client = reqwest::Client::new();
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

    let client = reqwest::Client::new();
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

// ─── API Auth (P1: type-mismatch fix) ─────────────────────────────
//
// Regression tests for the auth middleware. `api::routes` stores the shared
// auth config in request extensions as `Arc<AuthConfig>`; the middleware must
// read it back with the same type. When it read plain `AuthConfig` it always
// got `None` and silently allowed every request, so a token-less server
// exposed /api/v1 to the world. These tests spawn real servers both with and
// without `REVIEW_API_TOKEN` and assert the gate on /api/v1/system/version.

const API_TOKEN: &str = "test-token-123";

async fn get_version(port: u16, auth_header: Option<&str>, api_key: Option<&str>) -> reqwest::Response {
    let mut req = reqwest::Client::new().get(format!("http://127.0.0.1:{}/api/v1/system/version", port));
    if let Some(v) = auth_header {
        req = req.header("Authorization", v);
    }
    if let Some(k) = api_key {
        req = req.header("X-API-Key", k);
    }
    req.send().await.expect("failed to GET /api/v1/system/version")
}

/// Auth enabled: no token and a wrong token must both be rejected with
/// 401 + `{"error":"unauthorized"}` JSON.
#[tokio::test]
async fn api_auth_enabled_rejects_missing_and_wrong_token() {
    let port = find_free_port();
    let _guard = spawn_server_with_token(port, API_TOKEN);
    wait_for_server(port).await;

    for (label, req) in [
        ("no token", get_version(port, None, None).await),
        (
            "wrong bearer",
            get_version(port, Some("Bearer wrong-token"), None).await,
        ),
        ("wrong api key", get_version(port, None, Some("wrong-key")).await),
    ] {
        assert_eq!(
            req.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "expected 401 for {label}, got {}",
            req.status()
        );
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("application/json"),
            "401 for {label} must be JSON, got content-type {content_type}"
        );
        let body: serde_json::Value = req.json().await.expect("401 body must be JSON");
        assert_eq!(
            body,
            serde_json::json!({"error": "unauthorized"}),
            "unexpected 401 body for {label}"
        );
    }
}

/// Auth enabled: a correct Bearer token and a correct X-API-Key must both
/// pass through to the endpoint (200).
#[tokio::test]
async fn api_auth_enabled_accepts_valid_bearer_and_api_key() {
    let port = find_free_port();
    let _guard = spawn_server_with_token(port, API_TOKEN);
    wait_for_server(port).await;

    let bearer = format!("Bearer {API_TOKEN}");
    let resp = get_version(port, Some(&bearer), None).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "valid Bearer token returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("version body must be JSON");
    assert!(body["version"].is_string(), "version body missing `version`: {body}");

    let resp = get_version(port, None, Some(API_TOKEN)).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "valid X-API-Key returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("version body must be JSON");
    assert!(body["version"].is_string(), "version body missing `version`: {body}");
}

/// Auth disabled (loopback bind, no token): the API must stay open — no
/// token → 200. The auth middleware must not be mounted at all.
#[tokio::test]
async fn api_auth_disabled_allows_without_token() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let resp = get_version(port, None, None).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "auth-disabled server returned {} for an unauthenticated request",
        resp.status()
    );
}

// ─── Self-upgrade API (U5) ───────────────────────────────────────

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a small release tar.gz whose `bin/review-engine` is a harmless shell
/// script, so the upgrade pipeline (download → verify → extract → replace →
/// smoke) can run end-to-end against a temp install dir.
fn build_fake_release_tar() -> Vec<u8> {
    const SCRIPT: &str = "#!/bin/sh\necho smoke-ok\n";
    let mut out = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path("bin/review-engine").expect("set tar path");
        header.set_size(SCRIPT.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, SCRIPT.as_bytes()).expect("append tar entry");
        let encoder = builder.into_inner().expect("into_inner");
        encoder.finish().expect("finish gzip");
    }
    out
}

/// `GET /upgrade/check` returns the full contract and serves the second call
/// from the 1h server-side cache (wiremock counts exactly one GitHub request).
#[tokio::test]
async fn upgrade_check_caches_github_result() {
    let mock = MockServer::start().await;
    let api_base = mock.uri();
    let spec = review_engine::upgrade::platform::current_asset_spec().expect("test platform has an asset spec");
    let asset_name = spec.asset_name("review-engine");
    let checksum_name = spec.checksum_name("review-engine");

    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                {"name": asset_name, "browser_download_url": format!("{api_base}/asset"), "size": 100},
                {"name": checksum_name, "browser_download_url": format!("{api_base}/checksum"), "size": 72}
            ]
        })))
        .mount(&mock)
        .await;

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &api_base),
            ("REVIEW_UPGRADE_METHOD", "binary"),
        ],
    );
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/api/v1/system/upgrade/check", port);

    let resp = client.get(&url).send().await.expect("GET check");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "check must be 200");
    let body: serde_json::Value = resp.json().await.expect("check body is JSON");
    assert_eq!(body["currentVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["latestVersion"], "9.9.9");
    assert_eq!(body["updateAvailable"], serde_json::Value::Bool(true));
    assert_eq!(body["installMethod"], "binary");
    assert_eq!(body["platformAssetAvailable"], serde_json::Value::Bool(true));
    assert_eq!(
        body["releaseUrl"],
        "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9"
    );
    assert_eq!(body["upgradeHint"], "reng upgrade");
    let cached_at = body["cachedAt"].as_str().expect("cachedAt is a string");
    assert!(!cached_at.is_empty(), "cachedAt must be non-empty");

    // Second call: identical body, served from cache — no new GitHub request.
    let resp2 = client.get(&url).send().await.expect("GET check #2");
    assert_eq!(resp2.status(), reqwest::StatusCode::OK);
    let body2: serde_json::Value = resp2.json().await.expect("check body is JSON");
    assert_eq!(body2, body, "cached response must match the first");

    let requests = mock.received_requests().await.expect("received requests");
    let latest_hits = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/releases/latest"))
        .count();
    assert_eq!(
        latest_hits, 1,
        "releases/latest must be hit exactly once across two checks"
    );
}

/// `GET /upgrade/check` must surface an upstream failure as a 502 +
/// `{"error": "check failed: ..."}` — and must NOT write the failed result into
/// the 1h server cache. The GitHub client rejects a non-2xx release endpoint
/// as `UpgradeError::Api`; `refresh_check` only stores `Ok(check)`, so a
/// subsequent check must retry the upstream instead of serving a stale failure.
/// The instance runs with a fresh temp HOME (per-test `ServerGuard`), so no
/// cached key leaks across tests either.
#[tokio::test]
async fn upgrade_check_upstream_failure_returns_502_and_is_not_cached() {
    let mock = MockServer::start().await;
    // Non-2xx from the release endpoint — the upgrade service must map it to a
    // clear check failure, not a 200 with garbage or a hang.
    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&mock)
        .await;

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &mock.uri()),
            ("REVIEW_UPGRADE_METHOD", "binary"),
        ],
    );
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{}/api/v1/system/upgrade/check", port);

    // Call twice: both must be 502 with a non-empty error. The second call
    // hitting the upstream again proves the failure was not cached.
    for attempt in ["first", "second"] {
        let resp = client.get(&url).send().await.expect("GET check");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_GATEWAY,
            "{attempt} check against a 500 upstream must be 502, got {}",
            resp.status()
        );
        let body: serde_json::Value = resp.json().await.expect("502 body is JSON");
        let error = body["error"].as_str().expect("502 body must carry an error string");
        assert!(!error.is_empty(), "{attempt} error must be non-empty");
        assert!(
            error.contains("check failed: "),
            "{attempt} error should carry the check-failure framing, got: {error}"
        );
        assert!(
            error.contains("500"),
            "{attempt} error should surface the upstream status, got: {error}"
        );
    }

    let requests = mock.received_requests().await.expect("received requests");
    let latest_hits = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/releases/latest"))
        .count();
    assert_eq!(
        latest_hits, 2,
        "each failed check must hit the upstream again (failed results must not be cached)"
    );
}

/// `POST /upgrade` for a binary install: first call 202 + starts the pipeline,
/// second call 409 (single-flight). The pipeline runs end-to-end against a
/// wiremock-served asset and lands the replaced binary in a temp install dir.
#[tokio::test]
async fn upgrade_post_binary_single_flight_and_pipeline() {
    let mock = MockServer::start().await;
    let api_base = mock.uri();
    let spec = review_engine::upgrade::platform::current_asset_spec().expect("test platform has an asset spec");
    let asset_name = spec.asset_name("review-engine");
    let checksum_name = spec.checksum_name("review-engine");
    let tar_bytes = build_fake_release_tar();
    let sha = review_engine::upgrade::verify::data_sha256_hex(&tar_bytes);
    let checksum_text = format!("{sha}  {asset_name}");

    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                {"name": asset_name, "browser_download_url": format!("{api_base}/asset"), "size": tar_bytes.len()},
                {"name": checksum_name, "browser_download_url": format!("{api_base}/checksum"), "size": checksum_text.len()}
            ]
        })))
        .mount(&mock)
        .await;
    // Delayed asset download keeps the job in-flight for the 409 assertion.
    Mock::given(method("GET"))
        .and(path("/asset"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(tar_bytes.clone())
                .set_delay(Duration::from_secs(3)),
        )
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/checksum"))
        .respond_with(ResponseTemplate::new(200).set_body_string(checksum_text))
        .mount(&mock)
        .await;

    let install_dir = tempfile::tempdir().expect("temp install dir");
    let install_dir_str = install_dir.path().to_str().expect("utf8 install dir").to_string();
    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &api_base),
            ("REVIEW_UPGRADE_METHOD", "binary"),
            ("REVIEW_UPGRADE_INSTALL_DIR", &install_dir_str),
        ],
    );
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);

    let first = client.post(&base).send().await.expect("POST upgrade");
    assert_eq!(first.status(), reqwest::StatusCode::ACCEPTED, "first POST must be 202");
    let first_body: serde_json::Value = first.json().await.expect("202 body is JSON");
    assert_eq!(first_body["status"], "started");
    assert_eq!(first_body["targetVersion"], "9.9.9");

    let second = client.post(&base).send().await.expect("POST upgrade #2");
    assert_eq!(
        second.status(),
        reqwest::StatusCode::CONFLICT,
        "second POST must be 409"
    );
    let second_body: serde_json::Value = second.json().await.expect("409 body is JSON");
    assert!(second_body["error"].is_string(), "409 must carry an error message");

    // Wait for the pipeline to finish; assert done + replaced binary.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status_url = format!("{base}/status");
    loop {
        let status: serde_json::Value = client
            .get(&status_url)
            .send()
            .await
            .expect("GET status")
            .json()
            .await
            .expect("status is JSON");
        if status["state"] == "done" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "upgrade did not finish, last status: {status}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let final_status: serde_json::Value = client.get(&status_url).send().await.unwrap().json().await.unwrap();
    assert_eq!(final_status["targetVersion"], "9.9.9");
    assert!(
        install_dir.path().join("review-engine").exists(),
        "replaced binary must exist in install dir"
    );
}

/// `POST /upgrade` inside a container returns `notSupported` + host-side
/// instructions, and `/status` reflects the `notSupported` state.
#[tokio::test]
async fn upgrade_docker_returns_not_supported() {
    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("REVIEW_UPGRADE_METHOD", "docker")]);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);

    let resp = client.post(&base).send().await.expect("POST upgrade");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "docker POST should be 200");
    let body: serde_json::Value = resp.json().await.expect("body is JSON");
    assert_eq!(body["status"], "notSupported");
    assert_eq!(body["instructions"], "git pull && docker compose up -d --build");
    assert!(body["note"].is_string(), "note must be present");

    let status: serde_json::Value = client
        .get(format!("{base}/status"))
        .send()
        .await
        .expect("GET status")
        .json()
        .await
        .expect("status is JSON");
    assert_eq!(status["state"], "notSupported");
}

/// `POST /upgrade` for brew/cargo installs is refused (400) with the correct
/// manual upgrade command — the API must never mutate package-managed files.
#[tokio::test]
async fn upgrade_brew_and_cargo_reject_with_hint() {
    for (method, hint_fragment) in [
        ("brew", "brew upgrade review-engine"),
        ("cargo", "cargo install review-engine"),
    ] {
        let port = find_free_port();
        let _guard = spawn_server_inner_with_env(port, None, &[("REVIEW_UPGRADE_METHOD", method)]);
        wait_for_server(port).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/api/v1/system/upgrade", port))
            .send()
            .await
            .expect("POST upgrade");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "method {method} must be 400"
        );
        let body: serde_json::Value = resp.json().await.expect("body is JSON");
        let hint = body["upgradeHint"].as_str().unwrap_or("");
        assert!(
            hint.contains(hint_fragment),
            "hint {hint:?} must contain {hint_fragment:?}"
        );
    }
}

/// Auth enabled: all three upgrade endpoints reject unauthenticated requests
/// with 401 + `{"error":"unauthorized"}` (they live under the /api/v1 auth
/// middleware, same as every other API route).
#[tokio::test]
async fn upgrade_endpoints_require_auth_when_enabled() {
    let port = find_free_port();
    let _guard = spawn_server_with_token(port, API_TOKEN);
    wait_for_server(port).await;

    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);
    let client = reqwest::Client::new();
    for (label, resp) in [
        ("check", client.get(format!("{base}/check")).send().await.unwrap()),
        ("status", client.get(format!("{base}/status")).send().await.unwrap()),
        ("post", client.post(&base).send().await.unwrap()),
    ] {
        assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED, "{label} must be 401");
        let body: serde_json::Value = resp.json().await.expect("401 body is JSON");
        assert_eq!(body, serde_json::json!({"error": "unauthorized"}), "{label} 401 body");
    }
}

/// B1: when the server is launched through a `reng` symlink, a web upgrade
/// must replace the *real* binary and leave the symlink intact — not replace
/// the symlink itself (macOS `current_exe()` returns the symlink invocation
/// path; `resolve_install_dir`/`current_exe_name` canonicalize it).
#[cfg(unix)]
#[tokio::test]
async fn upgrade_via_symlink_replaces_real_binary_and_preserves_link() {
    use std::os::unix::fs::symlink;

    let mock = MockServer::start().await;
    let api_base = mock.uri();
    let spec = review_engine::upgrade::platform::current_asset_spec().expect("test platform has an asset spec");
    let asset_name = spec.asset_name("review-engine");
    let checksum_name = spec.checksum_name("review-engine");
    let tar_bytes = build_fake_release_tar();
    let sha = review_engine::upgrade::verify::data_sha256_hex(&tar_bytes);
    let checksum_text = format!("{sha}  {asset_name}");

    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                {"name": asset_name, "browser_download_url": format!("{api_base}/asset"), "size": tar_bytes.len()},
                {"name": checksum_name, "browser_download_url": format!("{api_base}/checksum"), "size": checksum_text.len()}
            ]
        })))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/asset"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(tar_bytes.clone()))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/checksum"))
        .respond_with(ResponseTemplate::new(200).set_body_string(checksum_text))
        .mount(&mock)
        .await;

    // Symlink layout: `reng` → `review-engine`, both in the same dir. The
    // server is launched through the symlink; NO REVIEW_UPGRADE_INSTALL_DIR
    // override — the install dir must derive from the canonicalized exe.
    let layout = tempfile::tempdir().expect("layout tempdir");
    let real_bin = layout.path().join("review-engine");
    std::fs::copy(bin_path(), &real_bin).expect("copy compiled binary into layout");
    let link = layout.path().join("reng");
    symlink("review-engine", &link).expect("create reng symlink");

    let link_str = link.to_str().expect("utf8 symlink path").to_string();
    let port = find_free_port();
    let _guard = spawn_server_with_bin(
        &link_str,
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &api_base),
            ("REVIEW_UPGRADE_METHOD", "binary"),
        ],
    );
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);
    let resp = client.post(&base).send().await.expect("POST upgrade");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "POST via symlink should be accepted"
    );

    let deadline = Instant::now() + Duration::from_secs(15);
    let status_url = format!("{base}/status");
    loop {
        let status: serde_json::Value = client
            .get(&status_url)
            .send()
            .await
            .expect("GET status")
            .json()
            .await
            .expect("status is JSON");
        if status["state"] == "done" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "upgrade did not finish, last status: {status}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // The real binary was replaced with the new artifact…
    assert_eq!(
        std::fs::read(&real_bin).expect("read replaced binary"),
        b"#!/bin/sh\necho smoke-ok\n",
        "real binary must be replaced by the new artifact"
    );
    // …and the symlink survives, still pointing at the same name.
    let meta = std::fs::symlink_metadata(&link).expect("symlink metadata");
    assert!(meta.file_type().is_symlink(), "reng symlink must be preserved");
    let target = std::fs::read_link(&link).expect("readlink reng");
    assert_eq!(
        target,
        std::path::Path::new("review-engine"),
        "reng must still point at review-engine"
    );
}

/// B2: `POST /upgrade` rejects cross-site browser-triggered requests (Origin
/// authority ≠ Host → 403) while allowing same-origin and no-Origin clients;
/// GET endpoints are not origin-gated.
#[tokio::test]
async fn upgrade_post_origin_validation_three_states() {
    let mock = MockServer::start().await;
    let api_base = mock.uri();
    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": []
        })))
        .mount(&mock)
        .await;

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &api_base),
            ("REVIEW_UPGRADE_METHOD", "docker"),
        ],
    );
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);

    // No Origin (curl / script / in-process) → allowed.
    let resp = client.post(&base).send().await.expect("POST no origin");
    assert_ne!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "no-Origin POST must not be 403"
    );

    // Same-origin (Origin authority == Host) → allowed.
    let same = format!("http://127.0.0.1:{port}");
    let resp = client
        .post(&base)
        .header("Origin", &same)
        .send()
        .await
        .expect("POST same origin");
    assert_ne!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "same-origin POST must not be 403"
    );

    // Cross-origin (DNS-rebinding / CSRF) → 403.
    let resp = client
        .post(&base)
        .header("Origin", "http://evil.example")
        .send()
        .await
        .expect("POST cross origin");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "cross-origin POST must be 403"
    );
    let body: serde_json::Value = resp.json().await.expect("403 body is JSON");
    assert_eq!(body["error"], "cross-origin upgrade rejected");

    // GET endpoints are NOT origin-gated (read-only, no state change).
    for (label, resp) in [
        (
            "status",
            client
                .get(format!("{base}/status"))
                .header("Origin", "http://evil.example")
                .send()
                .await
                .expect("GET status cross origin"),
        ),
        (
            "check",
            client
                .get(format!("{base}/check"))
                .header("Origin", "http://evil.example")
                .send()
                .await
                .expect("GET check cross origin"),
        ),
    ] {
        assert_ne!(
            resp.status(),
            reqwest::StatusCode::FORBIDDEN,
            "{label} must not be origin-gated"
        );
    }
}
