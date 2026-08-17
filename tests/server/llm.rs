use std::process::Command;

use super::llm_configs::mock_llm_provider;
use super::{
    bin_path, bootstrap_authed_client, find_free_port, spawn_server, spawn_server_inner_with_env, wait_for_server,
    ServerGuard, API_TOKEN,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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
    let resp = client
        .get(format!("http://127.0.0.1:{}/api/v1/llm/providers", port))
        .send()
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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

/// Defect regression: `POST /api/v1/llm/providers/{id}/test` declared a
/// required `Json<serde_json::Value>` body that the handler never reads, so an
/// empty `application/json` body returned 400 "EOF while parsing" and a
/// bodyless request returned 415. Test Connection semantics need no body, so
/// the handler extracts none: an absent/empty/arbitrary body must reach the
/// handler and complete the connectivity check. (Note: `Option<Json<Value>>`
/// is NOT sufficient here — axum's `Json: OptionalFromRequest` only treats a
/// *missing* Content-Type as `None` and still errors on a present-but-empty
/// JSON body.)
#[tokio::test]
async fn test_provider_accepts_empty_body_and_no_content_type() {
    let mock = MockServer::start().await;
    // `test_llm_connectivity` GETs `{api_base}/models`; mount it so the happy
    // path succeeds without touching the real network.
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [{"id": "gpt-4o", "object": "model"}]
        })))
        .mount(&mock)
        .await;
    // Seed exactly one provider via `LLM_CONFIG`; its id derives as `openai-0`
    // (same `{provider}-{index}` scheme used by `GET /providers`).
    let llm_config_env = serde_json::json!([mock_llm_provider(&mock.uri())]).to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let url = format!("http://127.0.0.1:{}/api/v1/llm/providers/openai-0/test", port);

    // 1) Empty body WITH `Content-Type: application/json` — the exact reported
    //    repro that used to 400 with "EOF while parsing".
    let resp = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("")
        .send()
        .await
        .expect("failed to POST /test with empty JSON body");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "empty body with application/json must not 400, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("test response body is not JSON");
    assert_eq!(body["success"], true, "connectivity check must succeed, got {:?}", body);

    // 2) Empty body WITHOUT a Content-Type header — used to 415.
    let resp = client
        .post(&url)
        .body("")
        .send()
        .await
        .expect("failed to POST /test without content-type");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "empty body without content-type must not 415, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("test response body is not JSON");
    assert_eq!(body["success"], true, "connectivity check must succeed, got {:?}", body);

    // 3) A `{}` JSON body — previously the only passing shape — keeps working.
    let resp = client
        .post(&url)
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("failed to POST /test with {} body");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{{}} body must keep passing, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("test response body is not JSON");
    assert_eq!(body["success"], true, "connectivity check must succeed, got {:?}", body);
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}", port);

    // Sanity: exactly one provider configured before any save.
    let resp = client
        .get(format!("{}/api/v1/llm/providers", base))
        .send()
        .await
        .expect("failed to GET /api/v1/llm/providers");
    let body: serde_json::Value = resp.json().await.expect("GET providers body is not JSON");
    let items = body["items"].as_array().expect("items is not an array");
    assert_eq!(items.len(), 1, "expected exactly one seeded provider, got {:?}", items);

    // GET /config exposes the primary in both the legacy fields and providers —
    // but never the live key: it must come back masked.
    let resp = client
        .get(format!("{}/api/v1/config", base))
        .send()
        .await
        .expect("failed to GET /api/v1/config");
    let config: serde_json::Value = resp.json().await.expect("GET /config body is not JSON");
    assert_eq!(config["llm"]["openaiApiKey"], "***");
    assert_ne!(config["llm"]["openaiApiKey"], "sk-primary");
    let providers = config["llm"]["providers"]
        .as_array()
        .expect("llm.providers is not an array");
    assert_eq!(
        providers.len(),
        1,
        "GET /config should map the primary into llm.providers"
    );
    assert_eq!(providers[0]["provider"], "openai");
    assert_eq!(providers[0]["apiKey"], "***");
    assert_ne!(providers[0]["apiKey"], "sk-primary");

    // Save the config back unchanged, twice — each save must keep the provider
    // list at exactly one entry (no duplication, idempotent).
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

        let resp = client
            .get(format!("{}/api/v1/llm/providers", base))
            .send()
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

