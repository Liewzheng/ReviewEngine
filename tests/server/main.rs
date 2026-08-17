#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::TcpListener;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

mod auth;
mod frontend;
mod health;
mod llm;
mod llm_configs;
mod repo_scan;
mod tls;
mod upgrade;
mod upgrade_docker;
mod upgrade_misc;

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
    spawn_server_full(bin, port, token, extra_env, None)
}

/// Spawn `serve` with an optional working directory. The static frontend is
/// resolved relative to the process CWD (`./frontend/dist`), so tests that
/// exercise static-file serving point the server at a fixture tree.
fn spawn_server_full(
    bin: &str,
    port: u16,
    token: Option<&str>,
    extra_env: &[(&str, &str)],
    cwd: Option<&std::path::Path>,
) -> ServerGuard {
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
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
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

/// Drive a fresh no-token (loopback-bind, first-run) server through the
/// bootstrap flow: `PUT /api/v1/system/token` sets `token`, and the returned
/// client carries it as `Authorization: Bearer` for the rest of the test.
///
/// Every no-token integration test uses this, so the whole suite exercises the
/// new first-run bootstrap path instead of relying on the old "token-less
/// loopback API is open" behavior.
async fn bootstrap_authed_client(port: u16, token: &str) -> reqwest::Client {
    use reqwest::header::{HeaderValue, AUTHORIZATION};
    let resp = reqwest::Client::new()
        .put(format!("http://127.0.0.1:{}/api/v1/system/token", port))
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await
        .expect("bootstrap PUT /api/v1/system/token");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "first-run bootstrap (loopback) must accept the initial token, got {}",
        resp.status()
    );
    reqwest::Client::builder()
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
            );
            h
        })
        .build()
        .expect("failed to build authed client")
}

const API_TOKEN: &str = "test-token-123";
