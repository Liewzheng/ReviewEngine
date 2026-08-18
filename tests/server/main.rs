#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

mod auth;
mod frontend;
mod health;
mod llm;
mod llm_configs;
mod repo_scan;
mod reviews;
mod tls;
mod upgrade;
mod upgrade_docker;
mod upgrade_misc;

fn bin_path() -> String {
    std::env::var("CARGO_BIN_EXE_review-engine").unwrap_or_else(|_| "target/debug/review-engine".to_string())
}

/// Monotonic per-process port allocator.
///
/// The previous implementation bound `127.0.0.1:0`, read the assigned port,
/// then dropped the listener — a TOCTOU race under `--test-threads > 1`: the
/// just-freed port could be handed to another concurrently-spawning test
/// before this test's server bound it, so the loser's `serve` exited with
/// "Address already in use" and `wait_for_server` timed out.
///
/// Instead, each caller is handed a strictly unique port from an atomic
/// counter, so no two tests in this process can ever collide. Two more
/// properties make the ports safe to use directly:
///
/// - The range 21000..=28999 sits below both macOS (49152) and Linux (32768)
///   ephemeral ranges, so the kernel never hands these ports to outbound
///   sockets or other processes.
/// - We deliberately do NOT "check" a port by binding it first: on macOS,
///   bind-then-close followed by an immediate rebind from another thread or
///   process can fail with `EADDRINUSE` even when the port is free, which was
///   a second flake source. The atomic counter makes the check unnecessary.
fn find_free_port() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT_PORT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    21000 + (n % 8000) as u16
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

/// Guard for a manually-spawned `serve` child that is expected to exit on its
/// own (the docker-upgrade "restart trigger" test). Unlike [`ServerGuard`], it
/// must NOT kill the child during the happy path — the test polls
/// [`UpgradeChildGuard::try_wait`] until the child exits naturally. But if the
/// test panics before that (e.g. bootstrap fails), Drop kills the child so a
/// failed run never leaks a server that would hold its port and break the next
/// run's allocation.
struct UpgradeChildGuard {
    child: Option<Child>,
}

impl UpgradeChildGuard {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match &mut self.child {
            Some(c) => c.try_wait(),
            None => Ok(None),
        }
    }
}

impl Drop for UpgradeChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
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