/// Security regression (P2): `GET /config` must never leak a live LLM key, and
/// a masked GET→PUT round trip must preserve the real key server-side. The
/// mock `/models` only answers when the `Authorization` header carries the
/// original key, so a round trip that corrupted the stored key to `***` (or a
/// blank key that overwrote it) would fail the connectivity check at the end.
#[tokio::test]
async fn get_config_masks_keys_and_round_trip_preserves_them() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer sk-roundtrip-secret",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
        .mount(&mock)
        .await;

    let llm_config_env = serde_json::json!([{
        "provider": "openai",
        "model": "gpt-4o",
        "api_key": "sk-roundtrip-secret",
        "api_base": mock.uri(),
        "max_tokens": 4096,
        "temperature": 0.7,
    }])
    .to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(port, None, &[("LLM_CONFIG", &llm_config_env)]);
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{port}");

    // GET /config must mask the key in both the legacy field and the providers.
    let resp = client
        .get(format!("{base}/api/v1/config"))
        .send()
        .await
        .expect("failed to GET /api/v1/config");
    assert!(resp.status().is_success(), "GET /config returned {}", resp.status());
    let config: serde_json::Value = resp.json().await.expect("GET /config body is not JSON");
    assert_eq!(config["llm"]["openaiApiKey"], "***");
    assert_ne!(config["llm"]["openaiApiKey"], "sk-roundtrip-secret");
    assert_eq!(config["llm"]["providers"][0]["apiKey"], "***");

    // Round-trip the masked config through PUT, twice (idempotent).
    for round in 1..=2 {
        let resp = client
            .put(format!("{base}/api/v1/config"))
            .json(&config)
            .send()
            .await
            .expect("failed to PUT /api/v1/config");
        assert!(
            resp.status().is_success(),
            "masked PUT round {round} returned {}",
            resp.status()
        );
    }

    // The real key must have survived: the connectivity check reaches the mock
    // only when the Authorization header still carries the original key.
    let resp = client
        .post(format!("{base}/api/v1/llm/providers/openai-0/test"))
        .body("")
        .send()
        .await
        .expect("failed to POST /test");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("test body is not JSON");
    assert_eq!(
        body["success"], true,
        "stored key must survive the masked round trip, got {:?}",
        body
    );

    // "Leave blank = unchanged": a PUT with empty keys must also preserve.
    let mut blank = config.clone();
    blank["llm"]["openaiApiKey"] = serde_json::Value::String(String::new());
    if let Some(providers) = blank["llm"]["providers"].as_array_mut() {
        for p in providers {
            p["apiKey"] = serde_json::Value::String(String::new());
        }
    }
    let resp = client
        .put(format!("{base}/api/v1/config"))
        .json(&blank)
        .send()
        .await
        .expect("failed to PUT /api/v1/config (blank)");
    assert!(resp.status().is_success(), "blank-key PUT returned {}", resp.status());
    let resp = client
        .post(format!("{base}/api/v1/llm/providers/openai-0/test"))
        .body("")
        .send()
        .await
        .expect("failed to POST /test after blank PUT");
    let body: serde_json::Value = resp.json().await.expect("test body is not JSON");
    assert_eq!(
        body["success"], true,
        "blank-key PUT must keep the stored key, got {:?}",
        body
    );

    // GET /config still masks after the round trips — never blank, never the secret.
    let resp = client
        .get(format!("{base}/api/v1/config"))
        .send()
        .await
        .expect("failed to GET /api/v1/config (after)");
    let config: serde_json::Value = resp.json().await.expect("GET /config body is not JSON");
    assert_eq!(config["llm"]["openaiApiKey"], "***");
}
