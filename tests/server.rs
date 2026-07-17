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
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let child = Command::new(bin_path())
        .arg("serve")
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env("HOME", temp_dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn review-engine serve");
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
