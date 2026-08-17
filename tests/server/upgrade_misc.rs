use std::time::{Duration, Instant};

use super::upgrade::build_fake_release_tar;
use super::{
    bin_path, bootstrap_authed_client, find_free_port, spawn_server_inner_with_env, spawn_server_with_bin,
    spawn_server_with_token, wait_for_server, API_TOKEN,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

        let client = bootstrap_authed_client(port, API_TOKEN).await;
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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

    let client = bootstrap_authed_client(port, API_TOKEN).await;
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
