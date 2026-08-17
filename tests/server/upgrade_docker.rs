use std::process::Command;
use std::time::{Duration, Instant};

use super::upgrade::build_fake_release_tar;
use super::{
    bin_path, bootstrap_authed_client, find_free_port, spawn_server_inner_with_env, wait_for_server, UpgradeChildGuard,
    API_TOKEN,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a small `frontend-dist.tar.gz` with `index.html` at the root (flat
/// layout, matching how the static file server serves `{frontend_dir}/`).
fn build_fake_frontend_dist_tar() -> Vec<u8> {
    const INDEX_HTML: &str = "<!doctype html><html><body>fixture dist</body></html>\n";
    let mut out = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path("index.html").expect("set tar path");
        header.set_size(INDEX_HTML.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append(&header, INDEX_HTML.as_bytes())
            .expect("append tar entry");
        let encoder = builder.into_inner().expect("into_inner");
        encoder.finish().expect("finish gzip");
    }
    out
}

/// `POST /upgrade` inside a container (Docker install method) starts the
/// in-container auto upgrade: the binary and the frontend dist are replaced on
/// the writable volumes, the job lands on `done` with the "容器即将自动重启"
/// message, and the process stays alive because `REVIEW_UPGRADE_EXIT_AFTER=0`
/// disables the restart exit (the test seam).
#[tokio::test]
async fn upgrade_docker_auto_upgrades_binary_and_frontend_dist() {
    let mock = MockServer::start().await;
    let api_base = mock.uri();
    let spec = review_engine::upgrade::platform::current_asset_spec().expect("test platform has an asset spec");
    let asset_name = spec.asset_name("review-engine");
    let checksum_name = spec.checksum_name("review-engine");
    let tar_bytes = build_fake_release_tar();
    let sha = review_engine::upgrade::verify::data_sha256_hex(&tar_bytes);
    let checksum_text = format!("{sha}  {asset_name}");
    let dist_bytes = build_fake_frontend_dist_tar();
    let dist_sha = review_engine::upgrade::verify::data_sha256_hex(&dist_bytes);
    let dist_checksum_text = format!("{dist_sha}  frontend-dist.tar.gz");

    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                {"name": asset_name, "browser_download_url": format!("{api_base}/asset"), "size": tar_bytes.len()},
                {"name": checksum_name, "browser_download_url": format!("{api_base}/checksum"), "size": checksum_text.len()},
                {"name": "frontend-dist.tar.gz", "browser_download_url": format!("{api_base}/dist"), "size": dist_bytes.len()},
                {"name": "frontend-dist.sha256", "browser_download_url": format!("{api_base}/dist.sha256"), "size": dist_checksum_text.len()}
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
    Mock::given(method("GET"))
        .and(path("/dist"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(dist_bytes.clone()))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/dist.sha256"))
        .respond_with(ResponseTemplate::new(200).set_body_string(dist_checksum_text))
        .mount(&mock)
        .await;

    let install_dir = tempfile::tempdir().expect("temp install dir");
    let install_dir_str = install_dir.path().to_str().expect("utf8 install dir").to_string();
    let frontend_dir = tempfile::tempdir().expect("temp frontend dir");
    let frontend_dir_str = frontend_dir.path().to_str().expect("utf8 frontend dir").to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &api_base),
            ("REVIEW_UPGRADE_METHOD", "docker"),
            ("REVIEW_UPGRADE_INSTALL_DIR", &install_dir_str),
            ("REVIEW_UPGRADE_FRONTEND_DIR", &frontend_dir_str),
            ("REVIEW_UPGRADE_EXIT_AFTER", "0"),
        ],
    );
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);

    let resp = client.post(&base).send().await.expect("POST upgrade");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "docker POST must be 202, got {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("202 body is JSON");
    assert_eq!(body["status"], "started");
    assert_eq!(body["targetVersion"], "9.9.9");

    let deadline = Instant::now() + Duration::from_secs(15);
    let status_url = format!("{base}/status");
    let final_status = loop {
        let status: serde_json::Value = client
            .get(&status_url)
            .send()
            .await
            .expect("GET status")
            .json()
            .await
            .expect("status is JSON");
        if status["state"] == "done" {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "docker upgrade did not finish, last status: {status}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!(final_status["message"], "升级完成，容器即将自动重启");
    assert_eq!(final_status["targetVersion"], "9.9.9");

    // Binary and frontend dist both landed on the writable volumes.
    assert!(
        install_dir.path().join("review-engine").exists(),
        "replaced binary must exist in install dir"
    );
    assert!(
        frontend_dir.path().join("index.html").exists(),
        "frontend dist must be replaced with index.html"
    );

    // The server process is still alive (exit disabled by the env seam).
    let status: serde_json::Value = client
        .get(&status_url)
        .send()
        .await
        .expect("server must still be alive")
        .json()
        .await
        .expect("status is JSON");
    assert_eq!(status["state"], "done");
}

/// A release without `frontend-dist.tar.gz` must still upgrade the binary
/// (graceful degrade): the job lands on `done`, the binary is replaced, and
/// the frontend dir is left untouched.
#[tokio::test]
async fn upgrade_docker_degrades_to_binary_only_without_dist_asset() {
    let mock = MockServer::start().await;
    let api_base = mock.uri();
    let spec = review_engine::upgrade::platform::current_asset_spec().expect("test platform has an asset spec");
    let asset_name = spec.asset_name("review-engine");
    let checksum_name = spec.checksum_name("review-engine");
    let tar_bytes = build_fake_release_tar();
    let sha = review_engine::upgrade::verify::data_sha256_hex(&tar_bytes);
    let checksum_text = format!("{sha}  {asset_name}");

    // No frontend-dist.tar.gz in the release assets (older release).
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

    let install_dir = tempfile::tempdir().expect("temp install dir");
    let install_dir_str = install_dir.path().to_str().expect("utf8 install dir").to_string();
    let frontend_dir = tempfile::tempdir().expect("temp frontend dir");
    let frontend_dir_str = frontend_dir.path().to_str().expect("utf8 frontend dir").to_string();

    let port = find_free_port();
    let _guard = spawn_server_inner_with_env(
        port,
        None,
        &[
            ("REVIEW_UPGRADE_API_BASE", &api_base),
            ("REVIEW_UPGRADE_METHOD", "docker"),
            ("REVIEW_UPGRADE_INSTALL_DIR", &install_dir_str),
            ("REVIEW_UPGRADE_FRONTEND_DIR", &frontend_dir_str),
            ("REVIEW_UPGRADE_EXIT_AFTER", "0"),
        ],
    );
    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let base = format!("http://127.0.0.1:{}/api/v1/system/upgrade", port);

    let resp = client.post(&base).send().await.expect("POST upgrade");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED, "POST must be 202");

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
            "binary-only docker upgrade did not finish, last status: {status}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    assert!(
        install_dir.path().join("review-engine").exists(),
        "binary must still be upgraded without a dist asset"
    );
    assert!(
        !frontend_dir.path().join("index.html").exists(),
        "frontend dir must be left untouched when the release has no dist asset"
    );
}

