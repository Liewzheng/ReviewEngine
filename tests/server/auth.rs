use std::process::Command;

use super::{
    bin_path, find_free_port, spawn_server, spawn_server_inner_with_env, spawn_server_with_token, wait_for_server,
    ServerGuard, API_TOKEN,
};

/// Spawn `serve` on a NON-loopback bind (`0.0.0.0`) with no API token but a
/// one-time bootstrap key — the first-run Docker scenario. Every `/api/v1`
/// endpoint returns 401 until the initial token is set with `X-Bootstrap-Key`.
fn spawn_server_non_loopback_bootstrap(port: u16, bootstrap_key: &str) -> ServerGuard {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut cmd = Command::new(bin_path());
    cmd.arg("serve")
        .arg("--bind")
        .arg("0.0.0.0")
        .arg("--port")
        .arg(port.to_string())
        .arg("--bootstrap-key")
        .arg(bootstrap_key)
        .env("HOME", temp_dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("failed to spawn review-engine serve");
    ServerGuard {
        child,
        _temp_dir: temp_dir,
    }
}

// ─── API Auth (P1: type-mismatch fix) ─────────────────────────────
//
// Regression tests for the auth middleware. `api::routes` stores the shared
// auth config in request extensions as `Arc<AuthConfig>`; the middleware must
// read it back with the same type. When it read plain `AuthConfig` it always
// got `None` and silently allowed every request, so a token-less server
// exposed /api/v1 to the world. These tests spawn real servers both with and
// without `REVIEW_API_TOKEN` and assert the gate on /api/v1/system/version.

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

/// First-run bootstrap (loopback bind, no token): every `/api/v1` endpoint
/// returns `401 {"code":"auth_required"}` except the bootstrap endpoints, so
/// the frontend can detect "no token configured" and walk the user through
/// setting the initial token. Once set, the new token is enforced immediately.
#[tokio::test]
async fn api_bootstrap_loopback_first_run_sets_token_and_locks_api() {
    let port = find_free_port();
    let _guard = spawn_server(port);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}/api/v1");

    // 1. No token yet → ordinary endpoints return 401 + the bootstrap signal.
    let resp = get_version(port, None, None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json().await.expect("401 body is JSON");
    assert_eq!(body, serde_json::json!({"code": "auth_required"}));

    // 2. The status probe is open and reports bootstrap-needed.
    let resp = client.get(format!("{base}/system/auth-status")).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("auth-status body is JSON");
    assert_eq!(body["configured"], false);
    assert_eq!(body["bootstrap"], true);
    assert_eq!(
        body["bootstrapKeyRequired"], false,
        "loopback bind needs no bootstrap key"
    );

    // 3. Loopback bootstrap sets the initial token (no key required).
    let resp = client
        .put(format!("{base}/system/token"))
        .json(&serde_json::json!({"token": API_TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // 4. The new token is now enforced: correct one passes, missing is rejected
    //    with the regular unauthorized error (no longer bootstrap mode).
    let resp = get_version(port, Some(&format!("Bearer {API_TOKEN}")), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("version body is JSON");
    assert!(body["version"].is_string());

    let resp = get_version(port, None, None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json().await.expect("401 body is JSON");
    assert_eq!(body, serde_json::json!({"error": "unauthorized"}));

    // 5. auth-status now reports configured.
    let resp = client.get(format!("{base}/system/auth-status")).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.expect("auth-status body is JSON");
    assert_eq!(body["configured"], true);
    assert_eq!(body["bootstrap"], false);
}

/// First-run bootstrap on a NON-loopback bind (`0.0.0.0` — the Docker case):
/// the server starts only with a one-time bootstrap key, and `PUT
/// /api/v1/system/token` accepts that key to set the initial token. Without
/// the key the endpoint reports `401 {"code":"bootstrap_key_required"}`.
#[tokio::test]
async fn api_bootstrap_non_loopback_requires_bootstrap_key() {
    let port = find_free_port();
    let bootstrap_key = "one-time-bootstrap-key-123";
    let _guard = spawn_server_non_loopback_bootstrap(port, bootstrap_key);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}/api/v1");

    // Without the key → 401 bootstrap_key_required.
    let resp = client
        .put(format!("{base}/system/token"))
        .json(&serde_json::json!({"token": API_TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json().await.expect("401 body is JSON");
    assert_eq!(body, serde_json::json!({"code": "bootstrap_key_required"}));

    // Ordinary endpoints are locked with the generic bootstrap signal.
    let resp = get_version(port, None, None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json().await.expect("401 body is JSON");
    assert_eq!(body, serde_json::json!({"code": "auth_required"}));

    // With the key → the initial token is accepted.
    let resp = client
        .put(format!("{base}/system/token"))
        .header("X-Bootstrap-Key", bootstrap_key)
        .json(&serde_json::json!({"token": API_TOKEN}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // The new token is now enforced for ordinary endpoints; the bootstrap key
    // does NOT unlock them (security model preserved).
    let resp = get_version(port, Some(&format!("Bearer {API_TOKEN}")), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = get_version(port, None, None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let resp = client
        .get(format!("{base}/system/version"))
        .header("X-Bootstrap-Key", bootstrap_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // The bootstrap key stays valid as a ROTATION credential — the deadlock
    // rescue: even when the current token is lost/invalid, X-Bootstrap-Key
    // rotates to a fresh token (previously it went inert, locking operators out).
    let resp = client
        .put(format!("{base}/system/token"))
        .header("X-Bootstrap-Key", bootstrap_key)
        .json(&serde_json::json!({"token": "second-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "bootstrap key must remain a valid rotation credential once a token is configured"
    );

    // The rotated token is enforced and the bootstrap key still does not open
    // ordinary endpoints.
    let resp = get_version(port, Some("Bearer second-token"), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = get_version(port, None, None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// Token rotation: once a token is configured, `PUT /api/v1/system/token`
/// requires the CURRENT (old) token; a valid rotation makes the new token
/// effective immediately and the old one stops working.
#[tokio::test]
async fn api_token_rotation_requires_old_token() {
    let port = find_free_port();
    let _guard = spawn_server_with_token(port, API_TOKEN);
    wait_for_server(port).await;

    let base = format!("http://127.0.0.1:{port}/api/v1");

    // Rotate without the old token → 401.
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .json(&serde_json::json!({"token": "rotated-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = resp.json().await.expect("401 body is JSON");
    assert_eq!(body, serde_json::json!({"error": "unauthorized"}));

    // Rotate with the old token → 200, and the new token is enforced.
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .header("Authorization", format!("Bearer {API_TOKEN}"))
        .json(&serde_json::json!({"token": "rotated-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let resp = get_version(port, Some("Bearer rotated-token"), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let resp = get_version(port, Some(&format!("Bearer {API_TOKEN}")), None).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "old token must be rejected"
    );
}

/// Deadlock rescue via the bootstrap key (end-to-end): with a token configured
/// and the browser holding a STALE credential, `PUT /api/v1/system/token` with
/// `X-Bootstrap-Key` still rotates to a fresh token. The bootstrap key does NOT
/// unlock ordinary endpoints once a token is configured.
#[tokio::test]
async fn api_token_rotation_rescue_via_bootstrap_key() {
    let port = find_free_port();
    let bootstrap_key = "rescue-bootstrap-key-456";
    // Loopback bind + env token + env bootstrap key — the deployed default.
    let _guard = spawn_server_inner_with_env(port, Some(API_TOKEN), &[("REVIEW_BOOTSTRAP_KEY", bootstrap_key)]);
    wait_for_server(port).await;

    let base = format!("http://127.0.0.1:{port}/api/v1");

    // The browser's stale token is rejected on ordinary endpoints.
    let resp = get_version(port, Some("Bearer stale-browser-token"), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Rotating with the stale token fails — the deadlock the fix removes.
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .header("Authorization", "Bearer stale-browser-token")
        .json(&serde_json::json!({"token": "rescued-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Self-rescue: X-Bootstrap-Key authenticates the rotation regardless of
    // the (stale) current token.
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .header("X-Bootstrap-Key", bootstrap_key)
        .json(&serde_json::json!({"token": "rescued-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "bootstrap key must rotate a configured token (deadlock rescue)"
    );

    // The rescued token is now enforced…
    let resp = get_version(port, Some("Bearer rescued-token"), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    // …and the bootstrap key still does not open ordinary endpoints.
    let resp = reqwest::Client::new()
        .get(format!("{base}/system/version"))
        .header("X-Bootstrap-Key", bootstrap_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A wrong bootstrap key never rotates.
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .header("X-Bootstrap-Key", "wrong-key")
        .json(&serde_json::json!({"token": "should-not-stick"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// Deadlock rescue via env precedence (end-to-end): the env/CLI token
/// (`REVIEW_API_TOKEN` / `--api-token`) stays a valid rotation credential even
/// after a runtime rotation swapped the effective token — the operator's env
/// config always wins, so a rotation is never unrecoverable.
#[tokio::test]
async fn api_token_rotation_env_token_remains_valid_after_runtime_rotation() {
    let port = find_free_port();
    let _guard = spawn_server_with_token(port, API_TOKEN);
    wait_for_server(port).await;

    let base = format!("http://127.0.0.1:{port}/api/v1");

    // Rotate to a new effective token using the env token (normal path).
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .header("Authorization", format!("Bearer {API_TOKEN}"))
        .json(&serde_json::json!({"token": "rotated-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // The env token is no longer the effective token for ordinary endpoints…
    let resp = get_version(port, Some(&format!("Bearer {API_TOKEN}")), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // …but it still authenticates rotation (env precedence override).
    let resp = reqwest::Client::new()
        .put(format!("{base}/system/token"))
        .header("Authorization", format!("Bearer {API_TOKEN}"))
        .json(&serde_json::json!({"token": "env-rescued-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // The env-rescued token is now effective.
    let resp = get_version(port, Some("Bearer env-rescued-token"), None).await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// Auth enabled: the SSE log stream is reachable via `?token=` because
/// `EventSource` cannot send an `Authorization` header. A missing or wrong
/// query token is rejected with 401, and the query token must NOT be honored
/// on non-SSE endpoints — a query token leaks into access logs and browser
/// history, so the capability is deliberately scoped to the SSE stream only.
#[tokio::test]
async fn api_auth_sse_logs_accepts_query_token() {
    let port = find_free_port();
    let _guard = spawn_server_with_token(port, API_TOKEN);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}/api/v1/logs");

    // Correct ?token= passes the auth gate and reaches the SSE stream.
    let resp = client
        .get(format!("{base}?token={API_TOKEN}"))
        .send()
        .await
        .expect("failed to GET /api/v1/logs?token=…");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "SSE logs with correct query token returned {}",
        resp.status()
    );
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Drop without draining: the SSE stream never ends on its own.
    drop(resp);
    assert!(
        content_type.contains("text/event-stream"),
        "expected an SSE stream, got content-type {content_type}"
    );

    // The same mechanism covers the other SSE stream, /api/v1/events.
    let resp = client
        .get(format!("http://127.0.0.1:{port}/api/v1/events?token={API_TOKEN}"))
        .send()
        .await
        .expect("failed to GET /api/v1/events?token=…");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "SSE events with correct query token returned {}",
        resp.status()
    );
    drop(resp);

    // Wrong query token → 401 JSON.
    let resp = client
        .get(format!("{base}?token=wrong-token"))
        .send()
        .await
        .expect("failed to GET /api/v1/logs?token=wrong");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "wrong query token must be rejected"
    );
    let body: serde_json::Value = resp.json().await.expect("401 body must be JSON");
    assert_eq!(body, serde_json::json!({"error": "unauthorized"}));

    // No token → 401 JSON.
    let resp = client
        .get(&base)
        .send()
        .await
        .expect("failed to GET /api/v1/logs without token");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "missing query token must be rejected"
    );
    let body: serde_json::Value = resp.json().await.expect("401 body must be JSON");
    assert_eq!(body, serde_json::json!({"error": "unauthorized"}));

    // Query token must not authenticate non-SSE endpoints.
    let resp = client
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/system/version?token={API_TOKEN}"
        ))
        .send()
        .await
        .expect("failed to GET /api/v1/system/version?token=…");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "query token must not authenticate non-SSE endpoints"
    );
}
