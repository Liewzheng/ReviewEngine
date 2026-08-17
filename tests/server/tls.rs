use std::process::Command;
use std::time::{Duration, Instant};

use super::{bin_path, find_free_port, wait_for_server, ServerGuard};

// ─── TLS (HTTPS) ──────────────────────────────────────────────────

/// Write a fresh self-signed PEM certificate chain + private key for
/// `localhost`/`127.0.0.1` via rcgen. The test client disables certificate
/// verification, so the SANs are cosmetic — rcgen just needs to emit a pair
/// that axum-server's `RustlsConfig::from_pem_file` will accept.
fn write_self_signed_cert(cert_path: &std::path::Path, key_path: &std::path::Path) {
    use rcgen::generate_simple_self_signed;
    let certified_key = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("failed to generate self-signed certificate");
    std::fs::write(cert_path, certified_key.cert.pem()).expect("failed to write TLS cert PEM");
    std::fs::write(key_path, certified_key.key_pair.serialize_pem()).expect("failed to write TLS key PEM");
}

/// Spawn `serve` with both plain HTTP (`--port`) and HTTPS (`--tls-port`,
/// `--tls-cert`, `--tls-key`) listeners.
fn spawn_server_with_tls(
    http_port: u16,
    tls_port: u16,
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> ServerGuard {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let mut cmd = Command::new(bin_path());
    cmd.arg("serve")
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(http_port.to_string())
        .arg("--tls-port")
        .arg(tls_port.to_string())
        .arg("--tls-cert")
        .arg(cert_path)
        .arg("--tls-key")
        .arg(key_path)
        .env("HOME", temp_dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("failed to spawn review-engine serve (TLS)");
    ServerGuard {
        child,
        _temp_dir: temp_dir,
    }
}

/// Poll `https://127.0.0.1:{port}/health` until it answers 200, using a
/// rustls client with certificate verification disabled (self-signed cert).
async fn wait_for_tls_server(port: u16) {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .expect("failed to build TLS probe client");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client
            .get(format!("https://127.0.0.1:{port}/health"))
            .timeout(Duration::from_millis(200))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => break,
            _ if Instant::now() > deadline => panic!("TLS server did not start within 10 seconds"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// With `--tls-cert`/`--tls-key`, `serve` must bring up HTTPS on `--tls-port`
/// while the plain HTTP listener keeps serving on `--port` (they coexist on
/// different ports). The HTTPS `/health` is verified over rustls with cert
/// validation disabled, since the test uses a fresh self-signed cert.
#[tokio::test]
async fn tls_https_serves_health_and_http_coexists() {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir for TLS certs");
    let cert_path = temp_dir.path().join("cert.pem");
    let key_path = temp_dir.path().join("key.pem");
    write_self_signed_cert(&cert_path, &key_path);

    let http_port = find_free_port();
    let tls_port = find_free_port();
    assert_ne!(http_port, tls_port, "find_free_port returned the same port twice");
    let _guard = spawn_server_with_tls(http_port, tls_port, &cert_path, &key_path);

    // Both listeners must come up: HTTPS on --tls-port, plain HTTP on --port.
    wait_for_server(http_port).await;
    wait_for_tls_server(tls_port).await;

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .expect("failed to build TLS test client");

    let resp = client
        .get(format!("https://127.0.0.1:{tls_port}/health"))
        .send()
        .await
        .expect("failed to GET https /health");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "TLS /health returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("TLS /health body is not JSON");
    assert_eq!(body["status"], "ok", "TLS /health body must carry status ok");

    // Plain HTTP keeps working on the separate port (HTTP + TLS coexist).
    let resp = reqwest::get(format!("http://127.0.0.1:{http_port}/health"))
        .await
        .expect("failed to GET http /health");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "HTTP /health returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("HTTP /health body is not JSON");
    assert_eq!(body["status"], "ok", "HTTP /health body must carry status ok");
}

/// clap contract: `--tls-cert` and `--tls-key` are a required pair — passing
/// only one must fail argument parsing (exit non-zero) and name the missing
/// flag, never silently start a plain HTTP server.
#[test]
fn serve_rejects_half_tls_pair() {
    let port = find_free_port();
    let temp_home = tempfile::tempdir().expect("failed to create temp home");
    let output = Command::new(bin_path())
        .arg("serve")
        .arg("--bind")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--tls-cert")
        .arg("/nonexistent/cert.pem")
        .env("HOME", temp_home.path())
        .output()
        .expect("failed to run review-engine serve");
    assert!(
        !output.status.success(),
        "serve with only --tls-cert must fail, got status {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--tls-key"),
        "clap error must name the missing --tls-key, got: {stderr}"
    );
}