/// End-to-end restart trigger: with the exit enabled (no
/// `REVIEW_UPGRADE_EXIT_AFTER` override), the server process exits 0 after a
/// successful container upgrade, so the compose `restart: unless-stopped`
/// policy would pull the container back up with the new files.
#[tokio::test]
async fn upgrade_docker_exits_zero_after_successful_upgrade() {
    let mock = MockServer::start().await;
    let api_base = mock.uri();
    let spec = review_engine::upgrade::platform::current_asset_spec().expect("test platform has an asset spec");
    let asset_name = spec.asset_name("review-engine");
    let checksum_name = spec.checksum_name("review-engine");
    let tar_bytes = build_fake_release_tar();
    let sha = review_engine::upgrade::verify::data_sha256_hex(&tar_bytes);
    let checksum_text = format!("{sha}  {asset_name}");
    let dist_bytes = build_fake_frontend_dist_tar();
    let dist_sha = review_engine::upgrade::verify::data_sha256_hex(&dist_bytes);
    let dist_checksum_text = format!("{dist_sha}  frontend-dist.tar.gz");

    Mock::given(method("GET"))
        .and(path("/repos/Liewzheng/ReviewEngine/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.9.9",
            "html_url": "https://github.com/Liewzheng/ReviewEngine/releases/tag/v9.9.9",
            "published_at": "2026-01-01T00:00:00Z",
            "assets": [
                {"name": asset_name, "browser_download_url": format!("{api_base}/asset"), "size": tar_bytes.len()},
                {"name": checksum_name, "browser_download_url": format!("{api_base}/checksum"), "size": checksum_text.len()},
                {"name": "frontend-dist.tar.gz", "browser_download_url": format!("{api_base}/dist"), "size": dist_bytes.len()},
                {"name": "frontend-dist.sha256", "browser_download_url": format!("{api_base}/dist.sha256"), "size": dist_checksum_text.len()}
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
    Mock::given(method("GET"))
        .and(path("/dist"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(dist_bytes.clone()))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/dist.sha256"))
        .respond_with(ResponseTemplate::new(200).set_body_string(dist_checksum_text))
        .mount(&mock)
        .await;

    let install_dir = tempfile::tempdir().expect("temp install dir");
    let frontend_dir = tempfile::tempdir().expect("temp frontend dir");
    let temp_home = tempfile::tempdir().expect("temp home");
    let port = find_free_port();

    // Spawn manually (not via ServerGuard): the child is expected to exit on
    // its own, which ServerGuard's Drop would mask by killing it. Wrap it in
    // UpgradeChildGuard so a panicking test still kills the child instead of
    // leaking a server that would hold this port and break the next run.
    let mut child = UpgradeChildGuard {
        child: Some(
            Command::new(bin_path())
                .arg("serve")
                .arg("--bind")
                .arg("127.0.0.1")
                .arg("--port")
                .arg(port.to_string())
                .env("HOME", temp_home.path())
                .env("REVIEW_UPGRADE_API_BASE", &api_base)
                .env("REVIEW_UPGRADE_METHOD", "docker")
                .env("REVIEW_UPGRADE_INSTALL_DIR", install_dir.path())
                .env("REVIEW_UPGRADE_FRONTEND_DIR", frontend_dir.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("failed to spawn review-engine serve (docker exit test)"),
        ),
    };

    wait_for_server(port).await;

    let client = bootstrap_authed_client(port, API_TOKEN).await;
    let resp = client
        .post(format!("http://127.0.0.1:{port}/api/v1/system/upgrade"))
        .send()
        .await
        .expect("POST upgrade");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED, "POST must be 202");

    // Wait for the server to exit on its own (the restart trigger), max 15s.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if Instant::now() > deadline => panic!("server did not exit after a successful upgrade"),
            None => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    };
    assert!(status.success(), "server must exit 0 after upgrade, got {status:?}");

    // Both replacements landed before the exit.
    assert!(
        install_dir.path().join("review-engine").exists(),
        "replaced binary must exist in install dir"
    );
    assert!(
        frontend_dir.path().join("index.html").exists(),
        "frontend dist must be replaced with index.html"
    );
    // Reap the already-exited child so the guard's Drop has nothing to kill.
    let _ = child.child.take().map(|mut c| c.wait());
}
