use std::time::{Duration, Instant};

use super::{bootstrap_authed_client, find_free_port, spawn_server_inner_with_env, wait_for_server, API_TOKEN};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ─── Self-upgrade API (U5) ───────────────────────────────────────

/// Build a small release tar.gz whose `bin/review-engine` is a harmless shell
/// script, so the upgrade pipeline (download → verify → extract → replace →
/// smoke) can run end-to-end against a temp install dir.
pub(super) fn build_fake_release_tar() -> Vec<u8> {
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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
